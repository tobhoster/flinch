//! The *arr fleet page's API. `GET /api/fleet` serves the daemon's
//! `arr-fleet-status.json` (`null` before its first read). The keys live
//! with the daemon, so the two actions only queue a request under
//! `fleet-requests/` and wake the daemon (`run.now`), which carries it out at
//! the top of its next cycle, honouring the dry run
//! ([`flinch_archive::fleet::requests`]). A removal is queued only for an
//! item the last read judged removable: one waiting for free space never is.

use crate::{read_json, refuse, AppState};
use axum::extract::{Path as AxumPath, State as AxumState};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use flinch_archive::fleet::requests::FleetRequest;
use flinch_archive::fleet::{FleetStatus, STATUS_FILE};

pub(crate) async fn status(AxumState(st): AxumState<AppState>) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/json")], read_json(&st.dir.join(STATUS_FILE)))
}

/// `POST /api/fleet/queue/{app}/{id}/remove`, `app` an instance key
/// (`sonarr`, `radarr@4k`).
pub(crate) async fn remove(AxumState(st): AxumState<AppState>, AxumPath((app, id)): AxumPath<(String, String)>) -> Response {
    let Some(FleetRequest::QueueRemove { app, instance, id }) = FleetRequest::queue_remove(&app, &id) else {
        return refuse(StatusCode::BAD_REQUEST, "not a Radarr or Sonarr instance and queue id");
    };
    let last = last_read(&st);
    let Some(item) = last.as_ref().and_then(|status| status.stalled_item(app.into(), &instance, u64::from(id))) else {
        return refuse(StatusCode::NOT_FOUND, "the last read lists no such stuck queue item");
    };
    if !item.removable {
        return refuse(StatusCode::CONFLICT, &format!("refused: queue items that {} are never removed", item.reason.describe()));
    }
    enqueue(&st, &FleetRequest::QueueRemove { app, instance, id })
}

/// `POST /api/fleet/indexer/{id}/test`.
pub(crate) async fn test_indexer(AxumState(st): AxumState<AppState>, AxumPath(id): AxumPath<String>) -> Response {
    let Some(request @ FleetRequest::IndexerTest { id }) = FleetRequest::indexer_test(&id) else {
        return refuse(StatusCode::BAD_REQUEST, "not an indexer id");
    };
    if last_read(&st).as_ref().and_then(|status| status.indexer(u64::from(id))).is_none() {
        return refuse(StatusCode::NOT_FOUND, "Prowlarr listed no such indexer at the last read");
    }
    enqueue(&st, &request)
}

/// The daemon's last read; `None` before its first or while unreadable.
fn last_read(st: &AppState) -> Option<FleetStatus> {
    let bytes = std::fs::read(st.dir.join(STATUS_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn enqueue(st: &AppState, request: &FleetRequest) -> Response {
    let path = request.path(&st.dir);
    let written = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&path, b""))
        .and_then(|()| std::fs::write(st.dir.join("run.now"), b"1"));
    match written {
        Ok(()) => {
            let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
            (StatusCode::ACCEPTED, axum::Json(serde_json::json!({ "queued": name }))).into_response()
        }
        Err(error) => refuse(StatusCode::INTERNAL_SERVER_ERROR, &format!("cannot queue the request: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use crate::tests::{body_of, get, request, scratch, send, state, TOKEN};
    use axum::body::Body;
    use axum::http::StatusCode;
    use rstest::rstest;

    /// Sonarr's last read: 1 waits for space, 2 never imports; Prowlarr lists indexer 36.
    const LAST_READ: &str = r#"{"checked_at":1,"instances":[{"app":"sonarr","instance":"","url":"http://sonarr","reachable":true,
        "queue":{"total":2,"stalled":[
          {"id":1,"title":"a","state":"importPending","status":"warning","messages":["Not enough free space"],"reason":"no_space","removable":false,"since":1,"size_bytes":1},
          {"id":2,"title":"b","state":"importBlocked","status":"warning","messages":["Sample"],"reason":"unimportable","removable":true,"since":1,"size_bytes":1}]}}],
        "indexers":[{"id":36,"name":"AniSource","enabled":true,"protocol":"torrent","failing":true,"queries":0,"grabs":0,"failed_queries":0,"avg_response_ms":0}],
        "actions":[]}"#;

    #[rstest]
    #[case::an_unimportable_item("/api/fleet/queue/sonarr/2/remove", StatusCode::ACCEPTED, Some("queue-remove.sonarr-2"))]
    #[case::an_item_waiting_for_space("/api/fleet/queue/sonarr/1/remove", StatusCode::CONFLICT, None)]
    #[case::an_item_not_listed("/api/fleet/queue/sonarr/3/remove", StatusCode::NOT_FOUND, None)]
    #[case::another_instance("/api/fleet/queue/sonarr@anime/2/remove", StatusCode::NOT_FOUND, None)]
    #[case::not_an_arr("/api/fleet/queue/lidarr/2/remove", StatusCode::BAD_REQUEST, None)]
    #[case::a_path("/api/fleet/queue/..%2F..%2Fsettings.json/2/remove", StatusCode::BAD_REQUEST, None)]
    #[case::a_listed_indexer("/api/fleet/indexer/36/test", StatusCode::ACCEPTED, Some("indexer-test.36"))]
    #[case::an_unlisted_indexer("/api/fleet/indexer/37/test", StatusCode::NOT_FOUND, None)]
    #[case::not_an_indexer_id("/api/fleet/indexer/x/test", StatusCode::BAD_REQUEST, None)]
    #[tokio::test]
    async fn only_a_removable_item_or_a_listed_indexer_is_queued_for_the_daemon(
        #[case] path: &str,
        #[case] expected: StatusCode,
        #[case] queued_as: Option<&str>,
    ) {
        let tmp = scratch(&format!("fleet-{}", expected.as_u16()));
        let st = state(&tmp, Some(TOKEN));
        std::fs::write(st.dir.join("arr-fleet-status.json"), LAST_READ).expect("the last read");
        let auth = format!("Bearer {TOKEN}");
        let answer = send(&st, request("POST", path, &[("authorization", auth.as_str())], Body::empty())).await;
        let queue: Vec<String> = std::fs::read_dir(st.dir.join("fleet-requests"))
            .map(|entries| entries.filter_map(|entry| entry.ok()?.file_name().into_string().ok()).collect())
            .unwrap_or_default();
        let run_now = st.dir.join("run.now").exists();
        std::fs::remove_dir_all(&tmp).ok();

        assert_eq!(answer.status(), expected);
        assert_eq!(queue, queued_as.into_iter().map(str::to_string).collect::<Vec<_>>());
        assert_eq!(run_now, queued_as.is_some(), "the daemon is woken only for a queued request");
    }

    #[tokio::test]
    async fn the_fleet_is_null_before_the_daemons_first_read_and_its_file_after() {
        let tmp = scratch("fleet-get");
        let st = state(&tmp, Some(TOKEN));
        let auth = format!("Bearer {TOKEN}");
        let before = body_of(send(&st, get("/api/fleet", &[("authorization", auth.as_str())])).await).await;
        std::fs::write(st.dir.join("arr-fleet-status.json"), LAST_READ).expect("the last read");
        let after = body_of(send(&st, get("/api/fleet", &[("authorization", auth.as_str())])).await).await;
        let anonymous = send(&st, get("/api/fleet", &[])).await.status();
        std::fs::remove_dir_all(&tmp).ok();

        assert_eq!(before, "null");
        assert_eq!(after, LAST_READ);
        assert_eq!(anonymous, StatusCode::UNAUTHORIZED);
    }
}
