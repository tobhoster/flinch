//! The Overview's duplicate choices. The page never talks to Plex or the
//! *arrs: it reads the groups the daemon published (`status.dupes`) and
//! stores the operator's choice in `dupes.json`, which the daemon reads at
//! the top of its next cycle. Choosing never deletes; confirming the same
//! copy afterwards lets the daemon remove the others, and only when acting
//! is switched on.

use crate::{refuse, AppState};
use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use flinch_archive::dupes::{Decisions, DupesStatus};

/// `{"group": "tmdb:949", "keep": "plex:500:51", "confirm": true}`; a `null`
/// keep clears the choice.
#[derive(serde::Deserialize)]
pub(crate) struct Choice {
    group: String,
    keep: Option<String>,
    #[serde(default)]
    confirm: bool,
}

pub(crate) async fn decide(AxumState(st): AxumState<AppState>, body: String) -> Response {
    let choice: Choice = match serde_json::from_str(&body) {
        Ok(choice) => choice,
        Err(error) => return refuse(StatusCode::BAD_REQUEST, &format!("the choice is not JSON the daemon reads: {error}")),
    };
    // Checked against the daemon's latest groups: a stale page cannot choose
    // a copy it never showed.
    let status: Option<DupesStatus> = serde_json::from_str::<serde_json::Value>(&crate::read_json(&st.dir.join("status.json")))
        .ok()
        .and_then(|mut status| serde_json::from_value(status["dupes"].take()).ok());
    let Some(group) = status.as_ref().and_then(|status| status.groups.iter().find(|group| group.id == choice.group)) else {
        return refuse(StatusCode::NOT_FOUND, "that duplicate group is not in the latest run; reload the page");
    };
    let mut decisions = match Decisions::read(&st.dir) {
        Ok(decisions) => decisions,
        Err(error) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    if let Err(error) = decisions.choose(group, choice.keep.as_deref(), choice.confirm, now) {
        return refuse(StatusCode::CONFLICT, &error.to_string());
    }
    let written =
        decisions.write(&st.dir).and_then(|()| if choice.confirm { std::fs::write(st.dir.join("run.now"), b"1") } else { Ok(()) });
    match written {
        Ok(()) => (StatusCode::OK, axum::Json(serde_json::json!({ "decision": decisions.groups.get(&choice.group) }))).into_response(),
        Err(error) => refuse(StatusCode::INTERNAL_SERVER_ERROR, &format!("cannot store the choice: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use crate::tests::{request, scratch, send, state, TOKEN};
    use axum::body::Body;
    use axum::http::StatusCode;
    use flinch_archive::dupes::Decisions;
    use rstest::rstest;

    const STATUS: &str = r#"{"dupes":{"act":false,"dry_run":true,"unowned":[],"acted":[],"problems":[],"groups":[{
        "id":"tmdb:949","card_id":"radarr-7","title":"Heat","year":1995,"recommended":"plex:100:11","reasons":[],"redundant_bytes":1,
        "copies":[{"id":"plex:100:11","source":"plex","bytes":1,"plays":0},{"id":"plex:500:51","source":"plex","bytes":1,"plays":0}]}]}}"#;

    /// Sends each body in turn; returns the last status and the stored choice.
    async fn post(label: &str, bodies: &[&str]) -> (StatusCode, Option<(String, bool)>, bool) {
        let tmp = scratch(label);
        let st = state(&tmp, Some(TOKEN));
        std::fs::write(st.dir.join("status.json"), STATUS).expect("status");
        let auth = format!("Bearer {TOKEN}");
        let mut last = StatusCode::OK;
        for body in bodies {
            let headers = [("authorization", auth.as_str()), ("content-type", "application/json")];
            last = send(&st, request("POST", "/api/dupes/decide", &headers, Body::from(body.to_string()))).await.status();
        }
        let stored = Decisions::read(&st.dir).expect("readable").groups.get("tmdb:949").map(|d| (d.keep.clone(), d.confirmed));
        let woke = st.dir.join("run.now").exists();
        std::fs::remove_dir_all(&tmp).ok();
        (last, stored, woke)
    }

    const CHOOSE: &str = r#"{"group":"tmdb:949","keep":"plex:500:51"}"#;
    const CONFIRM: &str = r#"{"group":"tmdb:949","keep":"plex:500:51","confirm":true}"#;

    #[rstest]
    #[case::a_choice_is_stored_unconfirmed("dupes-choose", &[CHOOSE], StatusCode::OK, Some(("plex:500:51", false)), false)]
    #[case::confirming_it_wakes_the_daemon("dupes-confirm", &[CHOOSE, CONFIRM], StatusCode::OK, Some(("plex:500:51", true)), true)]
    #[case::confirming_without_a_choice_is_refused("dupes-blind", &[CONFIRM], StatusCode::CONFLICT, None, false)]
    #[case::a_copy_not_in_the_group_is_refused("dupes-stale", &[r#"{"group":"tmdb:949","keep":"plex:9:9"}"#], StatusCode::CONFLICT, None, false)]
    #[case::a_group_not_in_the_run_is_refused("dupes-gone", &[r#"{"group":"tmdb:1","keep":"plex:100:11"}"#], StatusCode::NOT_FOUND, None, false)]
    #[case::null_clears_the_choice("dupes-clear", &[CHOOSE, r#"{"group":"tmdb:949","keep":null}"#], StatusCode::OK, None, false)]
    #[tokio::test]
    async fn choices_are_checked_against_the_latest_run(
        #[case] label: &str,
        #[case] bodies: &[&str],
        #[case] status: StatusCode,
        #[case] stored: Option<(&str, bool)>,
        #[case] woke: bool,
    ) {
        let (got, kept, run_now) = post(label, bodies).await;
        assert_eq!(got, status);
        assert_eq!(kept.as_ref().map(|(keep, confirmed)| (keep.as_str(), *confirmed)), stored);
        assert_eq!(run_now, woke, "only a confirmed choice wakes the daemon");
    }

    #[tokio::test]
    async fn a_copy_of_a_named_instance_is_chosen_and_confirmed_by_its_id() {
        // Heat in the HD Radarr and in the 4K one: two *arr files Plex lists nowhere.
        let status = r#"{"dupes":{"act":false,"dry_run":true,"unowned":[],"acted":[],"problems":[],"groups":[{
            "id":"tmdb:949","card_id":"radarr-7","title":"Heat","year":1995,"recommended":"radarr@4k:7:71","reasons":[],"redundant_bytes":1,
            "copies":[
              {"id":"radarr:7:70","source":"arr","bytes":1,"plays":0,
               "owner":{"instance":"radarr","movie_id":7,"file_id":70,"path":"/movies/Heat.mkv","bytes":1}},
              {"id":"radarr@4k:7:71","source":"arr","bytes":1,"plays":0,
               "owner":{"instance":"radarr@4k","movie_id":7,"file_id":71,"path":"/movies4k/Heat.mkv","bytes":1}}]}]}}"#;
        let tmp = scratch("dupes-named");
        let st = state(&tmp, Some(TOKEN));
        std::fs::write(st.dir.join("status.json"), status).expect("status");
        let auth = format!("Bearer {TOKEN}");
        let headers = [("authorization", auth.as_str()), ("content-type", "application/json")];
        for body in [r#"{"group":"tmdb:949","keep":"radarr@4k:7:71"}"#, r#"{"group":"tmdb:949","keep":"radarr@4k:7:71","confirm":true}"#] {
            let answer = send(&st, request("POST", "/api/dupes/decide", &headers, Body::from(body))).await;
            assert_eq!(answer.status(), StatusCode::OK, "{body}");
        }
        let decision = Decisions::read(&st.dir).expect("readable").groups.get("tmdb:949").cloned();
        std::fs::remove_dir_all(&tmp).ok();
        let decision = decision.expect("stored");
        assert_eq!((decision.keep.as_str(), decision.confirmed), ("radarr@4k:7:71", true));
        assert_eq!(decision.copies, ["radarr:7:70", "radarr@4k:7:71"]);
    }
}
