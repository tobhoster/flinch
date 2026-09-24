use super::*;

/// The fixed clock `watch` uses under test.
const NOW: u64 = 1_800_000_000;

fn season(season_number: u32, size_on_disk: u64) -> crate::arr::SeriesSeason {
    crate::arr::SeriesSeason {
        season_number,
        statistics: crate::arr::SeasonStats { episode_file_count: 1, episode_count: 1, total_episode_count: 1, size_on_disk },
        ..Default::default()
    }
}

/// On-disk seasons of one show; the last one is the newest and always kept.
fn golden_series(sizes: &[u64]) -> Vec<ArrSeries> {
    vec![ArrSeries {
        id: 5,
        title: "Golden Five".to_string(),
        year: Some(2021),
        series_type: "standard".to_string(),
        added: Some("2024-01-01T00:00:00Z".to_string()),
        seasons: sizes.iter().zip(1..).map(|(size, number)| season(number, *size)).collect(),
        path: Some("/media/tv/Golden Five".to_string()),
        status: Some("continuing".to_string()),
        ..Default::default()
    }]
}

/// Completed, and last watched 400 days ago: reclaimable.
fn completed_cold(ids: &[&str]) -> HashMap<String, watch::WatchEntry> {
    ids.iter()
        .map(|id| {
            let entry = watch::WatchEntry {
                id: id.to_string(),
                last_watched_epoch: Some(NOW - 400 * 86_400),
                progress: 1.0,
                rewatch_score: None,
                source: watch::WatchSource::Export,
            };
            (id.to_string(), entry)
        })
        .collect()
}

fn run(series: &[ArrSeries], watch: &HashMap<String, watch::WatchEntry>, keeps: &[&str], goal: ReclaimGoal) -> ReconcileOutput {
    let keeps: BTreeSet<String> = keeps.iter().map(|id| id.to_string()).collect();
    reconcile(&[], series, watch, &keeps, &ArchivePolicy::default(), &HashMap::new(), &goal)
}

#[test]
fn a_completed_cold_season_is_evicted_and_the_newest_is_kept() {
    let report = run(&golden_series(&[1_500_000_000, 1_600_000_000]), &completed_cold(&["sonarr-5-s1"]), &[], ReclaimGoal::AllSafe);

    assert_eq!(report.scanned, 2);
    assert_eq!(report.deleted_ids, ["sonarr-5-s1"]);
    assert_eq!(report.kept_ids, ["sonarr-5-s2"], "the newest season fails closed and is kept");
    assert!(report.reserve_ids.is_empty());
    assert_eq!(report.kept, 1);
    assert!(report.reclaimed_bytes >= 1_500_000_000);
}

#[test]
fn missing_watch_state_fails_closed_so_every_season_is_kept() {
    let report = run(&golden_series(&[1_500_000_000, 1_600_000_000]), &HashMap::new(), &[], ReclaimGoal::AllSafe);

    assert_eq!(report.delete_candidates, 0, "without media-server truth nothing is evicted");
    assert_eq!(report.kept_ids, ["sonarr-5-s1", "sonarr-5-s2"]);
}

#[test]
fn an_eligible_season_the_goal_does_not_need_is_reserve_not_kept() {
    let series = golden_series(&[1_500_000_000, 1_600_000_000, 1_700_000_000]);
    let report = run(&series, &completed_cold(&["sonarr-5-s1", "sonarr-5-s2"]), &[], ReclaimGoal::Bytes(1));

    assert_eq!(report.deleted_ids.len(), 1, "one season covers a one-byte goal");
    assert_eq!(report.reserve_ids.len(), 1);
    let mut eligible: Vec<&String> = report.deleted_ids.iter().chain(&report.reserve_ids).collect();
    eligible.sort();
    assert_eq!(eligible, ["sonarr-5-s1", "sonarr-5-s2"]);
    assert_eq!(report.kept_ids, ["sonarr-5-s3"], "reserve is not protected");
    assert_eq!(report.kept, 2);
}

#[test]
fn an_operator_keep_is_a_hard_guard_even_when_the_season_is_reclaimable() {
    let series = golden_series(&[1_500_000_000, 1_600_000_000]);
    let report = run(&series, &completed_cold(&["sonarr-5-s1"]), &["sonarr-5-s1"], ReclaimGoal::AllSafe);

    assert_eq!(report.delete_candidates, 0);
    assert_eq!(report.kept_ids, ["sonarr-5-s1", "sonarr-5-s2"], "kept, not reserve: the operator's exclusion wins");
}

#[test]
fn only_the_eviction_nobody_finished_is_announced() {
    // Golden Five's S1 was finished long ago; Unseen Six's S1 was never played.
    let mut series = golden_series(&[1_500_000_000, 1_600_000_000]);
    let mut unseen = series[0].clone();
    (unseen.id, unseen.title) = (6, "Unseen Six".to_string());
    // Dwell counts from the files' arrival: two years on disk, never played.
    unseen.seasons[0].files_added = Some("2024-09-01T00:00:00Z".to_string());
    series.push(unseen);
    let mut watch = completed_cold(&["sonarr-5-s1"]);
    let never = watch::WatchEntry {
        id: "sonarr-6-s1".to_string(),
        last_watched_epoch: None,
        progress: 0.0,
        rewatch_score: None,
        source: watch::WatchSource::Export,
    };
    watch.insert(never.id.clone(), never);
    let policy = ArchivePolicy {
        unwatched_reclaim: crate::policy::UnwatchedReclaim { enabled: true, ..Default::default() },
        ..ArchivePolicy::default()
    };
    let verdict = ScoreVerdict { p_safe: 0.99, hard_guard: false, sibling_played: false };
    let verdicts = HashMap::from([("sonarr-6-s1".to_string(), verdict)]);

    let report = reconcile(&[], &series, &watch, &BTreeSet::new(), &policy, &verdicts, &ReclaimGoal::AllSafe);

    let mut deleted = report.deleted_ids.clone();
    deleted.sort();
    assert_eq!(deleted, ["sonarr-5-s1", "sonarr-6-s1"]);
    assert_eq!(report.announced_ids, BTreeSet::from(["sonarr-6-s1".to_string()]));
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
