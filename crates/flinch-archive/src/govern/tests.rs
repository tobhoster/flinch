use super::*;
use crate::arr::{SeasonStats, SeriesSeason};
use crate::capacity::{AppDisks, Credit, RecycleBin, RootFolder, Volume};

const GB: u64 = 1_000_000_000;

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
    let share = Volume { path: "/media".to_string(), total_bytes: 100 * GB, free_bytes: (100 - used_gb) * GB };
    let root = |path: &str| vec![RootFolder { path: path.to_string(), free_bytes: None }];
    LibraryVolumes::build(&[
        AppDisks { app: App::Radarr, diskspace: vec![share.clone()], root_folders: root("/media/movies"), recycle: RecycleBin::Disabled },
        AppDisks { app: App::Sonarr, diskspace: vec![share], root_folders: root("/media/tv"), recycle: RecycleBin::Disabled },
    ])
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
    let governance = govern(LibraryVolumes::build(&[]), HashMap::new(), &CapacityConfig::default(), &Ingest::default(), OnDisk::default());
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
