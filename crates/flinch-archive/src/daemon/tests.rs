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

/// A movie two years on disk that Plex dated 200 days ago but never counted
/// as viewed: its watch state and Plex ids, the way the daemon reads them.
fn started_movie() -> (ArrMovie, HashMap<String, watch::WatchEntry>, crate::ids::PlexIds) {
    let movie = ArrMovie {
        id: 1,
        title: "Started One".to_string(),
        year: Some(2020),
        size_on_disk: 5_000_000_000,
        has_file: true,
        movie_file: Some(crate::arr::MovieFile { quality: None, date_added: Some("2025-01-01T00:00:00Z".to_string()) }),
        tmdb_id: Some(1),
        ..Default::default()
    };
    let row = serde_json::json!({
        "ratingKey": "100", "librarySectionID": 1, "title": "Started One", "year": 2020,
        "lastViewedAt": NOW - 200 * 86_400, "Guid": [{"id": "tmdb://1"}],
    });
    let library = crate::plex::PlexLibrary::new(&[serde_json::from_value(row).expect("a Plex movie row")], &[], &[]);
    let card = movie.to_card().expect("on disk");
    let target = crate::plex::WatchTarget { external: movie.external_ids(), ..crate::plex::WatchTarget::from(&card) };
    let resolution = crate::plex::resolve(&[target], &library);
    let health = watch::EvidenceHealth { plex_configured: true, plex_items_ok: true, plex_history_complete: true, ..Default::default() };
    let plex_ids = resolution.plex_ids().remove("radarr-1").expect("resolved by GUID");
    (movie, resolution.item_entries(&health), plex_ids)
}

const DELETE_MOVIES: i64 = 10;
const LEAVING_MOVIES: i64 = 30;

/// A Maintainerr movie collection bound to Plex section 1 whose *arr action
/// deletes the files.
fn movie_collection(id: i64, title: &str, delete_after_days: Option<i64>, visible_on_home: bool) -> crate::maintainerr::CollectionInfo {
    crate::maintainerr::CollectionInfo {
        id,
        title: title.to_string(),
        media_type: "movie".to_string(),
        library_id: "1".to_string(),
        is_active: true,
        arr_action: 1,
        delete_after_days,
        visible_on_home,
        visible_on_recommended: false,
        keep_in_maintainerr_only: false,
        overlay_enabled: false,
        force_seerr: false,
    }
}

/// Hand a cycle's decisions to Maintainerr the way bin/flinch-arrd/handoff.rs
/// does, with every eviction past its grace window and a Leaving Soon
/// collection named and valid. `held` is the collection an earlier cycle
/// already put radarr-1 in. Returns the collections this cycle schedules
/// into, then those it takes a card back out of.
fn hand_over(report: &ReconcileOutput, plex_ids: &crate::ids::PlexIds, held: Option<i64>) -> (Vec<i64>, Vec<i64>) {
    use crate::maintainerr as mx;
    let item = |id: &String| mx::SyncItem {
        card_id: id.clone(),
        kind: crate::card::LibraryKind::Movie,
        plex: Some(plex_ids.clone()),
        copies: Vec::new(),
        bytes: 5_000_000_000,
    };
    let desired = mx::Desired {
        protect: report.kept_ids.iter().chain(&report.reserve_ids).map(item).collect(),
        evict: report.deleted_ids.iter().map(item).collect(),
        announced: report.announced_ids.clone(),
        collections: mx::CollectionTitles {
            movie: "FLINCH Movies".to_string(),
            season: "FLINCH Seasons".to_string(),
            leaving: "Leaving Soon".to_string(),
        },
        gone: BTreeSet::new(),
        seerr_configured: false,
    };
    let mut observed = mx::Observed {
        version: mx::MaintainerrVersion::Release { major: 3, minor: 29, patch: 0 },
        collections: vec![
            movie_collection(DELETE_MOVIES, "FLINCH Movies", None, false),
            movie_collection(LEAVING_MOVIES, "Leaving Soon", Some(14), true),
        ],
        members: [(DELETE_MOVIES, BTreeSet::new()), (LEAVING_MOVIES, BTreeSet::new())].into(),
        exclusions: [(plex_ids.rating_key.clone(), Vec::new())].into(),
    };
    let mut owned = mx::OwnedState::default();
    if let Some(collection_id) = held {
        let target = mx::MaintainerrTarget::from_plex(plex_ids, crate::card::LibraryKind::Movie).expect("a movie key");
        observed.members.entry(collection_id).or_default().insert(target.item_key().to_string());
        owned.scheduled.insert("radarr-1".to_string(), mx::ScheduledEntry { target, collection_id, added_at: NOW - 86_400 });
    }
    let (mut into, mut out_of) = (Vec::new(), Vec::new());
    for action in mx::plan_sync(&desired, &observed, &owned, &mx::Caps::new(10, 100)).actions {
        match action {
            mx::SyncAction::Schedule { collection_id, .. } => into.push(collection_id),
            mx::SyncAction::Unschedule { collection_id, .. } => out_of.push(collection_id),
            _ => {}
        }
    }
    (into, out_of)
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
    let settings =
        RuntimeSettings { collection_leaving: leaving.to_string(), unwatched_reclaim_enabled: true, ..RuntimeSettings::default() };
    assert_eq!(NeverPlayedHold::of(&health, &settings.collection_titles()), hold);

    // Held, neither the operator's switch nor disk pressure arms the rule.
    let mut policy = ArchivePolicy {
        unwatched_reclaim: crate::policy::UnwatchedReclaim { enabled: settings.unwatched_reclaim_enabled, ..Default::default() },
        ..ArchivePolicy::default()
    };
    let governing = hold_never_played(&settings, hold, &mut policy);
    assert_eq!((policy.unwatched_reclaim.enabled, governing.capacity_arm_never_played), (hold.is_none(), hold.is_none()));
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

#[rstest::rstest]
#[case::its_switch(true, false, false, true)]
#[case::while_evicting_on_a_disk_that_evicts(false, true, true, true)]
#[case::while_evicting_with_every_disk_idle(false, true, false, false)]
#[case::neither(false, false, true, false)]
fn the_status_says_whether_the_settings_ask_for_never_played_reclaim(
    #[case] switch: bool,
    #[case] while_evicting: bool,
    #[case] evicting: bool,
    #[case] requested: bool,
) {
    use crate::capacity::CapacityAction;
    // As saved, whatever holds the rule: a blank Leaving Soon title is one.
    let settings = RuntimeSettings {
        unwatched_reclaim_enabled: switch,
        capacity_arm_never_played: while_evicting,
        collection_leaving: String::new(),
        ..RuntimeSettings::default()
    };
    let action = if evicting { CapacityAction::Evict { goal_bytes: 1 << 30, armed_never_played: false } } else { CapacityAction::Idle };
    assert_eq!(never_played_requested(&settings, &action), requested);

    // Held and not asked for, naming Leaving Soon alone frees nothing: the UI
    // reads this to say to enable the rule too. An older status file reads as
    // not asked for.
    let old: StatusSnapshot = serde_json::from_str(
        r#"{"scanned":4,"delete_candidates":0,"kept":4,"reclaimed_bytes":0,"protections_added":0,
            "protections_skipped_repeat":0,"dry_run":true,"ran_at_unix":1}"#,
    )
    .expect("an old status file");
    assert!(!old.never_played_requested);
    let status = serde_json::to_value(StatusSnapshot { never_played_requested: requested, ..old }).expect("serializable");
    assert_eq!(status["never_played_requested"], requested);
}

#[test]
fn a_blank_leaving_soon_title_leaves_the_capacity_goal_to_watched_evictions() {
    use crate::capacity::{App, AppDisks, CapacityAction, Latch, LibraryVolumes, OnDisk, RecycleBin, RootFolder, Volume};
    use crate::maintainerr as mx;
    const GB: u64 = 1_000_000_000;
    // Four years on disk: a large movie Plex saw unplayed, and a smaller one
    // watched 400 days ago. The unplayed one has less regret per byte, so an
    // armed rule would rank it first, and it alone would cover the goal.
    let movie = |id: u32, size_gb: u64| ArrMovie {
        id,
        title: format!("Movie {id}"),
        size_on_disk: size_gb * GB,
        has_file: true,
        movie_file: Some(crate::arr::MovieFile { quality: None, date_added: Some("2022-09-01T00:00:00Z".to_string()) }),
        path: Some(format!("/data/movies/Movie {id}")),
        ..Default::default()
    };
    let movies = [movie(1, 300), movie(2, 120)];
    let seen = |id: &str, progress, last_watched_epoch| {
        let entry =
            watch::WatchEntry { id: id.to_string(), last_watched_epoch, progress, rewatch_score: None, source: watch::WatchSource::Plex };
        (entry.id.clone(), entry)
    };
    let watch = HashMap::from([seen("radarr-1", 0.0, None), seen("radarr-2", 1.0, Some(NOW - 400 * 86_400))]);
    let verdict = |p_safe| ScoreVerdict { p_safe, hard_guard: false, sibling_played: false };
    let verdicts = HashMap::from([("radarr-1".to_string(), verdict(0.85)), ("radarr-2".to_string(), verdict(0.9))]);
    // /data is 85% full against the default 80% ceiling: free 100 GB to reach 75%.
    let library = LibraryVolumes::build(&[AppDisks {
        app: App::Radarr,
        diskspace: vec![Volume { path: "/data".to_string(), total_bytes: 1000 * GB, free_bytes: 150 * GB }],
        root_folders: vec![RootFolder { path: "/data/movies".to_string(), free_bytes: None }],
        recycle: RecycleBin::Disabled,
    }]);
    // The defaults, which arm never-played reclaim while evicting, with the
    // Leaving Soon title cleared; every watch source was read completely.
    let settings = RuntimeSettings { collection_leaving: String::new(), ..RuntimeSettings::default() };
    let titles = settings.collection_titles();
    let health = watch::EvidenceHealth { plex_configured: true, plex_items_ok: true, plex_history_complete: true, ..Default::default() };

    // One cycle the way bin/flinch-arrd/main.rs runs it.
    let mut policy = ArchivePolicy { score_floor: settings.score_floor, ..ArchivePolicy::default() };
    let governing = hold_never_played(&settings, NeverPlayedHold::of(&health, &titles), &mut policy);
    let volume_of = crate::govern::volume_map(&library, &movies, &[]);
    let governance = crate::govern::govern(library, volume_of, |_| true, &governing, &Latch::default(), OnDisk::default(), &mut policy);
    let report = reconcile(&movies, &[], &watch, &BTreeSet::new(), &policy, &verdicts, &governance.goal);

    assert_eq!(report.deleted_ids, ["radarr-2"], "the watched movie covers the goal");
    assert_eq!(report.kept_ids, ["radarr-1"], "the unplayed movie is held, not planned");
    assert!(report.announced_ids.is_empty(), "nothing is headed for a Leaving Soon that cannot announce");
    assert!(report.goal_met);
    assert!(matches!(governance.decision.action, CapacityAction::Evict { armed_never_played: false, .. }));

    // Handed over the way bin/flinch-arrd/handoff.rs does, to the one movie
    // delete collection there is.
    let item = |id: &String| mx::SyncItem {
        card_id: id.clone(),
        kind: crate::card::LibraryKind::Movie,
        plex: Some(crate::ids::PlexIds { rating_key: id.replace("radarr-", "10"), season_rating_key: None, section_id: Some(1) }),
        copies: Vec::new(),
        bytes: movies.iter().find(|movie| format!("radarr-{}", movie.id) == *id).map_or(0, |movie| movie.size_on_disk),
    };
    let desired = mx::Desired {
        protect: report.kept_ids.iter().chain(&report.reserve_ids).map(item).collect(),
        evict: report.deleted_ids.iter().map(item).collect(),
        announced: report.announced_ids.clone(),
        collections: titles.clone(),
        gone: BTreeSet::new(),
        seerr_configured: false,
    };
    let observed = mx::Observed {
        version: mx::MaintainerrVersion::Release { major: 3, minor: 29, patch: 0 },
        collections: vec![movie_collection(DELETE_MOVIES, &titles.movie, None, false)],
        members: [(DELETE_MOVIES, BTreeSet::new())].into(),
        exclusions: [("101".to_string(), Vec::new()), ("102".to_string(), Vec::new())].into(),
    };
    let plan = mx::plan_sync(&desired, &observed, &mx::OwnedState::default(), &mx::Caps::new(10, 500));
    let scheduled: Vec<(&str, i64)> = plan
        .actions
        .iter()
        .filter_map(|action| match action {
            mx::SyncAction::Schedule { card_id, collection_id, .. } => Some((card_id.as_str(), *collection_id)),
            _ => None,
        })
        .collect();
    assert_eq!(scheduled, [("radarr-2", DELETE_MOVIES)], "into its delete collection");
    assert!(plan.blocked.is_empty(), "{:?}", plan.blocked);
}

#[test]
fn a_started_movie_is_announced_or_kept_never_silently_deleted() {
    let (movie, watch, plex_ids) = started_movie();
    let verdicts = HashMap::from([("radarr-1".to_string(), ScoreVerdict { p_safe: 0.99, hard_guard: false, sibling_played: false })]);
    let cycle = |enabled| {
        let policy = ArchivePolicy {
            unwatched_reclaim: crate::policy::UnwatchedReclaim { enabled, ..Default::default() },
            ..ArchivePolicy::default()
        };
        reconcile(std::slice::from_ref(&movie), &[], &watch, &BTreeSet::new(), &policy, &verdicts, &ReclaimGoal::AllSafe)
    };

    // Started but never finished: kept while never-played reclaim is off.
    let unarmed = cycle(false);
    assert_eq!(unarmed.kept_ids, ["radarr-1"]);
    assert!(unarmed.deleted_ids.is_empty(), "never deleted as a watched movie");
    assert_eq!(hand_over(&unarmed, &plex_ids, None), (vec![], vec![]));
    // An earlier cycle read it as watched and scheduled it for deletion.
    assert_eq!(hand_over(&unarmed, &plex_ids, Some(DELETE_MOVIES)), (vec![], vec![DELETE_MOVIES]), "taken back");

    // Armed: it may go, but only announced, through Leaving Soon.
    let armed = cycle(true);
    assert_eq!(armed.deleted_ids, ["radarr-1"]);
    assert_eq!(armed.announced_ids, BTreeSet::from(["radarr-1".to_string()]));
    assert_eq!(
        hand_over(&armed, &plex_ids, None),
        (vec![LEAVING_MOVIES], vec![]),
        "into Leaving Soon, never straight into the delete collection"
    );
    assert_eq!(
        hand_over(&armed, &plex_ids, Some(DELETE_MOVIES)),
        (vec![LEAVING_MOVIES], vec![DELETE_MOVIES]),
        "moved to Leaving Soon, out of the delete collection"
    );
}

#[test]
fn armed_reclaim_takes_a_movie_seen_unplayed_but_keeps_one_without_evidence() {
    // Two large movies, four years on disk. A watch source reported zero
    // playback for one; nothing at all is known about the other.
    let movie = |id: u32, title: &str| ArrMovie {
        id,
        title: title.to_string(),
        size_on_disk: 300_000_000_000,
        has_file: true,
        movie_file: Some(crate::arr::MovieFile { quality: None, date_added: Some("2022-09-01T00:00:00Z".to_string()) }),
        ..Default::default()
    };
    let movies = [movie(8, "Unseen Eight"), movie(9, "Unknown Nine")];
    let unplayed = watch::WatchEntry {
        id: "radarr-8".to_string(),
        last_watched_epoch: None,
        progress: 0.0,
        rewatch_score: None,
        source: watch::WatchSource::Plex,
    };
    let watch = HashMap::from([(unplayed.id.clone(), unplayed)]);
    let policy = ArchivePolicy {
        unwatched_reclaim: crate::policy::UnwatchedReclaim { enabled: true, ..Default::default() },
        ..ArchivePolicy::default()
    };
    let verdict = ScoreVerdict { p_safe: 0.99, hard_guard: false, sibling_played: false };
    let verdicts = HashMap::from([("radarr-8".to_string(), verdict), ("radarr-9".to_string(), verdict)]);

    let report = reconcile(&movies, &[], &watch, &BTreeSet::new(), &policy, &verdicts, &ReclaimGoal::AllSafe);

    assert_eq!(report.deleted_ids, ["radarr-8"]);
    assert_eq!(report.announced_ids, BTreeSet::from(["radarr-8".to_string()]));
    assert_eq!(report.kept_ids, ["radarr-9"], "no watch evidence keeps, however safe the score");
}

#[test]
fn armed_reclaim_takes_a_season_seen_unplayed_but_keeps_one_without_evidence() {
    // Two large seasons, four years on disk, and the newest. A watch source
    // reported zero playback for S1; nothing at all is known about S2.
    let mut series = golden_series(&[300_000_000_000, 300_000_000_000, 1_600_000_000]);
    for season in &mut series[0].seasons[..2] {
        season.files_added = Some("2022-09-01T00:00:00Z".to_string());
    }
    let unplayed = watch::WatchEntry {
        id: "sonarr-5-s1".to_string(),
        last_watched_epoch: None,
        progress: 0.0,
        rewatch_score: None,
        source: watch::WatchSource::Plex,
    };
    let watch = HashMap::from([(unplayed.id.clone(), unplayed)]);
    let policy = ArchivePolicy {
        unwatched_reclaim: crate::policy::UnwatchedReclaim { enabled: true, ..Default::default() },
        ..ArchivePolicy::default()
    };
    let verdict = ScoreVerdict { p_safe: 0.99, hard_guard: false, sibling_played: false };
    let verdicts = HashMap::from([("sonarr-5-s1".to_string(), verdict), ("sonarr-5-s2".to_string(), verdict)]);

    let report = reconcile(&[], &series, &watch, &BTreeSet::new(), &policy, &verdicts, &ReclaimGoal::AllSafe);

    assert_eq!(report.deleted_ids, ["sonarr-5-s1"]);
    assert_eq!(report.announced_ids, BTreeSet::from(["sonarr-5-s1".to_string()]));
    assert_eq!(report.kept_ids, ["sonarr-5-s2", "sonarr-5-s3"], "no watch evidence keeps, however safe the score");
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
