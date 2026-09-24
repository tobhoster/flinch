use super::*;
use crate::plan::VolumeOutcome;
use crate::policy::UnwatchedReclaim;
use proptest::prelude::*;
use rstest::rstest;

const GB: u64 = 1_000_000_000;

fn marks() -> Watermarks {
    Watermarks::new(0.80, 0.75).expect("valid watermarks")
}

fn vol(path: &str, total_gb: u64, used_gb: u64) -> Volume {
    Volume { path: path.to_string(), total_bytes: total_gb * GB, free_bytes: (total_gb - used_gb) * GB }
}

fn disks(app: App, diskspace: Vec<Volume>, roots: &[&str]) -> AppDisks {
    let root_folders = roots.iter().map(|path| RootFolder { path: path.to_string(), free_bytes: None }).collect();
    AppDisks { app, diskspace, root_folders, recycle: RecycleBin::Disabled }
}

fn latch(paths: &[&str]) -> Latch {
    Latch { latched: paths.iter().map(|p| p.to_string()).collect() }
}

fn measured(volumes: &[Volume]) -> CapacitySnapshot {
    CapacitySnapshot::of(volumes, marks()).expect("measured")
}

#[rstest]
#[case::default_band(0.80, 0.75, true)]
#[case::no_hysteresis(0.80, 0.80, true)]
#[case::full_disk_allowed(1.00, 1.00, true)]
#[case::release_above_ceiling_never_releases(0.80, 0.85, false)]
#[case::zero(0.00, 0.00, false)]
#[case::above_one(1.10, 0.75, false)]
#[case::zero_release(0.80, 0.00, false)]
#[case::nan_ceiling(f32::NAN, 0.75, false)]
#[case::nan_release(0.80, f32::NAN, false)]
#[case::infinite(f32::INFINITY, 0.75, false)]
fn watermarks_need_zero_below_release_at_or_below_ceiling_at_or_below_one(
    #[case] ceiling: f32,
    #[case] release: f32,
    #[case] valid: bool,
) {
    assert_eq!(Watermarks::new(ceiling, release).is_some(), valid);
}

#[test]
fn only_mounts_hosting_a_root_folder_are_governed() {
    // The container overlay and the config PVC are nearly full; neither is the
    // library, and deleting media could never relieve them.
    let library = LibraryVolumes::build(&[disks(
        App::Radarr,
        vec![vol("/", 100, 99), vol("/config", 10, 9), vol("/media", 1000, 500)],
        &["/media/movies/"],
    )]);
    let paths: Vec<&str> = library.volumes.iter().map(|v| v.path.as_str()).collect();
    assert_eq!(paths, ["/media"]);
    assert!(library.unmatched_roots.is_empty());
}

#[rstest]
#[case::nested_folder("/media/movies/Film (2020)", Some("/media"))]
#[case::the_mount_itself("/media", Some("/media"))]
#[case::trailing_separator("/media/", Some("/media"))]
#[case::sibling_prefix_is_not_a_child("/media2/Film", None)]
#[case::outside_every_library_mount("/downloads/Film", None)]
fn attribution_matches_whole_path_components(#[case] item: &str, #[case] expected: Option<&str>) {
    let library = LibraryVolumes::build(&[disks(
        App::Radarr,
        vec![vol("/", 100, 10), vol("/media", 1000, 500), vol("/media2", 1000, 10)],
        &["/media/movies"],
    )]);
    assert_eq!(library.volume_of(App::Radarr, item), expected);
}

#[test]
fn windows_paths_attribute_by_backslash_components() {
    let library = LibraryVolumes::build(&[disks(App::Radarr, vec![vol("D:\\Media", 1000, 500)], &["D:\\Media\\Movies\\"])]);
    assert_eq!(library.volume_of(App::Radarr, "D:\\Media\\Movies\\Film"), Some("D:\\Media"));
    assert_eq!(library.volume_of(App::Radarr, "D:\\MediaX\\Film"), None);
}

#[test]
fn attribution_is_per_app_because_mount_paths_are_container_local() {
    let library = LibraryVolumes::build(&[disks(App::Radarr, vec![vol("/data", 1000, 500)], &["/data/movies"])]);
    assert_eq!(library.volume_of(App::Sonarr, "/data/tv/Show"), None, "Sonarr's /data is another container's path");
}

#[test]
fn a_share_mounted_at_different_paths_is_one_filesystem() {
    // Radarr sees the share at /movies, Sonarr at /tv, sampled seconds apart.
    let sonarr_view = Volume { path: "/tv".to_string(), total_bytes: 8000 * GB, free_bytes: 997 * GB };
    let library = LibraryVolumes::build(&[
        disks(App::Radarr, vec![vol("/movies", 8000, 7000)], &["/movies"]),
        disks(App::Sonarr, vec![sonarr_view], &["/tv"]),
    ]);
    assert_eq!(library.volumes.len(), 1, "one disk must mean one goal, never a doubled eviction");
    assert_eq!(library.volume_of(App::Sonarr, "/tv/Show"), Some("/movies"));
    assert_eq!(library.volume_of(App::Radarr, "/movies/Film"), Some("/movies"));
}

#[test]
fn distinct_filesystems_at_one_path_get_distinct_keys() {
    let library = LibraryVolumes::build(&[
        disks(App::Radarr, vec![vol("/media", 4000, 1000)], &["/media/movies"]),
        disks(App::Sonarr, vec![vol("/media", 8000, 7000)], &["/media/tv"]),
    ]);
    let keys: Vec<&str> = library.volumes.iter().map(|v| v.path.as_str()).collect();
    assert_eq!(keys, ["/media", "/media (sonarr)"]);
    assert_eq!(library.volume_of(App::Sonarr, "/media/tv/Show"), Some("/media (sonarr)"));
    assert_eq!(library.volume_of(App::Radarr, "/media/movies/Film"), Some("/media"));
}

#[test]
fn a_root_no_mount_holds_is_reported_not_guessed() {
    let library = LibraryVolumes::build(&[disks(App::Sonarr, Vec::new(), &["/tv"])]);
    assert!(library.volumes.is_empty());
    assert_eq!(library.unmatched_roots, [(App::Sonarr, "/tv".to_string())]);
}

#[test]
fn a_root_whose_free_space_contradicts_its_mount_is_on_an_unreported_disk() {
    // Live shape: Sonarr lists only `/` and `/config`, so by prefix its media
    // roots fall through to the container's root filesystem; each root's own
    // free space shows it lives elsewhere. Radarr's root agrees with `/data`.
    let root = |path: &str, free_gb: u64| RootFolder { path: path.to_string(), free_bytes: Some(free_gb * GB) };
    let app = |app, diskspace, root_folders| AppDisks { app, diskspace, root_folders, recycle: RecycleBin::Disabled };
    let library = LibraryVolumes::build(&[
        app(App::Radarr, vec![vol("/", 510, 57), vol("/data", 834, 460)], vec![root("/data/media/movies", 374)]),
        app(App::Sonarr, vec![vol("/", 510, 57), vol("/config", 5, 1)], vec![root("/data/media/tv", 191), root("/data/media/anime", 374)]),
    ]);
    let paths: Vec<&str> = library.volumes.iter().map(|v| v.path.as_str()).collect();
    assert_eq!(paths, ["/data"]);
    assert_eq!(
        library.unmatched_roots,
        [(App::Sonarr, "/data/media/tv".to_string()), (App::Sonarr, "/data/media/anime".to_string())]
    );
    assert_eq!(library.volume_of(App::Sonarr, "/data/media/tv/Andor"), None, "never governed against `/`");
}

#[test]
fn no_library_volume_is_unmeasured() {
    assert_eq!(CapacitySnapshot::of(&[], marks()), None);
}

#[test]
fn measures_are_per_volume_so_a_quiet_neighbour_cannot_mask_a_full_disk() {
    let snap = measured(&[vol("/movies", 1000, 900), vol("/tv", 1000, 100)]);
    assert!(snap.over_ceiling);
    assert_eq!(snap.utilization, 0.5, "pooled, the store looks half empty");
    assert_eq!(snap.deficit_bytes, 100 * GB);
    assert_eq!(snap.release_gap_bytes, 150 * GB);
}

#[rstest]
#[case::idle_far_under(50, false, false)]
#[case::releases_once_under_the_mark(70, true, false)]
#[case::releases_exactly_at_the_mark(75, true, false)]
#[case::idle_inside_the_band(78, false, false)]
#[case::keeps_evicting_inside_the_band(78, true, true)]
#[case::exactly_at_the_ceiling_is_not_over(80, false, false)]
#[case::latches_over_the_ceiling(85, false, true)]
#[case::stays_latched_over_the_ceiling(85, true, true)]
fn eviction_latches_at_the_ceiling_and_releases_at_the_mark(
    #[case] used_gb: u64,
    #[case] was_latched: bool,
    #[case] evicting: bool,
) {
    let snap = measured(&[vol("/media", 100, used_gb)]);
    let before = if was_latched { latch(&["/media"]) } else { Latch::default() };
    let mut policy = ArchivePolicy::default();
    let decision = decide_capacity(&mut policy, Some(&snap), &before, false, &BTreeMap::new());
    assert_eq!(decision.latch.latched.contains("/media"), evicting);
    match decision.action {
        CapacityAction::Evict { goal_bytes, .. } => {
            assert!(evicting);
            assert_eq!(goal_bytes, (used_gb - 75) * GB, "free down to the release mark");
        }
        CapacityAction::Idle => assert!(!evicting),
        CapacityAction::Unmeasured => panic!("a measured snapshot is never unmeasured"),
    }
}

#[test]
fn goals_are_per_volume() {
    let snap = measured(&[vol("/movies", 100, 90), vol("/tv", 100, 50)]);
    let decision = decide_capacity(&mut ArchivePolicy::default(), Some(&snap), &Latch::default(), false, &BTreeMap::new());
    assert_eq!(decision.goals, BTreeMap::from([("/movies".to_string(), 15 * GB)]));
}

#[test]
fn unmeasured_evicts_nothing_and_keeps_the_latch() {
    let before = latch(&["/media"]);
    let mut policy = ArchivePolicy::default();
    let decision = decide_capacity(&mut policy, None, &before, true, &BTreeMap::new());
    assert_eq!(decision.action, CapacityAction::Unmeasured);
    assert_eq!(decision.latch, before, "no measurement, no release");
    assert!(decision.goals.is_empty());
    assert!(!policy.unwatched_reclaim.enabled, "no pressure without a measurement");
}

#[test]
fn a_latched_volume_that_vanished_is_released() {
    let snap = measured(&[vol("/tv", 100, 50)]);
    let decision = decide_capacity(&mut ArchivePolicy::default(), Some(&snap), &latch(&["/movies"]), false, &BTreeMap::new());
    assert_eq!(decision.action, CapacityAction::Idle);
    assert!(decision.latch.latched.is_empty());
}

#[rstest]
#[case::armed(true)]
#[case::operator_said_no(false)]
fn eviction_arms_never_played_only_when_permitted_and_never_lowers_its_floor(#[case] arm: bool) {
    let snap = measured(&[vol("/media", 100, 90)]);
    let mut policy = ArchivePolicy::default();
    let decision = decide_capacity(&mut policy, Some(&snap), &Latch::default(), arm, &BTreeMap::new());
    assert_eq!(policy.unwatched_reclaim.enabled, arm);
    assert_eq!(policy.unwatched_reclaim.floor, UnwatchedReclaim::default().floor);
    assert!(matches!(decision.action, CapacityAction::Evict { armed_never_played, .. } if armed_never_played == arm));
}

#[test]
fn an_idle_run_never_touches_the_operators_always_on_rule() {
    let snap = measured(&[vol("/media", 100, 50)]);
    let mut policy = ArchivePolicy {
        unwatched_reclaim: UnwatchedReclaim { enabled: true, ..UnwatchedReclaim::default() },
        ..ArchivePolicy::default()
    };
    decide_capacity(&mut policy, Some(&snap), &Latch::default(), false, &BTreeMap::new());
    assert!(policy.unwatched_reclaim.enabled);
}

fn outcome(volume: &str, goal_gb: u64, reclaimed_gb: u64, eligible_gb: u64) -> VolumeOutcome {
    VolumeOutcome {
        volume: volume.to_string(),
        goal_bytes: goal_gb * GB,
        reclaimed_bytes: reclaimed_gb * GB,
        eligible_bytes: eligible_gb * GB,
    }
}

#[rstest]
#[case::idle(60, 0, 0, None, None)]
#[case::handed_over(90, 15, 15, Some(true), Some(true))]
#[case::covered_but_paced_by_the_caps(90, 15, 5, Some(true), Some(false))]
#[case::short(90, 5, 5, Some(false), Some(false))]
fn a_goal_is_claimed_only_while_evicting_and_met_only_once_handed_over(
    #[case] used_gb: u64,
    #[case] reclaimed_gb: u64,
    #[case] handed_gb: u64,
    #[case] covered: Option<bool>,
    #[case] met: Option<bool>,
) {
    let snap = measured(&[vol("/media", 100, used_gb)]);
    let decision = decide_capacity(&mut ArchivePolicy::default(), Some(&snap), &Latch::default(), false, &BTreeMap::new());
    let outcomes = [outcome("/media", used_gb.saturating_sub(75), reclaimed_gb, 40)];
    let handed = BTreeMap::from([("/media".to_string(), handed_gb * GB)]);
    let status = CapacityStatus::new(&snap, &decision, &outcomes, &[], &BTreeMap::new(), &handed);
    assert_eq!((status.covered, status.goal_met), (covered, met));
    assert_eq!((status.volumes[0].covered, status.volumes[0].goal_met), (covered, met));
    assert_eq!(status.latched, met.is_some());
    assert_eq!(status.volumes[0].eligible_bytes, 40 * GB);
    assert_eq!(status.ceiling, 0.80);
}

#[test]
fn status_names_every_ungoverned_root() {
    let snap = measured(&[vol("/media", 100, 50)]);
    let decision = decide_capacity(&mut ArchivePolicy::default(), Some(&snap), &Latch::default(), false, &BTreeMap::new());
    let status = CapacityStatus::new(&snap, &decision, &[], &[(App::Sonarr, "/anime".to_string())], &BTreeMap::new(), &BTreeMap::new());
    assert_eq!(status.unmatched_roots, ["sonarr:/anime"]);
}

#[test]
fn the_latch_round_trips_and_a_corrupt_file_reads_unlatched() {
    let path = std::env::temp_dir().join(format!("flinch-latch-{}.json", std::process::id()));
    let before = latch(&["/movies", "/tv"]);
    write_latch(&path, &before).expect("write latch");
    assert_eq!(read_latch(&path), before);
    std::fs::write(&path, b"{not json").expect("corrupt it");
    assert_eq!(read_latch(&path), Latch::default(), "corruption must delete less, not more");
    std::fs::remove_file(&path).ok();
}

proptest! {
    #[test]
    fn freeing_the_goal_lands_at_or_under_the_release_mark_without_overshoot(
        total_gb in 1u64..=100_000,
        used_permille in 0u64..=1000,
        a_pct in 1u32..=100,
        b_pct in 1u32..=100,
    ) {
        let (ceiling_pct, release_pct) = (a_pct.max(b_pct), a_pct.min(b_pct));
        let marks = Watermarks::new(ceiling_pct as f32 / 100.0, release_pct as f32 / 100.0).expect("valid");
        let total = total_gb * GB;
        let used = total / 1000 * used_permille;
        let volume = Volume { path: "/m".to_string(), total_bytes: total, free_bytes: total - used };
        let snap = CapacitySnapshot::of(&[volume], marks).expect("measured");
        let m = &snap.volumes[0];
        let release_line = total as f64 * f64::from(release_pct) / 100.0;

        prop_assert!((used - m.release_gap_bytes) as f64 <= release_line + 1.0);
        if m.release_gap_bytes > 0 {
            prop_assert!((used - m.release_gap_bytes) as f64 >= release_line - 1.0, "no byte evicted beyond the mark");
        }
        prop_assert!(m.deficit_bytes <= m.release_gap_bytes, "the ceiling sits above the release mark");
        prop_assert_eq!(m.over_ceiling, m.deficit_bytes > 0);
        prop_assert!((0.0..=1.0).contains(&m.utilization));
    }

    #[test]
    fn a_fuller_disk_never_needs_less_eviction(total_gb in 1u64..=100_000, a in 0u64..=1000, b in 0u64..=1000) {
        let total = total_gb * GB;
        let at = |permille: u64| {
            let used = total / 1000 * permille;
            measured(&[Volume { path: "/m".to_string(), total_bytes: total, free_bytes: total - used }])
        };
        let (low, high) = (at(a.min(b)), at(a.max(b)));
        prop_assert!(high.release_gap_bytes >= low.release_gap_bytes);
        prop_assert!(high.deficit_bytes >= low.deficit_bytes);
    }

    #[test]
    fn the_latch_holds_exactly_from_crossing_until_release(
        total_gb in 1u64..=100_000,
        used_permille in 0u64..=1000,
        was_latched in any::<bool>(),
    ) {
        let total = total_gb * GB;
        let used = total / 1000 * used_permille;
        let snap = measured(&[Volume { path: "/m".to_string(), total_bytes: total, free_bytes: total - used }]);
        let before = if was_latched { latch(&["/m"]) } else { Latch::default() };
        let decision = decide_capacity(&mut ArchivePolicy::default(), Some(&snap), &before, false, &BTreeMap::new());
        let m = &snap.volumes[0];
        let latched = m.over_ceiling || (was_latched && m.release_gap_bytes > 0);
        prop_assert_eq!(decision.latch.latched.contains("/m"), latched);
        prop_assert_eq!(decision.goals.get("/m").copied(), latched.then_some(m.release_gap_bytes));
    }

    #[test]
    fn every_item_under_a_root_lands_on_a_governed_volume(
        depth in 1usize..4,
        names in prop::collection::vec("[a-z]{1,8}", 4),
    ) {
        let root = format!("/media/{}", names[..depth].join("/"));
        let item = format!("{root}/{}", names[3]);
        let library = LibraryVolumes::build(&[disks(
            App::Sonarr,
            vec![vol("/", 100, 10), vol("/media", 1000, 500), vol("/config", 10, 1)],
            &[root.as_str()],
        )]);
        let key = library.volume_of(App::Sonarr, &item);
        prop_assert_eq!(key, Some("/media"));
        prop_assert!(library.volumes.iter().any(|v| Some(v.path.as_str()) == key));
    }
}

#[test]
fn evicted_bytes_in_a_recycle_bin_are_credited_not_evicted_twice() {
    let snap = measured(&[vol("/media", 100, 90)]);
    let pending = BTreeMap::from([("/media".to_string(), 10 * GB)]);
    let decision = decide_capacity(&mut ArchivePolicy::default(), Some(&snap), &Latch::default(), false, &pending);
    assert_eq!(decision.goals.get("/media"), Some(&(5 * GB)), "15 GB gap, 10 GB already on its way out");
}

#[test]
fn a_volume_waiting_on_its_recycle_bin_stays_latched_with_nothing_more_to_evict() {
    let snap = measured(&[vol("/media", 100, 90)]);
    let pending = BTreeMap::from([("/media".to_string(), 40 * GB)]);
    let decision = decide_capacity(&mut ArchivePolicy::default(), Some(&snap), &Latch::default(), false, &pending);
    assert_eq!(decision.goals.get("/media"), Some(&0));
    assert!(decision.latch.latched.contains("/media"), "latched on the measurement, never on the credit");
}

#[rstest]
#[case::disabled("", 7, RecycleBin::Disabled, 0)]
#[case::whitespace_path_is_disabled("  ", 3, RecycleBin::Disabled, 0)]
#[case::days("/data/.RecycleBin", 3, RecycleBin::Days(3), 3 * 86_400)]
#[case::never_emptied("/data/.RecycleBin", 0, RecycleBin::NeverEmptied, STALE_ON_DISK_SECS)]
fn recycle_bin_settings_decide_how_long_a_delete_holds_space(
    #[case] path: &str,
    #[case] cleanup_days: u32,
    #[case] bin: RecycleBin,
    #[case] hold_secs: u64,
) {
    assert_eq!(RecycleBin::from_settings(path, cleanup_days), bin);
    assert_eq!(bin.hold_secs(), hold_secs);
}

#[test]
fn an_app_that_never_answered_holds_space_for_the_default_week() {
    let library = LibraryVolumes::build(&[]);
    assert_eq!(library.recycle_bin(App::Radarr), RecycleBin::Unknown);
    assert_eq!(library.recycle_secs(App::Radarr), 7 * 86_400);
}

proptest! {
    #[test]
    fn credit_lowers_a_goal_by_exactly_the_pending_bytes_and_never_moves_the_latch(
        total_gb in 1u64..=10_000,
        used_permille in 801u64..=1000,
        pending_gb in 0u64..=10_000,
    ) {
        let total = total_gb * GB;
        let used = total / 1000 * used_permille;
        let snap = measured(&[Volume { path: "/m".to_string(), total_bytes: total, free_bytes: total - used }]);
        let pending = BTreeMap::from([("/m".to_string(), pending_gb * GB)]);
        let without = decide_capacity(&mut ArchivePolicy::default(), Some(&snap), &Latch::default(), false, &BTreeMap::new());
        let with = decide_capacity(&mut ArchivePolicy::default(), Some(&snap), &Latch::default(), false, &pending);
        prop_assert_eq!(with.goals["/m"], without.goals["/m"].saturating_sub(pending_gb * GB));
        prop_assert_eq!(with.latch, without.latch);
    }
}
