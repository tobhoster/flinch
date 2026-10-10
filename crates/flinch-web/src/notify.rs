//! "Send test" for the notification channels.
//!
//! The channels' URLs are secrets held in the daemon's environment, never
//! here, so the page asks the daemon through the state volume, as `run.now`
//! does: a request file the daemon answers within seconds (it looks every two
//! seconds, also mid-cycle), and the answer beside it, which the page polls.

use crate::{refuse, AppState};
use axum::extract::State as AxumState;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use flinch_archive::notify::{TestRequest, TEST_REQUEST_FILE, TEST_RESULT_FILE};

/// A request younger than this is still waiting for the daemon: another
/// click is refused rather than queued behind it.
const PENDING_SECS: u64 = 30;

fn now() -> std::time::Duration {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default()
}

/// Ask the daemon to post a test to every saved channel. Answers 202 with the
/// request's `id`; the result carries the same id once the daemon answered.
pub(crate) async fn request(AxumState(st): AxumState<AppState>) -> Response {
    let path = st.dir.join(TEST_REQUEST_FILE);
    let now = now();
    let pending = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<TestRequest>(&bytes).ok())
        .is_some_and(|pending| now.as_secs().saturating_sub(pending.requested_at) < PENDING_SECS);
    if pending {
        return refuse(StatusCode::CONFLICT, "A test is already waiting for the daemon");
    }
    let request = TestRequest { id: now.as_nanos().to_string(), requested_at: now.as_secs() };
    let written =
        serde_json::to_vec(&request).map_err(std::io::Error::from).and_then(|bytes| flinch_archive::persist::replace(&path, &bytes));
    match written {
        Ok(()) => (StatusCode::ACCEPTED, axum::Json(serde_json::json!({ "id": request.id }))).into_response(),
        Err(error) => refuse(StatusCode::INTERNAL_SERVER_ERROR, &format!("cannot ask for a test: {error}")),
    }
}

/// The daemon's latest answer, or `null` before any.
pub(crate) async fn result(AxumState(st): AxumState<AppState>) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], crate::read_json(&st.dir.join(TEST_RESULT_FILE))).into_response()
}

#[cfg(test)]
mod tests {
    use crate::tests::{body_of, request, scratch, send, state, TOKEN};
    use axum::body::Body;
    use axum::http::StatusCode;
    use flinch_archive::notify::{TestRequest, TEST_REQUEST_FILE, TEST_RESULT_FILE};

    #[tokio::test]
    async fn a_test_is_asked_of_the_daemon_once_and_its_answer_is_served() {
        let tmp = scratch("notify-test");
        let st = state(&tmp, Some(TOKEN));
        let auth = format!("Bearer {TOKEN}");
        let headers = [("authorization", auth.as_str())];

        let asked = send(&st, request("POST", "/api/notify/test", &headers, Body::empty())).await;
        let status = asked.status();
        let id: serde_json::Value = serde_json::from_str(&body_of(asked).await).expect("json");
        let again = send(&st, request("POST", "/api/notify/test", &headers, Body::empty())).await;

        assert_eq!(status, StatusCode::ACCEPTED);
        let written: TestRequest = serde_json::from_slice(&std::fs::read(st.dir.join(TEST_REQUEST_FILE)).expect("request")).expect("json");
        assert_eq!(id["id"], written.id);
        assert_eq!(again.status(), StatusCode::CONFLICT, "a waiting request is not replaced");

        std::fs::write(st.dir.join(TEST_RESULT_FILE), r#"{"id":"1","finished_at":2,"channels":[]}"#).expect("result");
        let answer = send(&st, request("GET", "/api/notify/test", &headers, Body::empty())).await;
        assert_eq!(body_of(answer).await, r#"{"id":"1","finished_at":2,"channels":[]}"#);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn asking_for_a_test_needs_the_login() {
        let tmp = scratch("notify-test-auth");
        let st = state(&tmp, Some(TOKEN));
        let refused = send(&st, request("POST", "/api/notify/test", &[], Body::empty())).await;
        assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
        assert!(!st.dir.join(TEST_REQUEST_FILE).exists());
        std::fs::remove_dir_all(&tmp).ok();
    }
}
