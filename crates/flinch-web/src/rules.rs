//! `POST /api/rules/preview`: what a draft rule list would change before it
//! is saved. The planner runs here, twice, on the inputs the daemon last
//! published (`plan-inputs.json`): under the saved rules and under the draft
//! ([`flinch_archive::rules::preview`]). Nothing is written.

use crate::{refuse, AppState};
use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use flinch_archive::daemon::read_settings;
use flinch_archive::rules::preview::{preview, PlanInputs, INPUTS_FILE};
use flinch_archive::rules::{validate, Rule};

#[derive(serde::Deserialize)]
struct Draft {
    rules: Vec<Rule>,
}

pub(crate) async fn api_preview(AxumState(st): AxumState<AppState>, body: String) -> Response {
    let draft = match serde_json::from_str::<Draft>(&body) {
        Ok(draft) => draft.rules,
        Err(error) => return refuse(StatusCode::BAD_REQUEST, &format!("invalid rules: {error}")),
    };
    if let Err(error) = validate(&draft) {
        return refuse(StatusCode::BAD_REQUEST, &error.to_string());
    }
    let settings = match read_settings(&st.dir.join("settings.json")) {
        Ok(settings) => settings,
        Err(error) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    let inputs: PlanInputs = match std::fs::read(st.dir.join(INPUTS_FILE)) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(inputs) => inputs,
            Err(error) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, &format!("{INPUTS_FILE} unreadable: {error}")),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return refuse(StatusCode::CONFLICT, "No plan inputs yet: the daemon publishes them on its next run")
        }
        Err(error) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, &format!("{INPUTS_FILE} unreadable: {error}")),
    };
    // HiGHS is CPU-bound: off the async workers.
    let planned = tokio::task::spawn_blocking(move || preview(&inputs, &settings.planner, &settings.rules, &draft)).await;
    match planned {
        Ok(Ok(diff)) => axum::Json(diff).into_response(),
        Ok(Err(error)) => refuse(StatusCode::INTERNAL_SERVER_ERROR, &format!("planning failed: {error}")),
        Err(error) => refuse(StatusCode::INTERNAL_SERVER_ERROR, &format!("planning failed: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use crate::tests::{body_of, request, scratch, send, state, TOKEN};
    use axum::body::Body;
    use axum::http::StatusCode;
    use flinch_archive::capacity::{CapacityForecast, VolumeForecast};
    use flinch_archive::plan::MediaCandidate;
    use flinch_archive::regret::Regret;
    use flinch_archive::rules::preview::{PlanInputs, INPUTS_FILE};
    use rstest::rstest;

    const GIB: u64 = 1 << 30;

    fn candidate(id: &str, p_watch: f64) -> MediaCandidate {
        let regret = Regret::new(p_watch, 1.0, 1.0);
        MediaCandidate {
            id: id.to_string(),
            title: id.to_string(),
            size_bytes: 10 * GIB,
            volume: Some("movies".to_string()),
            regret,
            reason: String::new(),
            age_days: 400.0,
            exclusion: None,
            sequence: None,
            handed: false,
            announce: false,
            protect: false,
            quality: flinch_archive::quality::advise(&regret, &flinch_archive::quality::Item::default()),
            eviction_safety: 0.0,
            force: None,
        }
    }

    fn inputs() -> PlanInputs {
        let forecast = CapacityForecast {
            current_used_bytes: 0,
            max_capacity_bytes: 1,
            current_utilization: 0.0,
            daily_ingest_rate_bytes: 0,
            queue_bytes: 0,
            in_flight_bytes: 0,
            projected_used_bytes: 0,
            target_reclaim_bytes: 10 * GIB,
            is_emergency: false,
        };
        PlanInputs {
            computed_at: 1,
            forecasts: vec![VolumeForecast { volume: "movies".to_string(), forecast }],
            candidates: vec![candidate("cheap", 0.1), candidate("dear", 0.9)],
            facts: Default::default(),
        }
    }

    const KEEP_CHEAP: &str = r#"{"rules": [{"name": "keep cheap", "scope": {"p_watch": {"max": 0.2}}, "effect": {"type": "keep"}}]}"#;

    #[tokio::test]
    async fn a_draft_is_planned_against_the_published_inputs_and_diffed() {
        let tmp = scratch("rules-preview");
        let st = state(&tmp, Some(TOKEN));
        std::fs::write(st.dir.join(INPUTS_FILE), serde_json::to_vec(&inputs()).expect("json")).expect("inputs");
        let auth = format!("Bearer {TOKEN}");
        let answer = send(&st, request("POST", "/api/rules/preview", &[("authorization", auth.as_str())], Body::from(KEEP_CHEAP))).await;
        assert_eq!(answer.status(), StatusCode::OK);
        let diff: serde_json::Value = serde_json::from_str(&body_of(answer).await).expect("json");
        assert_eq!(diff["added"][0]["id"], "dear");
        assert_eq!(diff["removed"][0]["id"], "cheap");
        assert_eq!(diff["removed"][0]["kept_because"], "Kept by rule \u{201c}keep cheap\u{201d}");
        assert!(!st.dir.join("settings.json").exists(), "a preview saves nothing");
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn a_rule_naming_one_instances_copy_keeps_that_copy_only() {
        let tmp = scratch("rules-preview-named");
        let st = state(&tmp, Some(TOKEN));
        // The HD and the 4K copy of one movie; the 4K one is the cheaper loss.
        let published = PlanInputs { candidates: vec![candidate("radarr-1", 0.9), candidate("radarr@4k-1", 0.1)], ..inputs() };
        std::fs::write(st.dir.join(INPUTS_FILE), serde_json::to_vec(&published).expect("json")).expect("inputs");
        let draft = r#"{"rules": [{"name": "keep the 4K", "scope": {"ids": ["radarr@4k-1"]}, "effect": {"type": "keep"}}]}"#;
        let auth = format!("Bearer {TOKEN}");
        let answer = send(&st, request("POST", "/api/rules/preview", &[("authorization", auth.as_str())], Body::from(draft))).await;
        assert_eq!(answer.status(), StatusCode::OK);
        let diff: serde_json::Value = serde_json::from_str(&body_of(answer).await).expect("json");
        assert_eq!(diff["added"][0]["id"], "radarr-1");
        assert_eq!(diff["removed"][0]["id"], "radarr@4k-1");
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[rstest]
    #[case::no_inputs_yet(KEEP_CHEAP, false, StatusCode::CONFLICT)]
    #[case::unscoped_evict(r#"{"rules": [{"name": "all", "effect": {"type": "must_evict"}}]}"#, true, StatusCode::BAD_REQUEST)]
    #[case::misspelt_scope(
        r#"{"rules": [{"name": "r", "scope": {"tag": ["a"]}, "effect": {"type": "keep"}}]}"#,
        true,
        StatusCode::BAD_REQUEST
    )]
    #[tokio::test]
    async fn a_preview_that_cannot_run_says_why(#[case] body: &'static str, #[case] published: bool, #[case] status: StatusCode) {
        let tmp = scratch("rules-preview-refused");
        let st = state(&tmp, Some(TOKEN));
        if published {
            std::fs::write(st.dir.join(INPUTS_FILE), serde_json::to_vec(&inputs()).expect("json")).expect("inputs");
        }
        let auth = format!("Bearer {TOKEN}");
        let answer = send(&st, request("POST", "/api/rules/preview", &[("authorization", auth.as_str())], Body::from(body))).await;
        assert_eq!(answer.status(), status);
        let error: serde_json::Value = serde_json::from_str(&body_of(answer).await).expect("json");
        assert!(error["error"].as_str().is_some_and(|message| !message.is_empty()));
        std::fs::remove_dir_all(&tmp).ok();
    }
}
