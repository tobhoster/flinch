use super::*;
use crate::arr::{SeasonStats, SeriesSeason};
use crate::capacity::{AppDisks, Credit, RecycleBin, RootFolder, Volume};
use crate::signals::Import;

const GB: u64 = 1_000_000_000;

fn no_probe(_: &str) -> Option<Volume> {
    None
}

fn movie(id: u32, path: Option<&str>) -> ArrMovie {
    ArrMovie {
        id,
        title: format!("Movie {id}"),
        title_slug: None,
        year: None,
        size_on_disk: 10 * GB,
        has_file: true,
        added: None,
        images: Vec::new(),
        movie_file: None,
        path: path.map(str::to_string),
        ..Default::default()
    }
}

fn show(id: u32, path: Option<&str>, seasons: &[u32]) -> ArrSeries {
    ArrSeries {
        id,
        title: format!("Show {id}"),
        title_slug: None,
        year: None,
        series_type: "standard".to_string(),
        seasons: seasons
            .iter()
            .map(|n| SeriesSeason {
                season_number: *n,
                statistics: SeasonStats { episode_file_count: 1, episode_count: 1, total_episode_count: 1, size_on_disk: GB },
                ..Default::default()
            })
            .collect(),
        added: None,
        images: Vec::new(),
        path: path.map(str::to_string),
        status: None,
        previous_airing: None,
        ..Default::default()
    }
}

/// Radarr and Sonarr both see one 100 GB share at /media, `used_gb` full.
fn library(used_gb: u64) -> LibraryVolumes {
    let share = Volume { path: "/media".to_string(), total_bytes: 100 * GB, free_bytes: (100 - used_gb) * GB, device: None };
    let root = |path: &str| vec![RootFolder { path: path.to_string(), free_bytes: None }];
    LibraryVolumes::build(
        &[
            AppDisks {
                app: App::Radarr,
                instance: String::new(),
                diskspace: vec![share.clone()],
                root_folders: root("/media/movies"),
                recycle: RecycleBin::Disabled,
                recycle_path: String::new(),
            },
            AppDisks {
                app: App::Sonarr,
                instance: String::new(),
                diskspace: vec![share],
                root_folders: root("/media/tv"),
                recycle: RecycleBin::Disabled,
                recycle_path: String::new(),
            },
        ],
        no_probe,
    )
}

fn governed(used_gb: u64, config: &CapacityConfig, ingest: &Ingest, on_disk: OnDisk) -> Governance {
    let library = library(used_gb);
    let located = volume_map(&library, &[movie(1, Some("/media/movies/One (2001)"))], &[show(7, Some("/media/tv/Seven"), &[1, 2])]);
    govern(library, located, config, ingest, on_disk)
}

fn no_headroom() -> CapacityConfig {
    CapacityConfig { headroom_buffer_bytes: 0, ..CapacityConfig::default() }
}

#[test]
fn items_are_attributed_through_their_own_app_and_path() {
    let library = library(50);
    let map = volume_map(
        &library,
        &[movie(1, Some("/media/movies/One")), movie(2, None), movie(3, Some("/downloads/Three"))],
        &[show(7, Some("/media/tv/Seven"), &[1, 2]), show(8, None, &[1])],
    );
    assert_eq!(map.get("radarr-1").map(String::as_str), Some("/media"));
    assert_eq!(map.get("sonarr-7-s1").map(String::as_str), Some("/media"));
    assert_eq!(map.get("sonarr-7-s2").map(String::as_str), Some("/media"));
    for absent in ["radarr-2", "radarr-3", "sonarr-8-s1"] {
        assert!(!map.contains_key(absent), "{absent} has no governed path");
    }
}

#[test]
fn no_library_volume_is_unmeasured_and_reports_nothing() {
    let governance =
        govern(LibraryVolumes::build(&[], no_probe), HashMap::new(), &CapacityConfig::default(), &Ingest::default(), OnDisk::default());
    assert!(governance.forecasts.is_empty());
    assert!(governance.status(&crate::plan::generate_eviction_plan(&[], &[], &Default::default()).expect("empty plan"), []).is_none());
}

#[test]
fn one_shared_disk_is_forecast_once_with_its_ingest_queue_and_credit() {
    let ingest = Ingest {
        daily: BTreeMap::from([("/media".to_string(), vec![GB; crate::capacity::INGEST_HISTORY_DAYS])]),
        queue: BTreeMap::from([("/media".to_string(), 3 * GB)]),
    };
    let on_disk = OnDisk { credit: BTreeMap::from([("/media".to_string(), Credit { pending: 2 * GB, held: 0 })]), ..OnDisk::default() };
    let governance = governed(75, &no_headroom(), &ingest, on_disk);
    assert_eq!(governance.forecasts.len(), 1, "Radarr and Sonarr see the same share");
    let forecast = &governance.forecasts[0].forecast;
    assert_eq!(forecast.projected_used_bytes, (75 + 14 + 3 - 2) * GB);
    assert_eq!(forecast.target_reclaim_bytes, 10 * GB, "90 projected against an 80 GB target");
    assert_eq!(governance.volume_for("sonarr-7-s2").as_deref(), Some("/media"));
}

#[test]
fn an_invalid_config_forecasts_nothing_rather_than_guessing() {
    let config = CapacityConfig { target_utilization: 0.99, ..CapacityConfig::default() };
    assert!(governed(99, &config, &Ingest::default(), OnDisk::default()).forecasts.is_empty());
}

/// Sonarr's two TV disks, as live: same size, told apart by device.
fn tv_disks() -> LibraryVolumes {
    let disk = |path: &str, device| Volume { path: path.to_string(), total_bytes: 800 * GB, free_bytes: 200 * GB, device: Some(device) };
    let root = |path: &str| RootFolder { path: path.to_string(), free_bytes: None };
    LibraryVolumes::build(
        &[AppDisks {
            app: App::Sonarr,
            instance: String::new(),
            diskspace: vec![disk("/data/media/tv", 1), disk("/data/media/tv-b", 2)],
            root_folders: vec![root("/data/media/tv"), root("/data/media/tv-b")],
            recycle: RecycleBin::Disabled,
            recycle_path: String::new(),
        }],
        no_probe,
    )
}

/// Dark Matter on tv; Carrie and The Dropout on tv-b.
fn tv_shows() -> Vec<ArrSeries> {
    vec![
        show(36, Some("/data/media/tv/Dark Matter (2024)"), &[1]),
        show(38, Some("/data/media/tv-b/Carrie"), &[1]),
        show(40, Some("/data/media/tv-b/The Dropout"), &[1]),
    ]
}

const NOW: u64 = 1_792_442_000;
const HOUR: u64 = 3_600;

#[test]
fn a_download_held_complete_is_queued_whole_on_the_disk_it_will_import_to() {
    // As live Sonarr held them on full disks: complete, nothing left, not imported.
    let held = |id: u64, series: u64, size: u64, state: &str| {
        serde_json::json!({"id": id, "seriesId": series, "seasonNumber": 1, "size": size, "sizeleft": 0, "status": "completed",
            "trackedDownloadStatus": "warning", "trackedDownloadState": state, "downloadId": format!("dl-{id}"),
            "statusMessages": [{"messages": ["Not enough free space"]}]})
    };
    let records = vec![
        held(1, 36, 9 * GB, "importPending"),
        held(2, 36, 8 * GB, "importBlocked"),
        held(3, 38, 6 * GB, "importPending"),
        serde_json::json!({"id": 4, "seriesId": 40, "seasonNumber": 1, "size": 5 * GB, "sizeleft": 2 * GB,
            "status": "downloading", "trackedDownloadState": "downloading", "downloadId": "dl-4"}),
    ];
    let signals = Signals { queue: crate::signals::arr::parse_queue(App::Sonarr, "", records), ..Signals::default() };
    let ingest = Ingest::attribute(&tv_disks(), &[], &tv_shows(), &signals, NOW);
    assert_eq!(ingest.queue, BTreeMap::from([("/data/media/tv".to_string(), 17 * GB), ("/data/media/tv-b".to_string(), 8 * GB)]));
}

#[test]
fn a_backlog_flush_stays_out_of_the_rate_while_a_new_import_counts() {
    let import = |series_id: u64, bytes: u64, waited_hours: Option<u64>| Import {
        app: App::Sonarr,
        instance: String::new(),
        item: ItemRef::Series { series_id, season: Some(1) },
        epoch: NOW - HOUR,
        bytes,
        grabbed_at: waited_hours.map(|hours| NOW - HOUR - hours * HOUR),
    };
    let signals = Signals {
        imports: vec![
            // Held six days on a full tv, four on tv-b, then flushed together.
            import(36, 55 * GB, Some(145)),
            import(38, 21 * GB, Some(104)),
            // Grabbed and imported the same evening.
            import(40, 40 * GB, Some(1)),
            // No grab in the window: a manual import counts as arrival.
            import(40, 2 * GB, None),
        ],
        ..Signals::default()
    };
    let ingest = Ingest::attribute(&tv_disks(), &[], &tv_shows(), &signals, NOW);
    let arrived = |volume: &str| ingest.daily.get(volume).map_or(0, |days| days.iter().sum::<u64>());
    assert_eq!(arrived("/data/media/tv"), 0);
    assert_eq!(arrived("/data/media/tv-b"), 42 * GB);

    let governance = govern(tv_disks(), HashMap::new(), &no_headroom(), &ingest, OnDisk::default());
    let rate = |volume: &str| governance.forecasts.iter().find(|row| row.volume == volume).map(|row| row.forecast.daily_ingest_rate_bytes);
    assert_eq!(rate("/data/media/tv"), Some(0));
    assert_eq!(rate("/data/media/tv-b"), Some(42 * GB / 5), "α = 0.2 of one day's 42 GB");
}
