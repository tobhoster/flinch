//! flinch-web's HTTP tests: each request goes through the whole app (routing,
//! the token check, the response headers) as `axum::serve` would run it.

use super::*;
use axum::body::Body;
use axum::http::Request;
use rstest::rstest;
use std::fs;

pub(crate) const TOKEN: &str = "s3cret";

pub(crate) fn state(tmp: &Path, token: Option<&str>) -> AppState {
    let dir = tmp.join("state");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("status.json"), r#"{"scanned":4,"kept":3,"dry_run":true}"#).unwrap();
    fs::write(dir.join("items.json"), r#"[{"id":"radarr-1","title":"A Movie","kind":"movie","size_bytes":1000000000,"decision":"keep","reason":"guard","delete_probability":0.0,"protected":true}]"#).unwrap();
    fs::write(dir.join("history.json"), r#"[{"ran_at_unix":1,"scanned":4,"delete_candidates":0,"reclaimed_bytes":0,"protections_added":0}]"#).unwrap();
    let items = Arc::new(snapshot::Snapshot::new(dir.join("items.json")));
    AppState { dir: Arc::from(dir), web: Arc::from(tmp.join("web")), token: token.map(Arc::from), items }
}

pub(crate) fn get(path: &str, authorization: Option<&str>) -> Request<Body> {
    let request = Request::get(path);
    let request = match authorization {
        Some(value) => request.header(header::AUTHORIZATION, value),
        None => request,
    };
    request.body(Body::empty()).unwrap()
}

fn put_settings(body: String) -> Request<Body> {
    Request::put("/api/settings")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::from(body))
        .unwrap()
}

/// One request through the whole app: routing, the token check and the
/// response headers, exactly as `axum::serve` would run it.
pub(crate) async fn send(st: &AppState, request: Request<Body>) -> Response {
    call(app(st.clone()), request).await
}

async fn call<S>(mut service: S, request: Request<Body>) -> Response
where
    S: axum::ServiceExt<Request<Body>, Response = Response, Error = std::convert::Infallible>,
{
    match service.call(request).await {
        Ok(response) => response,
        Err(never) => match never {},
    }
}

/// A fresh directory per test case: rstest cases run in parallel.
pub(crate) fn scratch(label: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("fw-{}-{label}-{n}", std::process::id()))
}

#[tokio::test]
async fn the_spa_shell_is_served_at_the_root() {
    let tmp = std::env::temp_dir().join(format!("fw-{}-{}", std::process::id(), "shell"));
    fs::create_dir_all(tmp.join("web")).unwrap();
    fs::write(tmp.join("web/index.html"), "<div id=\"root\"></div><script src=\"/assets/app.js\"></script>").unwrap();
    let st = state(&tmp, Some(TOKEN));
    let res = index(AxumState(st.clone())).await;
    assert_eq!(res.status(), StatusCode::OK);
    fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn api_endpoints_report_the_snapshot_not_a_broken_page() {
    let tmp = std::env::temp_dir().join(format!("fw-{}-{}", std::process::id(), "api"));
    fs::create_dir_all(tmp.join("web")).unwrap();
    fs::write(tmp.join("web/index.html"), "<div id=\"root\"></div>").unwrap();
    let st = state(&tmp, Some(TOKEN));
    let status = api_status(AxumState(st.clone())).await.into_response();
    assert_eq!(status.status(), StatusCode::OK);
    let history = api_history(AxumState(st.clone())).await.into_response();
    assert_eq!(history.status(), StatusCode::OK);
    fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn systemone_answers_from_the_snapshot_and_refuses_with_400() {
    let tmp = std::env::temp_dir().join(format!("fw-{}-{}", std::process::id(), "systemone"));
    let st = state(&tmp, Some(TOKEN));
    let ask = r#"{"state": "radarr-1", "questions": {"decision": {"type": "choice", "criteria": ["keep", "delete"]}}}"#;
    let answered = api_systemone(AxumState(st.clone()), ask.to_string()).await;
    assert_eq!(answered.status(), StatusCode::OK);
    let malformed = api_systemone(AxumState(st.clone()), "{".to_string()).await;
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    // radarr-1 has no forecast in the fixture: a 400, not a made-up number.
    let unscored = r#"{"state": "radarr-1", "questions": {"safe": {"type": "noul"}}}"#;
    let refused = api_systemone(AxumState(st.clone()), unscored.to_string()).await;
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    fs::remove_dir_all(&tmp).ok();
}

pub(crate) async fn body_of(res: Response) -> String {
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

const PLEX: &str = "http://plex.home:32400";

fn save_plex(st: &AppState, url: &str, token: &str) {
    let saved = RuntimeSettings { plex_url: url.to_string(), plex_token: token.to_string(), ..RuntimeSettings::default() };
    write_settings(&st.dir.join("settings.json"), &saved).unwrap();
}

#[tokio::test]
async fn the_plex_token_never_reaches_the_browser_and_a_blank_one_keeps_it() {
    let tmp = std::env::temp_dir().join(format!("fw-{}-{}", std::process::id(), "token"));
    let st = state(&tmp, Some(TOKEN));
    save_plex(&st, PLEX, "plex-s3cret-token");

    let shown = body_of(send(&st, get("/api/settings", Some(&format!("Bearer {TOKEN}")))).await).await;
    assert!(!shown.contains("plex-s3cret-token"), "token sent to the browser: {shown}");
    let view: serde_json::Value = serde_json::from_str(&shown).unwrap();
    assert_eq!(view["plex_token_set"], true);
    // f32 settings print as typed, not widened to 0.800000011920929.
    assert_eq!(view["capacity_ceiling"].to_string(), "0.8");

    // The page saves back what it was shown, with one change.
    let mut edited = view.clone();
    edited["score_floor"] = serde_json::json!(0.9);
    let res = send(&st, put_settings(edited.to_string())).await;
    assert_eq!(res.status(), StatusCode::OK);
    let stored = read_settings(&st.dir.join("settings.json")).unwrap();
    assert_eq!(stored.plex_token, "plex-s3cret-token");
    assert!((stored.score_floor - 0.9).abs() < 1e-6);
    fs::remove_dir_all(&tmp).ok();
}

/// `(url, token)` saved, then PUT; `Some` is what must be stored after,
/// `None` a 400 that leaves the file as it was.
#[rstest]
#[case::url_unchanged_blank_token((PLEX, "old"), (PLEX, ""), Some((PLEX, "old")))]
#[case::url_unchanged_padded((PLEX, "old"), (" http://plex.home:32400 ", " "), Some((PLEX, "old")))]
#[case::url_unchanged_new_token((PLEX, "old"), (PLEX, " new "), Some((PLEX, "new")))]
#[case::url_changed_blank_token((PLEX, "old"), ("http://evil.example:32400", ""), None)]
#[case::url_changed_with_its_token((PLEX, "old"), ("http://plex2.home:32400", "new"), Some(("http://plex2.home:32400", "new")))]
#[case::first_url_without_a_token(("", ""), (PLEX, ""), None)]
#[case::url_cleared((PLEX, "old"), ("", ""), Some(("", "")))]
#[case::url_cleared_with_a_token((PLEX, "old"), ("  ", "stray"), Some(("", "")))]
#[tokio::test]
async fn the_plex_url_and_token_are_saved_as_one_credential(
    #[case] saved: (&str, &str),
    #[case] put: (&str, &str),
    #[case] stored: Option<(&str, &str)>,
) {
    let tmp = scratch("pair");
    let st = state(&tmp, Some(TOKEN));
    save_plex(&st, saved.0, saved.1);
    let before = fs::read(st.dir.join("settings.json")).unwrap();

    let body = serde_json::json!({ "plex_url": put.0, "plex_token": put.1 });
    let res = send(&st, put_settings(body.to_string())).await;
    match stored {
        Some((url, token)) => {
            assert_eq!(res.status(), StatusCode::OK);
            let after = read_settings(&st.dir.join("settings.json")).unwrap();
            assert_eq!((after.plex_url.as_str(), after.plex_token.as_str()), (url, token));
        }
        None => {
            assert_eq!(res.status(), StatusCode::BAD_REQUEST);
            let refusal: serde_json::Value = serde_json::from_str(&body_of(res).await).unwrap();
            assert!(refusal["error"].is_string());
            assert_eq!(fs::read(st.dir.join("settings.json")).unwrap(), before, "a refused save must not touch the file");
        }
    }
    fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn an_out_of_range_setting_is_refused_and_nothing_is_written() {
    let tmp = std::env::temp_dir().join(format!("fw-{}-{}", std::process::id(), "outofrange"));
    let st = state(&tmp, Some(TOKEN));
    save_plex(&st, PLEX, "old");
    let before = fs::read(st.dir.join("settings.json")).unwrap();
    let body = serde_json::json!({ "plex_url": PLEX, "capacity_ceiling": 0.7, "capacity_release": 0.9 });
    let res = send(&st, put_settings(body.to_string())).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let refusal: serde_json::Value = serde_json::from_str(&body_of(res).await).unwrap();
    assert!(refusal["error"].is_string());
    assert_eq!(fs::read(st.dir.join("settings.json")).unwrap(), before);
    fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn an_unreadable_settings_file_is_an_error_not_defaults() {
    let tmp = std::env::temp_dir().join(format!("fw-{}-{}", std::process::id(), "badsettings"));
    let st = state(&tmp, Some(TOKEN));
    fs::write(st.dir.join("settings.json"), "{ not json").unwrap();
    let res = api_settings_get(AxumState(st.clone())).await;
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    fs::remove_dir_all(&tmp).ok();
}
