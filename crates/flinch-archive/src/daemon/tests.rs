use super::*;
use crate::capacity::{CapacityForecast, VolumeForecast};
use crate::plan::{Exclusion, MediaCandidate, Pin};
use crate::regret::Regret;

const GIB: u64 = 1 << 30;

fn candidate(id: &str, regret: f64) -> MediaCandidate {
    MediaCandidate {
        id: id.to_string(),
        title: id.to_string(),
        size_bytes: 10 * GIB,
        volume: Some("disk".to_string()),
        regret: Regret::new(regret, 1.0, 1.0),
        reason: String::new(),
        age_days: 400.0,
        exclusion: None,
        sequence: None,
        handed: false,
        announce: false,
        protect: false,
        quality: crate::quality::advise(&Regret::new(regret, 1.0, 1.0), &crate::quality::Item::default()),
        eviction_safety: 0.0,
    }
}

fn needs(gib: u64) -> Vec<VolumeForecast> {
    vec![VolumeForecast {
        volume: "disk".to_string(),
        forecast: CapacityForecast {
            current_used_bytes: 0,
            max_capacity_bytes: 1,
            current_utilization: 0.0,
            daily_ingest_rate_bytes: 0,
            queue_bytes: 0,
            in_flight_bytes: 0,
            projected_used_bytes: 0,
            target_reclaim_bytes: gib * GIB,
            is_emergency: false,
        },
    }]
}

#[test]
fn an_eviction_outranks_a_partway_protection_and_only_unfinished_items_are_announced() {
    let pinned = MediaCandidate { exclusion: Some(Exclusion::Pinned(Pin::Favorite)), protect: true, ..candidate("pinned", 0.0) };
    let partway_taken = MediaCandidate { protect: true, announce: true, ..candidate("partway-taken", 0.1) };
    let partway_kept = MediaCandidate { protect: true, ..candidate("partway-kept", 9.0) };
    let finished = candidate("finished", 0.2);
    let report = reconcile(&[pinned, partway_taken, partway_kept, finished], &needs(20), &Default::default()).expect("plans");

    let mut deleted = report.deleted_ids.clone();
    deleted.sort();
    assert_eq!(deleted, ["finished", "partway-taken"]);
    assert_eq!(report.announced_ids, BTreeSet::from(["partway-taken".to_string()]));
    assert_eq!(report.protected_ids, ["pinned", "partway-kept"], "never protect what the plan takes");
}

#[rstest::rstest]
#[case::named_with_complete_evidence("Leaving Soon", true, None)]
#[case::a_blank_title("", true, Some(NeverPlayedHold::LeavingSoonUntitled))]
#[case::a_title_of_spaces("  ", true, Some(NeverPlayedHold::LeavingSoonUntitled))]
#[case::incomplete_evidence("Leaving Soon", false, Some(NeverPlayedHold::IncompleteEvidence))]
#[case::incomplete_evidence_is_named_first("", false, Some(NeverPlayedHold::IncompleteEvidence))]
fn never_played_reclaim_needs_complete_evidence_and_a_leaving_soon_title(
    #[case] leaving: &str,
    #[case] complete: bool,
    #[case] hold: Option<NeverPlayedHold>,
) {
    let health =
        watch::EvidenceHealth { plex_configured: true, plex_items_ok: true, plex_history_complete: complete, ..Default::default() };
    let settings = RuntimeSettings { collection_leaving: leaving.to_string(), ..RuntimeSettings::default() };
    assert_eq!(NeverPlayedHold::of(&health, &settings.collection_titles()), hold);
}

#[rstest::rstest]
#[case::a_blank_title(Some(NeverPlayedHold::LeavingSoonUntitled), "\"leaving_soon_untitled\"")]
#[case::incomplete_evidence(Some(NeverPlayedHold::IncompleteEvidence), "\"incomplete_evidence\"")]
#[case::nothing_holds_it(None, "null")]
fn the_status_names_the_never_played_hold_for_the_ui(#[case] hold: Option<NeverPlayedHold>, #[case] published: &str) {
    // A status file written before the hold was published still loads.
    let old: StatusSnapshot = serde_json::from_str(
        r#"{"scanned":4,"delete_candidates":0,"kept":4,"reclaimed_bytes":0,"protections_added":0,
            "protections_skipped_repeat":0,"dry_run":true,"ran_at_unix":1}"#,
    )
    .expect("an old status file");
    assert_eq!(old.never_played_hold, None);

    // The UI reads these words to say what would lift the hold.
    let status = serde_json::to_value(StatusSnapshot { never_played_hold: hold, ..old }).expect("serializable");
    assert_eq!(status["never_played_hold"].to_string(), published);
    let read: StatusSnapshot = serde_json::from_value(status).expect("round trip");
    assert_eq!(read.never_played_hold, hold);
}

/// The run (by index) at which one steady candidate first becomes eligible.
fn first_eligible(runs: u32, at: &[u64]) -> Option<usize> {
    let mut state = CandidateState::default();
    let candidates = ["radarr-1".to_string()];
    let grace = Grace { runs, interval_s: 300 };
    at.iter().position(|now| !advance_streaks(&mut state, &candidates, grace, *now).is_empty())
}

#[rstest::rstest]
#[case::the_normal_cadence_hands_over_on_the_second_run(2, &[0, 300], Some(1))]
#[case::three_runs_need_two_intervals(3, &[0, 300, 600], Some(2))]
#[case::one_run_needs_no_wait(1, &[0], Some(0))]
#[case::triggered_runs_cannot_shorten_the_window(2, &[0, 60, 120, 180, 240], None)]
#[case::a_triggered_run_counts_once_the_interval_has_passed(3, &[0, 60, 600], Some(2))]
fn grace_needs_both_the_runs_and_the_time_they_take(#[case] runs: u32, #[case] at: &[u64], #[case] eligible: Option<usize>) {
    assert_eq!(first_eligible(runs, at), eligible);
}

#[test]
fn a_streak_saved_before_its_start_was_recorded_waits_a_full_interval() {
    let mut state: CandidateState =
        serde_json::from_str(r#"{"streaks":{"radarr-1":5},"last_run_unix":0}"#).expect("the older file still reads");
    let candidates = ["radarr-1".to_string()];
    let grace = Grace { runs: 2, interval_s: 300 };
    assert!(advance_streaks(&mut state, &candidates, grace, 1_000).is_empty(), "no start on file: the clock starts now");
    assert_eq!(advance_streaks(&mut state, &candidates, grace, 1_300), candidates);
}
