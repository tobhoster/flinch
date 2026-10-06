use super::*;
use crate::plan::{EvictionPlan, VolumeOutcome};
use proptest::prelude::*;
use rstest::rstest;

const GB: u64 = 1_000_000_000;
const DAY: u64 = 86_400;

fn vol(path: &str, total_gb: u64, used_gb: u64) -> Volume {
    Volume { path: path.to_string(), total_bytes: total_gb * GB, free_bytes: (total_gb - used_gb) * GB }
}

fn disks(app: App, diskspace: Vec<Volume>, roots: &[&str]) -> AppDisks {
    let root_folders = roots.iter().map(|path| RootFolder { path: path.to_string(), free_bytes: None }).collect();
    AppDisks { app, diskspace, root_folders, recycle: RecycleBin::Disabled }
}

/// Target 80%, 10-day window, no headroom: round numbers to reason about.
fn forecaster(headroom_gb: u64) -> SlidingWindowCapacityForecaster {
    SlidingWindowCapacityForecaster::new(CapacityConfig {
        sliding_window_days: 10,
        headroom_buffer_bytes: headroom_gb * GB,
        ..CapacityConfig::default()
    })
    .expect("valid config")
}

fn load(total_gb: u64, used_gb: u64, daily: &[u64]) -> VolumeLoad<'_> {
    VolumeLoad { total_bytes: total_gb * GB, used_bytes: used_gb * GB, daily_ingest: daily, queue_bytes: 0, in_flight_bytes: 0 }
}

#[test]
fn flat_growth_under_the_target_needs_nothing() {
    let quiet = [0u64; INGEST_HISTORY_DAYS];
    let forecast = forecaster(0).forecast(&load(1000, 700, &quiet)).expect("measured");
    assert_eq!(forecast.daily_ingest_rate_bytes, 0);
    assert_eq!(forecast.projected_used_bytes, 700 * GB);
    assert_eq!(forecast.target_reclaim_bytes, 0);
    assert!(!forecast.is_emergency);
}

#[test]
fn steady_ingest_projects_the_window_ahead() {
    // 10 GB a day for 10 days lands 100 GB on a disk at 750 of 1000: 50 GB
    // over the 800 GB target.
    let steady = [10 * GB; INGEST_HISTORY_DAYS];
    let forecast = forecaster(0).forecast(&load(1000, 750, &steady)).expect("measured");
    assert_eq!(forecast.daily_ingest_rate_bytes, 10 * GB);
    assert_eq!(forecast.projected_used_bytes, 850 * GB);
    assert_eq!(forecast.target_reclaim_bytes, 50 * GB);
}

#[test]
fn a_spike_ramps_the_target_by_alpha_of_it_not_all_of_it() {
    let mut history = [0u64; INGEST_HISTORY_DAYS];
    let quiet = forecaster(0).forecast(&load(1000, 790, &history)).expect("measured");
    history[INGEST_HISTORY_DAYS - 1] = 100 * GB;
    let spiked = forecaster(0).forecast(&load(1000, 790, &history)).expect("measured");
    assert_eq!(quiet.target_reclaim_bytes, 0);
    assert_eq!(spiked.daily_ingest_rate_bytes, 20 * GB, "α = 0.2 of one 100 GB day");
    assert_eq!(spiked.target_reclaim_bytes, 190 * GB, "790 + 20·10 − 800");
}

#[test]
fn rising_ingest_raises_the_target_day_by_day() {
    let mut targets = Vec::new();
    for days in 0..=INGEST_HISTORY_DAYS {
        let mut history = [0u64; INGEST_HISTORY_DAYS];
        history[INGEST_HISTORY_DAYS - days..].fill(20 * GB);
        targets.push(forecaster(0).forecast(&load(1000, 700, &history)).expect("measured").target_reclaim_bytes);
    }
    assert!(targets.windows(2).all(|pair| pair[1] >= pair[0]), "{targets:?}");
    assert_eq!(targets[0], 0);
    assert!(targets[INGEST_HISTORY_DAYS] > 0);
}

#[test]
fn headroom_queue_and_in_flight_evictions_move_the_target() {
    let quiet = [0u64; INGEST_HISTORY_DAYS];
    let at = |headroom: u64, queue: u64, in_flight: u64| {
        let load = VolumeLoad { queue_bytes: queue * GB, in_flight_bytes: in_flight * GB, ..load(1000, 790, &quiet) };
        forecaster(headroom).forecast(&load).expect("measured").target_reclaim_bytes / GB
    };
    assert_eq!(at(0, 0, 0), 0);
    assert_eq!(at(50, 0, 0), 40, "790 − 800 + 50");
    assert_eq!(at(0, 30, 0), 20, "queued downloads land in the window");
    assert_eq!(at(0, 30, 25), 0, "evictions already on their way out are not evicted twice");
}

#[rstest]
#[case::below(940, false)]
#[case::at_the_mark(950, true)]
#[case::over(990, true)]
fn emergency_is_current_usage_at_or_over_the_emergency_ratio(#[case] used_gb: u64, #[case] emergency: bool) {
    let quiet = [0u64; INGEST_HISTORY_DAYS];
    assert_eq!(forecaster(0).forecast(&load(1000, used_gb, &quiet)).expect("measured").is_emergency, emergency);
}

#[test]
fn an_operator_cap_shrinks_capacity_but_never_grows_it() {
    let quiet = [0u64; INGEST_HISTORY_DAYS];
    let capped = |cap_gb: u64| {
        let config = CapacityConfig { max_capacity_bytes: Some(cap_gb * GB), headroom_buffer_bytes: 0, ..CapacityConfig::default() };
        SlidingWindowCapacityForecaster::new(config).expect("valid").forecast(&load(1000, 500, &quiet)).expect("measured")
    };
    assert_eq!(capped(600).max_capacity_bytes, 600 * GB);
    assert_eq!(capped(600).target_reclaim_bytes, 20 * GB, "500 − 0.8·600");
    assert_eq!(capped(5000).max_capacity_bytes, 1000 * GB);
}

#[test]
fn a_volume_with_no_capacity_is_not_forecast() {
    assert_eq!(forecaster(0).forecast(&load(0, 0, &[])), None);
}

#[rstest]
#[case::defaults(CapacityConfig::default(), true)]
#[case::target_at_emergency(CapacityConfig { target_utilization: 0.95, ..CapacityConfig::default() }, false)]
#[case::emergency_above_one(CapacityConfig { emergency_utilization: 1.01, ..CapacityConfig::default() }, false)]
#[case::nan_target(CapacityConfig { target_utilization: f64::NAN, ..CapacityConfig::default() }, false)]
#[case::zero_window(CapacityConfig { sliding_window_days: 0, ..CapacityConfig::default() }, false)]
#[case::zero_alpha(CapacityConfig { ewma_alpha: 0.0, ..CapacityConfig::default() }, false)]
#[case::zero_cap(CapacityConfig { max_capacity_bytes: Some(0), ..CapacityConfig::default() }, false)]
fn config_bounds(#[case] config: CapacityConfig, #[case] valid: bool) {
    assert_eq!(config.validate().is_ok(), valid);
}

#[test]
fn daily_series_buckets_by_24_hour_windows_ending_now() {
    let now = 100 * DAY;
    let series = daily_series([(now - 10, 1), (now - DAY - 1, 2), (now - 3 * DAY, 4), (now + 5, 8), (0, 16)], now, 3);
    assert_eq!(series, [0, 2, 1], "oldest first; future and out-of-window events dropped");
}

proptest! {
    #[test]
    fn more_usage_or_more_ingest_never_needs_less(
        used_a in 0u64..=1000, used_b in 0u64..=1000, ingest_a in 0u64..=50, ingest_b in 0u64..=50,
    ) {
        let at = |used: u64, ingest: u64| {
            let history = [ingest * GB; INGEST_HISTORY_DAYS];
            forecaster(50).forecast(&load(1000, used, &history)).expect("measured").target_reclaim_bytes
        };
        prop_assert!(at(used_a.max(used_b), ingest_a.max(ingest_b)) >= at(used_a.min(used_b), ingest_a.min(ingest_b)));
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
    assert_eq!(library.unmatched_roots, [(App::Sonarr, "/data/media/tv".to_string()), (App::Sonarr, "/data/media/anime".to_string())]);
    assert_eq!(library.volume_of(App::Sonarr, "/data/media/tv/Andor"), None, "never governed against `/`");
}

fn plan_on(volume: &str, target: u64, planned: u64, eligible: u64) -> EvictionPlan {
    EvictionPlan {
        method: None,
        solver_error: None,
        items: Vec::new(),
        volumes: vec![VolumeOutcome { volume: volume.to_string(), target_bytes: target, planned_bytes: planned, eligible_bytes: eligible }],
        target_bytes: target,
        total_reclaimed_bytes: planned,
        total_regret: 0.0,
        candidates_count: 0,
        eligible_bytes: eligible,
        kept: Default::default(),
    }
}

fn status_of(
    volumes: &[Volume],
    forecasts: &[VolumeForecast],
    plan: &EvictionPlan,
    on_disk: &OnDisk,
    handed: &BTreeMap<String, u64>,
) -> CapacityStatus {
    CapacityStatus::new(CycleCapacity {
        config: &CapacityConfig::default(),
        volumes,
        forecasts,
        plan,
        unmatched_roots: &[(App::Sonarr, "/anime".to_string())],
        on_disk,
        handed,
    })
}

#[rstest]
#[case::healthy(0, 0, 0, None, None)]
#[case::handed_over(15, 15, 15, Some(true), Some(true))]
#[case::covered_but_paced_by_the_caps(15, 15, 5, Some(true), Some(false))]
#[case::short(15, 5, 5, Some(false), Some(false))]
fn a_target_is_covered_by_the_plan_and_met_only_once_handed_over(
    #[case] target_gb: u64,
    #[case] planned_gb: u64,
    #[case] handed_gb: u64,
    #[case] covered: Option<bool>,
    #[case] met: Option<bool>,
) {
    let volumes = [vol("/media", 100, 70)];
    let quiet = [0u64; INGEST_HISTORY_DAYS];
    let mut forecast = forecaster(0).forecast(&load(100, 70, &quiet)).expect("measured");
    forecast.target_reclaim_bytes = target_gb * GB;
    let forecasts = [VolumeForecast { volume: "/media".to_string(), forecast }];
    let handed = BTreeMap::from([("/media".to_string(), handed_gb * GB)]);
    let status = status_of(&volumes, &forecasts, &plan_on("/media", target_gb * GB, planned_gb * GB, 40 * GB), &OnDisk::default(), &handed);
    assert_eq!((status.covered, status.goal_met), (covered, met));
    assert_eq!((status.volumes[0].covered, status.volumes[0].goal_met), (covered, met));
    assert_eq!(status.healthy, met.is_none());
    assert_eq!(status.volumes[0].eligible_bytes, 40 * GB);
    assert_eq!(status.unmatched_roots, ["sonarr:/anime"]);
}

#[rstest]
#[case::downloads_and_leftovers(90, 50, 5, 3, 32)]
#[case::nothing_but_library(50, 50, 0, 0, 0)]
#[case::credit_beyond_the_measurement_is_never_negative(40, 38, 5, 0, 0)]
fn untracked_is_what_the_disk_holds_beyond_library_media_and_credited_evictions(
    #[case] used_gb: u64,
    #[case] library_gb: u64,
    #[case] pending_gb: u64,
    #[case] held_gb: u64,
    #[case] untracked_gb: u64,
) {
    let volumes = [vol("/media", 100, used_gb)];
    let quiet = [0u64; INGEST_HISTORY_DAYS];
    let forecast = forecaster(0).forecast(&load(100, used_gb, &quiet)).expect("measured");
    let forecasts = [VolumeForecast { volume: "/media".to_string(), forecast }];
    let on_disk = OnDisk {
        library: BTreeMap::from([("/media".to_string(), library_gb * GB)]),
        credit: BTreeMap::from([("/media".to_string(), Credit { pending: pending_gb * GB, held: held_gb * GB })]),
        held: BTreeMap::new(),
    };
    let status = status_of(&volumes, &forecasts, &plan_on("/media", 0, 0, 0), &on_disk, &BTreeMap::new());
    assert_eq!((status.untracked_bytes, status.volumes[0].untracked_bytes), (untracked_gb * GB, untracked_gb * GB));
    assert_eq!((status.pending_bytes, status.held_bytes), (pending_gb * GB, held_gb * GB));
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
