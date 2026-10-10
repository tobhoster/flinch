//! Undo for the native executor's deletes. The *arr keys live with the daemon,
//! so the page never talks to Radarr or Sonarr: a restore drops
//! `restore/<card id>` on the state volume and wakes the daemon (`run.now`),
//! which monitors and searches the item again at the top of its next cycle,
//! honouring the dry run. Only a delete FLINCH made in the last 30 days, and
//! not restored yet, can be queued.

use crate::{refuse, AppState};
use axum::extract::{Path as AxumPath, State as AxumState};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use flinch_archive::executor::state::{is_card_id, restore_dir};
use flinch_archive::executor::NativeState;

pub(crate) async fn request(AxumState(st): AxumState<AppState>, AxumPath(id): AxumPath<String>) -> Response {
    // Checked before it becomes a file name: only `radarr-<n>`, `sonarr-<n>-s<n>`
    // or the same of a named instance (`radarr@4k-<n>`).
    if !is_card_id(&id) {
        return refuse(StatusCode::BAD_REQUEST, "not a FLINCH item id");
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    if NativeState::read(&st.dir).restorable(&id, now).is_none() {
        return refuse(StatusCode::NOT_FOUND, "FLINCH made no delete of this item in the last 30 days that is not restored yet");
    }
    let queue = restore_dir(&st.dir);
    let written = std::fs::create_dir_all(&queue)
        .and_then(|()| std::fs::write(queue.join(&id), b""))
        .and_then(|()| std::fs::write(st.dir.join("run.now"), b"1"));
    match written {
        Ok(()) => (StatusCode::ACCEPTED, axum::Json(serde_json::json!({ "queued": id }))).into_response(),
        Err(error) => refuse(StatusCode::INTERNAL_SERVER_ERROR, &format!("cannot queue the restore: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use crate::tests::{request, scratch, send, state, TOKEN};
    use axum::body::Body;
    use axum::http::StatusCode;
    use rstest::rstest;

    /// One native delete of the card `id` (Heat), `days` days ago, restored or not.
    fn deleted(id: &str, days: u64, restored: bool) -> String {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let restored_at = if restored { "1" } else { "null" };
        format!(
            r#"{{"deleted":[{{"id":"{id}","title":"Heat","kind":"MOVIE","bytes":1,"deleted_at":{},"announced":false,
            "target":{{"app":"radarr","radarr_id":7,"tmdb_id":949,"mode":"file_and_unmonitor","quality_profile_id":1,"root_folder_path":"/m"}},
            "restored_at":{restored_at}}}]}}"#,
            now - days * 86_400
        )
    }

    #[rstest]
    #[case::a_recent_delete("radarr-7", deleted("radarr-7", 2, false), StatusCode::ACCEPTED)]
    #[case::a_named_instances_recent_delete("radarr@4k-7", deleted("radarr@4k-7", 2, false), StatusCode::ACCEPTED)]
    #[case::a_named_instances_season("sonarr@anime-3-s2", deleted("sonarr@anime-3-s2", 2, false), StatusCode::ACCEPTED)]
    #[case::older_than_thirty_days("radarr-7", deleted("radarr-7", 31, false), StatusCode::NOT_FOUND)]
    #[case::already_restored("radarr-7", deleted("radarr-7", 2, true), StatusCode::NOT_FOUND)]
    #[case::never_deleted("radarr-8", deleted("radarr-7", 2, false), StatusCode::NOT_FOUND)]
    #[case::the_same_movie_of_another_instance("radarr@4k-7", deleted("radarr-7", 2, false), StatusCode::NOT_FOUND)]
    #[case::not_an_item_id("..%2Fsettings.json", deleted("radarr-7", 2, false), StatusCode::BAD_REQUEST)]
    #[case::not_an_instance_name("radarr@..%2F-7", deleted("radarr-7", 2, false), StatusCode::BAD_REQUEST)]
    #[case::an_upper_case_instance("radarr@4K-7", deleted("radarr@4K-7", 2, false), StatusCode::BAD_REQUEST)]
    #[tokio::test]
    async fn only_a_recent_native_delete_is_queued_for_the_daemon(#[case] id: &str, #[case] native: String, #[case] expected: StatusCode) {
        let tmp = scratch(&format!("restore-{}", expected.as_u16()));
        // What the daemon reads back from the queue: the id, decoded.
        let queued_as = id.replace("%2F", "/");
        let st = state(&tmp, Some(TOKEN));
        std::fs::write(st.dir.join("native.json"), native).expect("native state");
        let auth = format!("Bearer {TOKEN}");
        let answer = send(&st, request("POST", &format!("/api/restore/{id}"), &[("authorization", auth.as_str())], Body::empty())).await;
        let queued = st.dir.join("restore").join(&queued_as).exists();
        let run_now = st.dir.join("run.now").exists();
        std::fs::remove_dir_all(&tmp).ok();

        assert_eq!(answer.status(), expected);
        let accepted = expected == StatusCode::ACCEPTED;
        assert_eq!(queued, accepted, "only an accepted restore is queued");
        assert_eq!(run_now, accepted, "the daemon is woken only for a queued restore");
    }
}
