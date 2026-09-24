//! flinch-web — the swarm's window for a homelab.
//!
//! Serves the React UI (built by `frontend/` at image build time) next to the
//! JSON API. It reads the state files the daemon publishes and writes only two:
//! `settings.json` from the Settings page and `run.now` to ask for a run. No
//! *arr keys, no database.

mod systemone;

use anyhow::{Context, Result};
use axum::{
    extract::{Path as AxumPath, State as AxumState},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use flinch_archive::daemon::{read_settings, write_settings, RuntimeSettings};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone)]
struct AppState {
    dir: Arc<PathBuf>,
    web: Arc<PathBuf>,
}

fn read_json(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|_| "null".to_string())
}

async fn api_status(AxumState(st): AxumState<AppState>) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/json")], read_json(&st.dir.join("status.json")))
}

async fn api_items(AxumState(st): AxumState<AppState>) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/json")], read_json(&st.dir.join("items.json")))
}

/// Settings as the browser sees them. The Plex token stays on the server;
/// `plex_token_set` says whether one is saved.
#[derive(serde::Serialize)]
struct SettingsView {
    #[serde(flatten)]
    settings: RuntimeSettings,
    plex_token_set: bool,
}

/// The settings the daemon runs with: `settings.json`, every missing field at
/// its default. An unreadable file is reported, never replaced by defaults:
/// the daemon keeps its last good settings then, which this cannot show.
async fn api_settings_get(AxumState(st): AxumState<AppState>) -> Response {
    match read_settings(&st.dir.join("settings.json")) {
        Ok(settings) => {
            let plex_token_set = !settings.plex_token.is_empty();
            let settings = RuntimeSettings { plex_token: String::new(), ..settings };
            axum::Json(SettingsView { settings, plex_token_set }).into_response()
        }
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

/// Persist operator settings. The daemon re-reads this file each cycle, so a
/// saved change takes effect on the next loop without a redeploy. The browser
/// never holds the saved Plex token, so a blank one keeps it.
async fn api_settings_put(AxumState(st): AxumState<AppState>, body: String) -> Response {
    let path = st.dir.join("settings.json");
    let mut settings: RuntimeSettings = match serde_json::from_str(&body) {
        Ok(settings) => settings,
        Err(error) => return (StatusCode::BAD_REQUEST, format!("invalid settings: {error}")).into_response(),
    };
    if settings.plex_token.is_empty() {
        if let Ok(saved) = read_settings(&path) {
            settings.plex_token = saved.plex_token;
        }
    }
    match write_settings(&path, &settings) {
        Ok(()) => (StatusCode::OK, "saved").into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, format!("write failed: {error}")).into_response(),
    }
}

async fn api_history(AxumState(st): AxumState<AppState>) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/json")], read_json(&st.dir.join("history.json")))
}

/// TypeSafe's System One API (`POST /v1/systemone`), answered from the
/// published snapshot. The body is parsed by hand, not by axum's `Json`, so
/// every refusal is a 400 with `{"error": …}` whatever the content type.
async fn api_systemone(AxumState(st): AxumState<AppState>, body: String) -> Response {
    let started = std::time::Instant::now();
    let request: flinch_archive::systemone::Request = match serde_json::from_str(&body) {
        Ok(request) => request,
        Err(error) => return refuse(StatusCode::BAD_REQUEST, &format!("malformed request: {error}")),
    };
    let items: Option<Vec<flinch_archive::ItemSnapshot>> = std::fs::read_to_string(st.dir.join("items.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    let Some(items) = items else {
        return refuse(StatusCode::SERVICE_UNAVAILABLE, "no library snapshot yet: the daemon has not published items.json");
    };
    match systemone::answer(&items, &request) {
        Ok(answers) => axum::Json(flinch_archive::systemone::Response {
            model: systemone::model_name(&read_json(&st.dir.join("status.json"))),
            answers,
            latency_ms: Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
        })
        .into_response(),
        Err(error) => refuse(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

fn refuse(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(serde_json::json!({ "error": message }))).into_response()
}

/// The brand marks: the original, and a light-on-dark variant for the dark UI.
/// Small and harmless, but still only ever these two static files.
async fn logo(AxumState(st): AxumState<AppState>) -> Response {
    brand_mark(&st, "logo.png")
}

async fn logo_dark(AxumState(st): AxumState<AppState>) -> Response {
    brand_mark(&st, "logo-dark.png")
}

fn brand_mark(st: &AppState, name: &str) -> Response {
    match std::fs::read(st.web.join(name)) {
        Ok(bytes) => ([(header::CONTENT_TYPE, "image/png")], bytes).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "no logo").into_response(),
    }
}

async fn healthz() -> &'static str {
    "ok"
}

/// Ask the daemon for an immediate scan.
///
/// Cross-container signalling is a file on the shared volume — no queue, no
/// broker, no auth surface. The daemon consumes and deletes it at the top of
/// its loop, so a burst of clicks collapses into one run.
async fn api_run(AxumState(st): AxumState<AppState>) -> Response {
    let path = st.dir.join("run.now");
    match std::fs::write(&path, b"1") {
        Ok(()) => (StatusCode::ACCEPTED, "queued").into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("cannot queue a run: {error}"),
        )
            .into_response(),
    }
}

/// The SPA shell for any non-API path; React owns the rest of the routing.
async fn index(AxumState(st): AxumState<AppState>) -> Response {
    let mut buf = String::new();
    std::fs::File::open(st.web.join("index.html"))
        .and_then(|mut f| f.read_to_string(&mut buf))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response())
        .map(|_| {
            (
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                buf,
            )
                .into_response()
        })
        .unwrap_or_else(|r: Response| r)
}

/// Hashed build assets, served verbatim from the mounted dist dir.
async fn assets(AxumState(st): AxumState<AppState>, AxumPath(path): AxumPath<String>) -> Response {
    let Some(name) = Path::new(&path).file_name().map(|n| n.to_os_string()) else {
        return (StatusCode::NOT_FOUND, "missing asset").into_response();
    };
    let candidate = st.web.join("assets").join(name);
    match std::fs::read(&candidate) {
        Ok(bytes) => {
            let content_type = if candidate.extension().map(|e| e == "css").unwrap_or(false) {
                "text/css; charset=utf-8"
            } else if candidate.extension().map(|e| e == "svg").unwrap_or(false) {
                "image/svg+xml"
            } else {
                "application/javascript; charset=utf-8"
            };
            ([(header::CONTENT_TYPE, content_type)], bytes).into_response()
        }
        Err(err) => {
            eprintln!("[assets] read {:?} failed: {err}", candidate);
            (StatusCode::NOT_FOUND, "missing asset").into_response()
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let port = std::env::var("FLINCH_WEB_PORT").unwrap_or_else(|_| "7911".to_string());
    let dir = std::env::var("FLINCH_STATE_DIR").unwrap_or_else(|_| "state".to_string());
    let web = std::env::var("FLINCH_WEB_DIR").unwrap_or_else(|_| "web".to_string());
    let app = Router::new()
        .route("/", get(index))
        .route("/api/status", get(api_status))
        .route("/api/items", get(api_items))
        .route("/api/history", get(api_history))
        .route("/api/run", axum::routing::post(api_run))
        .route(flinch_archive::systemone::PATH, axum::routing::post(api_systemone))
        .route("/api/settings", get(api_settings_get).put(api_settings_put))
        .route("/healthz", get(healthz))
        .route("/assets/{path}", get(assets))
        .route("/logo.png", get(logo))
        .route("/logo-dark.png", get(logo_dark))
        .fallback(get(index));
    let app = app.with_state(AppState { dir: Arc::from(PathBuf::from(dir)), web: Arc::from(PathBuf::from(web)) });
    let addr = format!("0.0.0.0:{port}");
    println!("flinch-web on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await.context("bind")?;
    axum::serve(listener, app).await.context("serve")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn state(tmp: &Path) -> AppState {
        let dir = tmp.join("state");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("status.json"), r#"{"scanned":4,"kept":3,"dry_run":true}"#).unwrap();
        fs::write(dir.join("items.json"), r#"[{"id":"radarr-1","title":"A Movie","kind":"movie","size_bytes":1000000000,"decision":"keep","reason":"guard","delete_probability":0.0,"protected":true}]"#).unwrap();
        fs::write(dir.join("history.json"), r#"[{"ran_at_unix":1,"scanned":4,"delete_candidates":0,"reclaimed_bytes":0,"protections_added":0}]"#).unwrap();
        AppState { dir: Arc::from(dir), web: Arc::from(tmp.join("web")) }
    }

    #[tokio::test]
    async fn the_spa_shell_is_served_at_the_root() {
        let tmp = std::env::temp_dir().join(format!("fw-{}-{}", std::process::id(), "shell"));
        fs::create_dir_all(tmp.join("web")).unwrap();
        fs::write(tmp.join("web/index.html"), "<div id=\"root\"></div><script src=\"/assets/app.js\"></script>").unwrap();
        let st = state(&tmp);
        let res = index(AxumState(st.clone())).await;
        assert_eq!(res.status(), StatusCode::OK);
        fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn api_endpoints_report_the_snapshot_not_a_broken_page() {
        let tmp = std::env::temp_dir().join(format!("fw-{}-{}", std::process::id(), "api"));
        fs::create_dir_all(tmp.join("web")).unwrap();
        fs::write(tmp.join("web/index.html"), "<div id=\"root\"></div>").unwrap();
        let st = state(&tmp);
        let status = api_status(AxumState(st.clone())).await.into_response();
        assert_eq!(status.status(), StatusCode::OK);
        let history = api_history(AxumState(st.clone())).await.into_response();
        assert_eq!(history.status(), StatusCode::OK);
        fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn systemone_answers_from_the_snapshot_and_refuses_with_400() {
        let tmp = std::env::temp_dir().join(format!("fw-{}-{}", std::process::id(), "systemone"));
        let st = state(&tmp);
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

    async fn body_of(res: Response) -> String {
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn the_plex_token_never_reaches_the_browser_and_a_blank_one_keeps_it() {
        let tmp = std::env::temp_dir().join(format!("fw-{}-{}", std::process::id(), "token"));
        let st = state(&tmp);
        let saved = RuntimeSettings { plex_token: "s3cret-token".to_string(), ..RuntimeSettings::default() };
        write_settings(&st.dir.join("settings.json"), &saved).unwrap();

        let shown = body_of(api_settings_get(AxumState(st.clone())).await).await;
        assert!(!shown.contains("s3cret-token"), "token sent to the browser: {shown}");
        let view: serde_json::Value = serde_json::from_str(&shown).unwrap();
        assert_eq!(view["plex_token_set"], true);
        // f32 settings print as typed, not widened to 0.800000011920929.
        assert_eq!(view["capacity_ceiling"].to_string(), "0.8");

        // The page saves back what it was shown, with one change.
        let mut edited = view.clone();
        edited["score_floor"] = serde_json::json!(0.9);
        let res = api_settings_put(AxumState(st.clone()), edited.to_string()).await;
        assert_eq!(res.status(), StatusCode::OK);
        let stored = read_settings(&st.dir.join("settings.json")).unwrap();
        assert_eq!(stored.plex_token, "s3cret-token");
        assert!((stored.score_floor - 0.9).abs() < 1e-6);
        fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn an_unreadable_settings_file_is_an_error_not_defaults() {
        let tmp = std::env::temp_dir().join(format!("fw-{}-{}", std::process::id(), "badsettings"));
        let st = state(&tmp);
        fs::write(st.dir.join("settings.json"), "{ not json").unwrap();
        let res = api_settings_get(AxumState(st.clone())).await;
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        fs::remove_dir_all(&tmp).ok();
    }
}