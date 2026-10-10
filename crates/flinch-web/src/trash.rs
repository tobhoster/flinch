//! The Quality profiles page's two calls. The *arr keys live with the daemon,
//! so the page never talks to Radarr or Sonarr: it reads the preview the
//! daemon published (`trash.json`) and leaves its selection on the state
//! volume (`trash-apply.json` plus `run.now`), which the daemon applies at
//! the top of its next cycle, honouring the dry run.

use crate::{refuse, AppState};
use axum::extract::State as AxumState;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use flinch_archive::trash::{ApplyRequest, TrashState, APPLY_REQUEST_FILE};
use std::collections::BTreeSet;

/// More than any preview holds (a full guide profile set is a few hundred).
const MAX_CHANGES: usize = 2000;

/// The daemon's last preview and apply, with any selection still waiting for
/// it as `apply_pending`; `null` before the first preview.
pub(crate) async fn diff(AxumState(st): AxumState<AppState>) -> Response {
    let preview: serde_json::Value =
        serde_json::from_str(&crate::read_json(&st.dir.join(flinch_archive::trash::STATE_FILE))).unwrap_or_default();
    let body = match preview {
        serde_json::Value::Object(mut object) => {
            let pending = std::fs::read(st.dir.join(APPLY_REQUEST_FILE))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
            object.insert("apply_pending".to_string(), pending.unwrap_or_default());
            serde_json::Value::Object(object)
        }
        _ => serde_json::Value::Null,
    };
    ([(header::CONTENT_TYPE, "application/json")], body.to_string()).into_response()
}

/// Queue the selected changes for the daemon. Every id must be in the
/// current preview: a stale page cannot apply what it never showed.
pub(crate) async fn apply(AxumState(st): AxumState<AppState>, body: String) -> Response {
    let request: ApplyRequest = match serde_json::from_str(&body) {
        Ok(request) => request,
        Err(error) => return refuse(StatusCode::BAD_REQUEST, &format!("the selection is not JSON the daemon reads: {error}")),
    };
    if request.changes.is_empty() || request.changes.len() > MAX_CHANGES {
        return refuse(StatusCode::BAD_REQUEST, "select at least one change to apply");
    }
    let Some(preview) = TrashState::read(&st.dir).filter(|state| state.enabled) else {
        return refuse(StatusCode::CONFLICT, "the TRaSH sync is off or has not previewed yet");
    };
    let known: BTreeSet<&str> = preview.apps.iter().flat_map(|app| app.changes.iter().map(|change| change.id.as_str())).collect();
    if let Some(unknown) = request.changes.iter().find(|id| !known.contains(id.as_str())) {
        return refuse(StatusCode::BAD_REQUEST, &format!("{unknown} is not in the current preview; reload the page"));
    }
    let path = st.dir.join(APPLY_REQUEST_FILE);
    if path.exists() {
        return refuse(StatusCode::CONFLICT, "an apply is already waiting for the daemon");
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let queued = ApplyRequest { changes: request.changes, requested_at_unix: now };
    let written = serde_json::to_vec(&queued)
        .map_err(std::io::Error::from)
        .and_then(|bytes| flinch_archive::persist::replace(&path, &bytes))
        .and_then(|()| std::fs::write(st.dir.join("run.now"), b"1"));
    match written {
        Ok(()) => (StatusCode::ACCEPTED, axum::Json(serde_json::json!({ "queued": queued.changes.len() }))).into_response(),
        Err(error) => refuse(StatusCode::INTERNAL_SERVER_ERROR, &format!("cannot queue the apply: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use crate::tests::{body_of, request, scratch, send, state, TOKEN};
    use axum::body::Body;
    use axum::http::StatusCode;
    use flinch_archive::trash::{ApplyRequest, APPLY_REQUEST_FILE, STATE_FILE};
    use rstest::rstest;

    const PREVIEW: &str = r#"{"enabled":true,"pcd_license":"MIT","apps":[{"app":"radarr","source":"pcd","changes":[
        {"id":"radarr:sizes:movie","app":"radarr","kind":"quality_definition","action":"update","name":"Quality sizes (movie)"},
        {"id":"radarr:profile-delete:4","app":"radarr","kind":"quality_profile","action":"delete","name":"Old","arr_id":4}],
        "impacts":{"radarr:sizes:movie":{"items":3,"sampled":2,"delta_bytes":1073741824,"warnings":["WEBDL-1080p: max size rises from 100 to 2000 MB/min"]}}}]}"#;

    #[rstest]
    #[case::a_change_in_the_preview(r#"{"changes":["radarr:sizes:movie"]}"#, StatusCode::ACCEPTED)]
    #[case::an_unused_profile_the_preview_offered(r#"{"changes":["radarr:profile-delete:4"]}"#, StatusCode::ACCEPTED)]
    #[case::a_change_the_page_never_showed(r#"{"changes":["radarr:cf-delete:1"]}"#, StatusCode::BAD_REQUEST)]
    #[case::nothing_selected(r#"{"changes":[]}"#, StatusCode::BAD_REQUEST)]
    #[tokio::test]
    async fn only_a_selection_from_the_current_preview_is_queued_for_the_daemon(#[case] body: &'static str, #[case] expected: StatusCode) {
        let tmp = scratch(&format!("trash-apply-{}-{}", expected.as_u16(), body.len()));
        let st = state(&tmp, Some(TOKEN));
        std::fs::write(st.dir.join(STATE_FILE), PREVIEW).expect("a preview");
        let auth = format!("Bearer {TOKEN}");
        let headers = [("authorization", auth.as_str())];

        let answer = send(&st, request("POST", "/api/trash/apply", &headers, Body::from(body))).await;
        let status = answer.status();
        let diff = body_of(send(&st, request("GET", "/api/trash/diff", &headers, Body::empty())).await).await;
        let queued =
            std::fs::read(st.dir.join(APPLY_REQUEST_FILE)).ok().and_then(|bytes| serde_json::from_slice::<ApplyRequest>(&bytes).ok());
        let run_now = st.dir.join("run.now").exists();
        std::fs::remove_dir_all(&tmp).ok();

        assert_eq!(status, expected);
        let accepted = expected == StatusCode::ACCEPTED;
        let asked: ApplyRequest = serde_json::from_str(body).expect("a request");
        assert_eq!(queued.map(|q| q.changes), accepted.then_some(asked.changes));
        assert_eq!(run_now, accepted, "the daemon is woken only for a queued apply");
        let diff: serde_json::Value = serde_json::from_str(&diff).expect("the preview is JSON");
        assert_eq!(diff["apps"][0]["changes"][0]["id"], "radarr:sizes:movie");
        assert_eq!(diff["apply_pending"].is_object(), accepted);
        assert_eq!(diff["apps"][0]["impacts"]["radarr:sizes:movie"]["delta_bytes"], 1_073_741_824_u64, "the estimate reaches the page");
        assert_eq!(diff["pcd_license"], "MIT");
    }
}
