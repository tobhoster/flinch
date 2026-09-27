//! The login, the API key and the session cookie, each through the whole app.

use super::*;
use crate::tests::{body_of, get, request, scratch, send, state, state_with};
use axum::body::Body;
use rstest::rstest;
use std::cell::RefCell;
use std::path::Path;
use std::sync::LazyLock;

thread_local! {
    /// What `log` wrote on this thread: a `#[tokio::test]` runs its requests
    /// on its own thread, so each test reads only its own lines.
    pub(super) static LOGGED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// The login is drawn when the tests run, the way the server draws session
/// ids: a password written into the source is what code scanning reports as
/// a hard-coded credential, in a test or not.
static USERNAME: LazyLock<String> = LazyLock::new(|| format!("keeper-{}", &drawn()[..6]));
static PASSWORD: LazyLock<String> = LazyLock::new(|| format!("correct horse {}", &drawn()[..12]));
/// A password that is not the login's.
static WRONG: LazyLock<String> = LazyLock::new(|| drawn()[..12].to_string());

fn drawn() -> String {
    new_session_id().expect("the tests draw their login from /dev/urandom")
}
const KEY: &str = "s3cret";

/// A server with the login, and the API key when `key` is set.
fn login_state(tmp: &Path, key: Option<&str>) -> AppState {
    state_with(tmp, Auth::new(Some(USERNAME.as_str()), Some(PASSWORD.as_str()), key))
}

/// The UI's own header, which a login, a logout and a write with the cookie need.
const FROM_THE_UI: (&str, &str) = ("x-flinch-request", "1");

/// A login as the UI sends it, with `headers` besides.
fn log_in_as(username: &str, password: &str, headers: &[(&str, &str)]) -> Request<Body> {
    let body = serde_json::json!({ "username": username, "password": password }).to_string();
    request("POST", "/api/login", &[&[FROM_THE_UI], headers].concat(), body)
}

/// A logout as the UI sends it, with this `Cookie` header.
fn log_out(cookie: &str) -> Request<Body> {
    request("POST", "/api/logout", &[FROM_THE_UI, ("cookie", cookie)], Body::empty())
}

/// Logs in and returns the `Cookie` header that carries the new session.
async fn logged_in(st: &AppState) -> String {
    let res = send(st, log_in_as(&USERNAME, &PASSWORD, &[])).await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    session_of(&res).expect("a login sets the session cookie")
}

/// `flinch_session=<id>` from a response's `Set-Cookie`, if it has one.
fn session_of(res: &Response) -> Option<String> {
    let set = res.headers().get(header::SET_COOKIE)?.to_str().unwrap();
    Some(set.split(';').next().unwrap().to_string())
}

async fn json_of(res: Response) -> serde_json::Value {
    serde_json::from_str(&body_of(res).await).unwrap()
}

/// Every session's end, moved to `ends`.
fn end_sessions_at(st: &AppState, ends: Instant) {
    st.auth.sessions().values_mut().for_each(|end| *end = ends);
}

/// Ends the pause after failed logins, as if its time had run out.
fn end_the_pause(st: &AppState) {
    st.auth.throttle.lock().unwrap().until = Some(Instant::now());
}

fn challenges(res: &Response) -> Vec<&str> {
    res.headers().get_all(header::WWW_AUTHENTICATE).iter().map(|value| value.to_str().unwrap()).collect()
}

const DECISION: &str = r#"{"state": "radarr-1", "questions": {"decision": {"type": "choice", "criteria": ["keep", "delete"]}}}"#;

#[rstest]
#[case::no_header(Some(KEY), &[], StatusCode::UNAUTHORIZED)]
#[case::wrong_key(Some(KEY), &[("x-api-key", "s3cre7")], StatusCode::UNAUTHORIZED)]
#[case::key_prefix(Some(KEY), &[("x-api-key", "s3cre")], StatusCode::UNAUTHORIZED)]
#[case::wrong_bearer(Some(KEY), &[("authorization", "Bearer s3cre7")], StatusCode::UNAUTHORIZED)]
#[case::bearer_prefix(Some(KEY), &[("authorization", "Bearer s3cre")], StatusCode::UNAUTHORIZED)]
#[case::not_a_bearer(Some(KEY), &[("authorization", "Basic s3cret")], StatusCode::UNAUTHORIZED)]
// A wrong X-Api-Key is the answer, even beside the right bearer.
#[case::wrong_key_right_bearer(Some(KEY), &[("x-api-key", "nope"), ("authorization", "Bearer s3cret")], StatusCode::UNAUTHORIZED)]
#[case::right_key(Some(KEY), &[("x-api-key", KEY)], StatusCode::OK)]
#[case::right_bearer(Some(KEY), &[("authorization", "Bearer s3cret")], StatusCode::OK)]
// Nothing set on the server: closed, even to a caller who sends an empty key.
#[case::unconfigured(None, &[("authorization", "Bearer ")], StatusCode::UNAUTHORIZED)]
#[case::unconfigured_empty_key(None, &[("x-api-key", "")], StatusCode::UNAUTHORIZED)]
#[tokio::test]
async fn the_api_answers_only_the_configured_key(
    #[case] server: Option<&str>,
    #[case] headers: &[(&str, &str)],
    #[case] expected: StatusCode,
) {
    let tmp = scratch("auth");
    let st = state(&tmp, server);
    for path in ["/api/status", "/api/settings", "/api/nothing-here"] {
        let res = send(&st, get(path, headers)).await;
        let wanted = if path == "/api/nothing-here" && expected == StatusCode::OK { StatusCode::NOT_FOUND } else { expected };
        assert_eq!(res.status(), wanted, "{path}");
        assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store", "{path}");
        if expected == StatusCode::UNAUTHORIZED {
            assert_eq!(challenges(&res), CHALLENGES, "{path}");
            let body = json_of(res).await;
            assert_eq!(body["login_configured"], false, "{path}");
            assert!(body["error"].is_string());
        }
    }
    // Automations ask System One with the key, and never need the UI's header.
    let systemone = send(&st, request("POST", flinch_archive::systemone::PATH, headers, DECISION)).await;
    assert_eq!(systemone.status(), expected);
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn public_routes_stay_open_and_every_response_carries_the_security_headers() {
    let tmp = std::env::temp_dir().join(format!("fw-{}-public", std::process::id()));
    std::fs::create_dir_all(tmp.join("web")).unwrap();
    std::fs::write(tmp.join("web/index.html"), "<div id=\"root\"></div>").unwrap();
    let st = login_state(&tmp, Some(KEY));
    for (path, expected) in [
        ("/healthz", StatusCode::OK),
        ("/", StatusCode::OK),
        ("/items", StatusCode::OK),
        ("/api/session", StatusCode::OK),
        ("/api/login", StatusCode::METHOD_NOT_ALLOWED),
        ("/api/status", StatusCode::UNAUTHORIZED),
    ] {
        let res = send(&st, get(path, &[])).await;
        assert_eq!(res.status(), expected, "{path}");
        let headers = res.headers();
        assert_eq!(headers[header::CONTENT_SECURITY_POLICY], CONTENT_SECURITY_POLICY, "{path}");
        assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY", "{path}");
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff", "{path}");
        assert_eq!(headers[header::REFERRER_POLICY], "no-referrer", "{path}");
    }
    std::fs::remove_dir_all(&tmp).ok();
}

#[rstest]
#[case::unset(None, None)]
#[case::blank(Some("  \n"), None)]
#[case::trimmed(Some(" s3cret\n"), Some("s3cret"))]
fn a_blank_environment_value_counts_as_unset(#[case] raw: Option<&str>, #[case] value: Option<&str>) {
    assert_eq!(configured(raw).as_deref(), value);
}

#[rstest]
#[case::both(Some("keeper"), Some("pw"), true)]
#[case::padded(Some(" keeper\n"), Some(" pw "), true)]
#[case::no_password(Some("keeper"), None, false)]
#[case::blank_password(Some("keeper"), Some("  "), false)]
#[case::no_username(None, Some("pw"), false)]
#[case::neither(None, None, false)]
fn the_login_needs_both_halves(#[case] username: Option<&str>, #[case] password: Option<&str>, #[case] on: bool) {
    assert_eq!(Auth::new(username, password, None).login.is_some(), on);
}

/// `(X-Forwarded-Proto, Secure expected)`: only an HTTPS ingress gets
/// `Secure`, so plain `http://localhost` keeps its cookie.
#[rstest]
#[case::plain_http(None, false)]
#[case::https_ingress(Some("https"), true)]
#[case::https_ingress_uppercase(Some("HTTPS"), true)]
#[case::http_ingress(Some("http"), false)]
#[case::https_first_in_a_chain(Some("https, http"), true)]
#[tokio::test]
async fn logging_in_sets_a_session_cookie_that_opens_the_api(#[case] proto: Option<&str>, #[case] secure: bool) {
    let tmp = scratch("login");
    let st = login_state(&tmp, None);
    let forwarded: Vec<(&str, &str)> = proto.map(|proto| ("x-forwarded-proto", proto)).into_iter().collect();
    let res = send(&st, log_in_as(&USERNAME, &PASSWORD, &forwarded)).await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
    let set = res.headers()[header::SET_COOKIE].to_str().unwrap().to_string();
    let attributes: Vec<&str> = set.split("; ").collect();
    let id = attributes[0].strip_prefix("flinch_session=").expect("the flinch_session cookie");
    assert_eq!(id.len(), 64, "32 random bytes as hex");
    assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
    for wanted in ["Path=/", "HttpOnly", "SameSite=Strict", "Max-Age=2592000"] {
        assert!(attributes.contains(&wanted), "{set} lacks {wanted}");
    }
    assert_eq!(attributes.contains(&"Secure"), secure, "{set}");

    let cookie = session_of(&res).unwrap();
    let with_cookie = [("cookie", cookie.as_str())];
    let status = send(&st, get("/api/status", &with_cookie)).await;
    assert_eq!(status.status(), StatusCode::OK);
    assert!(body_of(status).await.contains("scanned"));
    assert_eq!(send(&st, get("/api/nothing-here", &with_cookie)).await.status(), StatusCode::NOT_FOUND);
    let session = json_of(send(&st, get("/api/session", &with_cookie)).await).await;
    assert_eq!(session, serde_json::json!({ "authenticated": true, "login_configured": true }));
    // Among other cookies, and on System One with the UI's header.
    let among = format!("theme=dark; {cookie}; other=1");
    assert_eq!(send(&st, get("/api/items", &[("cookie", &among)])).await.status(), StatusCode::OK);
    let asked = request("POST", flinch_archive::systemone::PATH, &[("cookie", &cookie), ("x-flinch-request", "1")], DECISION);
    assert_eq!(send(&st, asked).await.status(), StatusCode::OK);
    std::fs::remove_dir_all(&tmp).ok();
}

/// Every wrong login gets the same answer: which half was wrong never shows.
#[rstest]
#[case::wrong_password(&USERNAME, &PASSWORD[..PASSWORD.len() - 1])]
#[case::password_with_more(&USERNAME, &format!("{} staple", *PASSWORD))]
#[case::wrong_username("root", &PASSWORD)]
#[case::username_in_another_case(&USERNAME.to_uppercase(), &PASSWORD)]
#[case::both_wrong("root", "hunter2")]
#[case::swapped(&PASSWORD, &USERNAME)]
#[case::empty("", "")]
#[tokio::test]
async fn a_wrong_login_is_refused_without_saying_which_half(#[case] username: &str, #[case] password: &str) {
    let tmp = scratch("wrong");
    let st = login_state(&tmp, None);
    let res = send(&st, log_in_as(username, password, &[])).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(res.headers().get(header::SET_COOKIE).is_none());
    assert_eq!(json_of(res).await, serde_json::json!({ "error": LOGIN_REFUSED, "login_configured": true }));
    assert!(st.auth.sessions().is_empty());
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn a_login_that_is_not_json_is_a_400_and_no_failure() {
    let tmp = scratch("malformed");
    let st = login_state(&tmp, None);
    for body in ["{", "username=keeper&password=x", r#"{"username": 7}"#] {
        let res = send(&st, request("POST", "/api/login", &[FROM_THE_UI], body)).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{body}");
        assert!(json_of(res).await["error"].is_string());
    }
    // A form with a field missing is a wrong guess like any other.
    assert_eq!(send(&st, request("POST", "/api/login", &[FROM_THE_UI], "{}")).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(st.auth.throttle.lock().unwrap().failures, 1);
    std::fs::remove_dir_all(&tmp).ok();
}

/// A page on another site can post a login through the owner's browser (a
/// `text/plain` form, a `no-cors` fetch), but cannot add the UI's header. So
/// such a login is refused before it is checked: no answer tells a right
/// guess from a wrong one, and no failure is counted toward the pause.
#[rstest]
#[case::right_login_no_header(&PASSWORD, &[])]
#[case::wrong_login_no_header("hunter2", &[])]
#[case::right_login_other_value(&PASSWORD, &[("x-flinch-request", "0")])]
#[case::wrong_login_other_value("hunter2", &[("x-flinch-request", "true")])]
#[tokio::test]
async fn a_login_without_the_ui_header_is_refused_before_it_is_checked(#[case] password: &str, #[case] extra: &[(&str, &str)]) {
    let tmp = scratch("login-csrf");
    let st = login_state(&tmp, None);
    // What an enctype="text/plain" form sends: the JSON, then "=".
    let body = format!(r#"{{"username": "{}", "password": "{password}", "x": "="}}"#, *USERNAME);
    let headers = [&[("content-type", "text/plain")], extra].concat();
    for _ in 0..=THROTTLE_AFTER {
        let res = send(&st, request("POST", "/api/login", &headers, body.clone())).await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        assert!(res.headers().get(header::SET_COOKIE).is_none());
        assert!(json_of(res).await["error"].as_str().unwrap().contains("X-Flinch-Request: 1"));
    }
    assert!(st.auth.sessions().is_empty());
    assert_eq!(st.auth.throttle.lock().unwrap().failures, 0, "a forged login must not count toward the pause");
    // The owner can still log in straight away.
    assert_eq!(send(&st, log_in_as(&USERNAME, &PASSWORD, &[])).await.status(), StatusCode::NO_CONTENT);
    std::fs::remove_dir_all(&tmp).ok();
}

/// The login is open to anyone, so it reads only a few KiB: a large body is
/// refused before it is read, and counts as no failure.
#[tokio::test]
async fn a_login_body_over_the_limit_is_refused_unread() {
    let tmp = scratch("login-limit");
    let st = login_state(&tmp, None);
    let huge = serde_json::json!({ "username": *USERNAME, "password": "x".repeat(LOGIN_BODY_LIMIT) }).to_string();
    let res = send(&st, request("POST", "/api/login", &[FROM_THE_UI], huge)).await;
    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(st.auth.throttle.lock().unwrap().failures, 0);
    // A long password within the limit is still checked.
    let long = send(&st, log_in_as(&USERNAME, &"x".repeat(LOGIN_BODY_LIMIT / 2), &[])).await;
    assert_eq!(long.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(st.auth.throttle.lock().unwrap().failures, 1);
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn surrounding_whitespace_in_the_form_never_counts() {
    let tmp = scratch("padded");
    let st = state_with(&tmp, Auth::new(Some(format!(" {}\n", *USERNAME).as_str()), Some(format!("{}\n", *PASSWORD).as_str()), None));
    let res = send(&st, log_in_as(&format!(" {} ", *USERNAME), &format!(" {} ", *PASSWORD), &[])).await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn logging_out_ends_the_session_and_clears_the_cookie() {
    let tmp = scratch("logout");
    let st = login_state(&tmp, None);
    let cookie = logged_in(&st).await;
    let res = send(&st, log_out(&cookie)).await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(res.headers()[header::SET_COOKIE], "flinch_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0");
    // The old cookie, replayed by hand, is dead on the server too.
    let res = send(&st, get("/api/status", &[("cookie", &cookie)])).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(json_of(res).await["error"], "Your session has ended: log in again");
    // Logging out with no session only clears the cookie.
    assert_eq!(send(&st, request("POST", "/api/logout", &[FROM_THE_UI], Body::empty())).await.status(), StatusCode::NO_CONTENT);
    std::fs::remove_dir_all(&tmp).ok();
}

/// A page on another site can post a form to the logout, and the browser
/// would honour the cleared cookie; without the UI's header nothing changes.
#[rstest]
#[case::no_header(&[])]
#[case::other_value(&[("x-flinch-request", "yes")])]
#[tokio::test]
async fn a_logout_without_the_ui_header_changes_nothing(#[case] extra: &[(&str, &str)]) {
    let tmp = scratch("logout-csrf");
    let st = login_state(&tmp, None);
    let cookie = logged_in(&st).await;
    let headers = [&[("cookie", cookie.as_str()), ("content-type", "application/x-www-form-urlencoded")], extra].concat();
    let res = send(&st, request("POST", "/api/logout", &headers, Body::empty())).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert!(res.headers().get(header::SET_COOKIE).is_none(), "the browser's cookie must stay");
    assert!(json_of(res).await["error"].as_str().unwrap().contains("X-Flinch-Request: 1"));
    assert_eq!(send(&st, get("/api/status", &[("cookie", &cookie)])).await.status(), StatusCode::OK);
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn an_ended_session_is_refused_and_forgotten() {
    let tmp = scratch("ended");
    let st = login_state(&tmp, None);
    let cookie = logged_in(&st).await;
    end_sessions_at(&st, Instant::now());
    assert_eq!(send(&st, get("/api/status", &[("cookie", &cookie)])).await.status(), StatusCode::UNAUTHORIZED);
    assert!(st.auth.sessions().is_empty());
    let session = json_of(send(&st, get("/api/session", &[("cookie", &cookie)])).await).await;
    assert_eq!(session["authenticated"], false);
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn a_session_in_use_moves_its_end_once_a_day() {
    let tmp = scratch("slide");
    let st = login_state(&tmp, None);
    let cookie = logged_in(&st).await;
    let with_cookie = [("cookie", cookie.as_str())];
    // Used again within the day: nothing moves, no new cookie.
    let res = send(&st, get("/api/status", &with_cookie)).await;
    assert!(res.headers().get(header::SET_COOKIE).is_none());

    // Two days in: the end moves 30 days past now, and so does the cookie.
    end_sessions_at(&st, Instant::now() + SESSION_TTL - Duration::from_secs(2 * 24 * 60 * 60));
    let res = send(&st, get("/api/status", &with_cookie)).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store", "a live session id must never be cached");
    let set = res.headers()[header::SET_COOKIE].to_str().unwrap();
    assert!(set.starts_with(&format!("{cookie}; ")) && set.contains("Max-Age=2592000"), "{set}");
    let ends = *st.auth.sessions().values().next().unwrap();
    assert!(ends > Instant::now() + SESSION_TTL - SESSION_REFRESH);
    assert!(send(&st, get("/api/status", &with_cookie)).await.headers().get(header::SET_COOKIE).is_none());
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn every_login_gets_a_new_session_and_ends_the_old_one() {
    let tmp = scratch("rotate");
    let st = login_state(&tmp, None);
    let old = logged_in(&st).await;
    let res = send(&st, log_in_as(&USERNAME, &PASSWORD, &[("cookie", &old)])).await;
    let new = session_of(&res).unwrap();
    assert_ne!(new, old);
    assert_eq!(send(&st, get("/api/status", &[("cookie", &old)])).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(send(&st, get("/api/status", &[("cookie", &new)])).await.status(), StatusCode::OK);
    assert_eq!(st.auth.sessions().len(), 1);
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn sessions_are_capped_and_the_longest_idle_ends_first() {
    let tmp = scratch("cap");
    let st = login_state(&tmp, None);
    let first = logged_in(&st).await;
    end_sessions_at(&st, Instant::now() + SESSION_TTL - Duration::from_secs(60 * 60));
    let mut last = String::new();
    for _ in 0..MAX_SESSIONS {
        last = logged_in(&st).await;
    }
    assert_eq!(st.auth.sessions().len(), MAX_SESSIONS);
    assert_eq!(send(&st, get("/api/status", &[("cookie", &first)])).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(send(&st, get("/api/status", &[("cookie", &last)])).await.status(), StatusCode::OK);
    std::fs::remove_dir_all(&tmp).ok();
}

/// A write made with the cookie needs `X-Flinch-Request: 1`; reads and the
/// API key do not.
#[rstest]
#[case::cookie_write_without_header("PUT", "/api/settings", false, &[], StatusCode::FORBIDDEN)]
#[case::cookie_write_with_another_value("PUT", "/api/settings", false, &[("x-flinch-request", "0")], StatusCode::FORBIDDEN)]
#[case::cookie_write_from_the_ui("PUT", "/api/settings", false, &[("x-flinch-request", "1")], StatusCode::OK)]
#[case::cookie_run_without_header("POST", "/api/run", false, &[], StatusCode::FORBIDDEN)]
#[case::cookie_run_from_the_ui("POST", "/api/run", false, &[("x-flinch-request", "1")], StatusCode::ACCEPTED)]
#[case::cookie_systemone_without_header("POST", flinch_archive::systemone::PATH, false, &[], StatusCode::FORBIDDEN)]
#[case::cookie_read_without_header("GET", "/api/settings", false, &[], StatusCode::OK)]
#[case::key_write_without_header("PUT", "/api/settings", true, &[], StatusCode::OK)]
#[case::key_run_without_header("POST", "/api/run", true, &[], StatusCode::ACCEPTED)]
#[tokio::test]
async fn a_write_with_the_cookie_needs_the_ui_header(
    #[case] method: &str,
    #[case] path: &str,
    #[case] with_key: bool,
    #[case] extra: &[(&str, &str)],
    #[case] expected: StatusCode,
) {
    let tmp = scratch("csrf");
    let st = login_state(&tmp, Some(KEY));
    let cookie = logged_in(&st).await;
    let mut headers = vec![if with_key { ("x-api-key", KEY) } else { ("cookie", cookie.as_str()) }];
    headers.extend_from_slice(extra);
    let body = if path == "/api/settings" { "{}" } else { DECISION };
    let res = send(&st, request(method, path, &headers, body)).await;
    assert_eq!(res.status(), expected);
    // Nothing behind the login may be kept by a cache: a cookie, unlike
    // Authorization, does not stop a shared one from serving it again.
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
    if expected == StatusCode::FORBIDDEN {
        assert!(json_of(res).await["error"].as_str().unwrap().contains("X-Flinch-Request: 1"));
        assert!(!st.dir.join("settings.json").exists() && !st.dir.join("run.now").exists(), "a refused write must change nothing");
    }
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn failed_logins_pause_logins_and_a_success_resets_the_count() {
    let tmp = scratch("throttle");
    let st = login_state(&tmp, Some(KEY));
    let before = logged_in(&st).await;
    for n in 1..=THROTTLE_AFTER {
        let res = send(&st, log_in_as(&USERNAME, &WRONG, &[])).await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "failure {n}");
    }
    LOGGED.with(|lines| assert!(lines.borrow().iter().any(|line| line.ends_with("(5 in a row); logins pause for 30 s")), "{lines:?}"));

    // Paused: even the right password waits, and waiting is no failure.
    for password in [PASSWORD.as_str(), WRONG.as_str()] {
        let res = send(&st, log_in_as(&USERNAME, password, &[])).await;
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(res.headers()[header::RETRY_AFTER], "30");
        assert_eq!(json_of(res).await["error"], "Too many failed logins: try again in 30 s");
    }
    assert_eq!(st.auth.throttle.lock().unwrap().failures, THROTTLE_AFTER);
    // Open sessions and the API key are not paused.
    assert_eq!(send(&st, get("/api/status", &[("cookie", &before)])).await.status(), StatusCode::OK);
    assert_eq!(send(&st, get("/api/status", &[("x-api-key", KEY)])).await.status(), StatusCode::OK);

    // Once the pause is over, one success clears the count: the next pause
    // again takes five failures and lasts 30 s, not 60.
    end_the_pause(&st);
    assert_eq!(send(&st, log_in_as(&USERNAME, &PASSWORD, &[])).await.status(), StatusCode::NO_CONTENT);
    for _ in 1..=THROTTLE_AFTER {
        assert_eq!(send(&st, log_in_as(&USERNAME, &WRONG, &[])).await.status(), StatusCode::UNAUTHORIZED);
    }
    let res = send(&st, log_in_as(&USERNAME, &PASSWORD, &[])).await;
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(res.headers()[header::RETRY_AFTER], "30");
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn the_pause_doubles_with_each_failure_up_to_fifteen_minutes() {
    let start = Instant::now();
    let mut throttle = Throttle::default();
    let pauses: Vec<Option<u64>> = (0..12).map(|_| throttle.failed(start).map(|pause| pause.as_secs())).collect();
    let wanted = [None, None, None, None, Some(30), Some(60), Some(120), Some(240), Some(480), Some(900), Some(900), Some(900)];
    assert_eq!(pauses, wanted);
    assert_eq!(throttle.wait(start + Duration::from_secs(899)), Some(Duration::from_secs(1)));
    assert_eq!(throttle.wait(start + THROTTLE_MAX), None);
    assert_eq!(spoken(Duration::from_millis(29_001)), "30 s");
    assert_eq!(spoken(Duration::from_secs(61)), "2 min");
    throttle.succeeded();
    assert_eq!((throttle.failed(start), throttle.wait(start)), (None, None));
}

/// `(username, password, key)` on the server, and what then works: the login
/// needs both halves, the API key works alone, and neither refuses all.
#[rstest]
#[case::nothing(None, None, None, false, false)]
#[case::key_only(None, None, Some(KEY), false, true)]
#[case::half_a_login_and_a_key(Some(USERNAME.as_str()), None, Some(KEY), false, true)]
#[case::half_a_login(None, Some(PASSWORD.as_str()), None, false, false)]
#[case::login_only(Some(USERNAME.as_str()), Some(PASSWORD.as_str()), None, true, false)]
#[case::both(Some(USERNAME.as_str()), Some(PASSWORD.as_str()), Some(KEY), true, true)]
#[tokio::test]
async fn each_credential_works_only_when_it_is_set(
    #[case] username: Option<&str>,
    #[case] password: Option<&str>,
    #[case] key: Option<&str>,
    #[case] login_works: bool,
    #[case] key_works: bool,
) {
    let tmp = scratch("combos");
    let st = state_with(&tmp, Auth::new(username, password, key));
    let refused = send(&st, get("/api/status", &[])).await;
    assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
    let refused = json_of(refused).await;
    assert_eq!(refused["login_configured"], login_works);
    if !login_works {
        let fix = if key_works { LOGIN_OFF } else { NOTHING_SET };
        assert_eq!(refused["error"], fix);
    }

    let login = send(&st, log_in_as(&USERNAME, &PASSWORD, &[])).await;
    assert_eq!(login.status(), if login_works { StatusCode::NO_CONTENT } else { StatusCode::UNAUTHORIZED });
    assert_eq!(login.headers().get(header::SET_COOKIE).is_some(), login_works);
    let key_status = if key_works { StatusCode::OK } else { StatusCode::UNAUTHORIZED };
    for header in [("x-api-key", KEY.to_string()), ("authorization", format!("Bearer {KEY}"))] {
        assert_eq!(send(&st, get("/api/status", &[(header.0, &header.1)])).await.status(), key_status, "{}", header.0);
    }
    let session = json_of(send(&st, get("/api/session", &[])).await).await;
    assert_eq!(session, serde_json::json!({ "authenticated": false, "login_configured": login_works }));
    std::fs::remove_dir_all(&tmp).ok();
}

/// With the login set, a guessed key gets the same answer whether or not the
/// server has an API key, so nobody learns that one exists by guessing.
#[tokio::test]
async fn a_wrong_api_key_never_says_whether_one_is_set() {
    let tmp = scratch("key-exists");
    let (with_key, without_key) = (login_state(&tmp.join("with"), Some(KEY)), login_state(&tmp.join("without"), None));
    for guess in [("x-api-key", "guess".to_string()), ("authorization", "Bearer guess".to_string())] {
        for path in ["/api/status", "/api/nothing-here"] {
            let headers = [(guess.0, guess.1.as_str())];
            let with = transcript(send(&with_key, get(path, &headers)).await).await;
            let without = transcript(send(&without_key, get(path, &headers)).await).await;
            assert_eq!(with, without, "{} on {path}", guess.0);
            assert!(with.starts_with("401"), "{with}");
        }
    }
    std::fs::remove_dir_all(&tmp).ok();
}

/// The session endpoint is open, so it says whether you are in and whether
/// the login is set up, and nothing else: exact bodies, no extra field.
#[tokio::test]
async fn the_session_endpoint_says_only_whether_you_are_in() {
    let tmp = scratch("session");
    let st = login_state(&tmp, Some(KEY));
    let cookie = logged_in(&st).await;
    for (headers, authenticated) in [
        (vec![], false),
        (vec![("cookie", cookie.as_str())], true),
        (vec![("x-api-key", KEY)], true),
        (vec![("x-api-key", "nope")], false),
        (vec![("cookie", "flinch_session=0000")], false),
    ] {
        let res = send(&st, get("/api/session", &headers)).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
        let body = json_of(res).await;
        assert_eq!(body, serde_json::json!({ "authenticated": authenticated, "login_configured": true }), "{headers:?}");
    }
    std::fs::remove_dir_all(&tmp).ok();
}

/// Everything a response carries (status, headers, body), as one text.
async fn transcript(res: Response) -> String {
    let mut text = format!("{}\n", res.status());
    for (name, value) in res.headers() {
        text.push_str(&format!("{name}: {}\n", value.to_str().unwrap_or("<bytes>")));
    }
    text + &body_of(res).await
}

#[tokio::test]
async fn the_password_never_appears_in_a_response_or_a_log_line() {
    let tmp = scratch("leak");
    let st = login_state(&tmp, Some(KEY));
    LOGGED.with(|lines| lines.borrow_mut().clear());
    st.auth.announce(false);
    Auth::new(Some(USERNAME.as_str()), None, None).announce(true);
    let unterminated = format!(r#"{{"username": "{}", "password": "{}""#, *USERNAME, *PASSWORD);
    let mistyped = format!(r#"{{"username": "{}", "password": ["{}"]}}"#, *USERNAME, *PASSWORD);
    let mut seen = Vec::new();
    for body in [unterminated, mistyped] {
        seen.push(transcript(send(&st, request("POST", "/api/login", &[FROM_THE_UI], body)).await).await);
    }
    // The password typed as the username, then as part of a longer password,
    // until logins pause.
    seen.push(transcript(send(&st, log_in_as(&PASSWORD, &PASSWORD, &[])).await).await);
    for _ in 1..THROTTLE_AFTER {
        seen.push(transcript(send(&st, log_in_as(&USERNAME, &format!("{}!", *PASSWORD), &[])).await).await);
    }
    seen.push(transcript(send(&st, log_in_as(&USERNAME, &PASSWORD, &[])).await).await);
    end_the_pause(&st);
    let res = send(&st, log_in_as(&USERNAME, &PASSWORD, &[])).await;
    let cookie = session_of(&res).unwrap();
    seen.push(transcript(res).await);
    for path in ["/api/session", "/api/status", "/api/settings"] {
        seen.push(transcript(send(&st, get(path, &[("cookie", &cookie)])).await).await);
    }
    seen.push(transcript(send(&st, log_out(&cookie)).await).await);
    seen.push(transcript(send(&st, get("/api/status", &[("cookie", &cookie)])).await).await);

    assert!(seen.iter().any(|text| text.starts_with("429")), "the pause was reached");
    for text in &seen {
        assert!(!text.contains(PASSWORD.as_str()), "the password in a response: {text}");
        assert!(!text.contains(KEY), "the API key in a response: {text}");
    }
    let logged = LOGGED.with(|lines| lines.borrow().clone());
    assert!(logged.len() >= 3 + THROTTLE_AFTER as usize, "{logged:?}");
    // The failure message names no credential: it would print the value.
    for line in &logged {
        for needle in [PASSWORD.as_str(), USERNAME.as_str(), KEY, cookie.trim_start_matches("flinch_session=")] {
            assert!(!line.contains(needle), "a credential leaked into the log: {line}");
        }
    }
    std::fs::remove_dir_all(&tmp).ok();
}
