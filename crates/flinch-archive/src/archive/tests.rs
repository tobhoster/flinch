use super::*;
use crate::capacity::{AppDisks, RecycleBin, RootFolder, Volume};
use rstest::rstest;

const GIB: u64 = 1 << 30;

fn forecast(capacity_gib: u64, projected_gib: u64, target_gib: u64) -> CapacityForecast {
    CapacityForecast {
        current_used_bytes: projected_gib * GIB,
        max_capacity_bytes: capacity_gib * GIB,
        current_utilization: 0.0,
        daily_ingest_rate_bytes: 0,
        queue_bytes: 0,
        in_flight_bytes: 0,
        projected_used_bytes: projected_gib * GIB,
        target_reclaim_bytes: target_gib * GIB,
        is_emergency: false,
    }
}

fn capacity() -> CapacityConfig {
    CapacityConfig { target_utilization: 0.8, headroom_buffer_bytes: 50 * GIB, ..CapacityConfig::default() }
}

#[rstest]
// 0.8 × 1000 − 50 − 500 = 250 GiB the archive disk can still take.
#[case::room_under_the_target(1_000, 500, 0, 250)]
#[case::exactly_at_the_target(1_000, 750, 0, 0)]
#[case::a_disk_that_must_free_bytes_takes_none(1_000, 900, 200, 0)]
fn headroom_is_what_keeps_the_archive_disk_under_its_own_target(
    #[case] capacity_gib: u64,
    #[case] projected_gib: u64,
    #[case] target_gib: u64,
    #[case] room_gib: u64,
) {
    assert_eq!(headroom(&forecast(capacity_gib, projected_gib, target_gib), &capacity()), room_gib * GIB);
}

#[rstest]
#[case::off_by_default(ArchiveConfig::default(), true)]
#[case::on_with_roots(ArchiveConfig { enabled: true, ..ArchiveConfig::default() }, true)]
#[case::a_windows_root(ArchiveConfig { enabled: true, radarr_root: "D:\\Archive\\Movies".into(), ..ArchiveConfig::default() }, true)]
#[case::a_relative_root(ArchiveConfig { radarr_root: "archive/movies".into(), ..ArchiveConfig::default() }, false)]
#[case::on_without_roots(ArchiveConfig { enabled: true, radarr_root: String::new(), sonarr_root: " ".into(), ..ArchiveConfig::default() }, false)]
#[case::no_moves_per_run(ArchiveConfig { max_moves_per_run: 0, ..ArchiveConfig::default() }, false)]
fn archive_settings_are_validated(#[case] config: ArchiveConfig, #[case] valid: bool) {
    assert_eq!(config.validate().is_ok(), valid);
}

#[test]
fn only_a_root_on_a_measured_disk_becomes_a_destination() {
    // Different sizes: two filesystems, never merged as one.
    let disk =
        |path: &str, total_gib: u64| Volume { path: path.to_string(), total_bytes: total_gib * GIB, free_bytes: total_gib / 2 * GIB };
    let radarr = AppDisks {
        app: App::Radarr,
        instance: String::new(),
        diskspace: vec![disk("/media", 1_000), disk("/archive", 4_000)],
        root_folders: vec![
            RootFolder { path: "/media/movies".into(), free_bytes: None },
            RootFolder { path: "/archive/movies".into(), free_bytes: None },
        ],
        recycle: RecycleBin::Unknown,
    };
    let library = LibraryVolumes::build(&[radarr], |_| None);
    let forecasts = [VolumeForecast { volume: "/archive".into(), forecast: forecast(1_000, 500, 0) }];
    let config =
        ArchiveConfig { enabled: true, radarr_root: "/archive/movies/".into(), sonarr_root: "/archive/tv".into(), max_moves_per_run: 2 };

    let (found, unresolved) = destinations(config.roots(), &library, &forecasts, &capacity());

    let expected = ArchiveDestination {
        app: App::Radarr,
        instance: String::new(),
        root: "/archive/movies".into(),
        volume: "/archive".into(),
        headroom_bytes: 250 * GIB,
    };
    assert_eq!(found, [expected]);
    assert_eq!(unresolved, ["sonarr:/archive/tv is not a root folder on a disk FLINCH measures"], "Sonarr reported no disk: fail closed");
    let off = ArchiveConfig { enabled: false, ..config };
    assert_eq!(destinations(off.roots(), &library, &forecasts, &capacity()), (Vec::new(), Vec::new()));
    let (_, other) = destinations([(App::Radarr, "4k", "/archive/movies")], &library, &forecasts, &capacity());
    assert_eq!(other, ["radarr@4k:/archive/movies is not a root folder on a disk FLINCH measures"], "the default's disks are not 4k's");
}

#[rstest]
#[case::under_it("/archive/movies/Heat (1995)", "/archive/movies", true)]
#[case::with_a_trailing_slash("/archive/movies/Heat (1995)", "/archive/movies/", true)]
#[case::windows("D:\\Archive\\Heat (1995)", "D:\\Archive", true)]
#[case::a_sibling_with_the_same_prefix("/archive/movies-old/Heat", "/archive/movies", false)]
#[case::the_old_root("/media/movies/Heat (1995)", "/archive/movies", false)]
fn a_read_back_path_counts_only_under_the_root(#[case] path: &str, #[case] root: &str, #[case] under: bool) {
    assert_eq!(under_root(path, root), under);
}
