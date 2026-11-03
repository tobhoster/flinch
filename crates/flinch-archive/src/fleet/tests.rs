//! How queue records are judged, how indexer reads join, and how requests
//! travel through the queue directory.

use super::indexers;
use super::queue::{classify, QueueMemory, QueueRecord, Reason};
use super::requests::{pending, FleetRequest};
use crate::capacity::App;
use rstest::rstest;

const HOUR: u64 = 3_600;
const STALLED_AFTER: u64 = 2 * HOUR;

fn record(json: &str) -> QueueRecord {
    serde_json::from_str(json).expect("a queue record")
}

/// The ten records live Sonarr lists today all look like this one.
const LIVE_NO_SPACE: &str = r#"{"id":1734954914,"seriesId":38,"title":"Carrie S01E06 2160p","size":6445614213,"sizeleft":0,
    "status":"completed","trackedDownloadStatus":"warning","trackedDownloadState":"importPending",
    "statusMessages":[{"title":"Carrie S01E06 2160p","messages":["Not enough free space"]}],"errorMessage":"",
    "added":"2026-10-14T19:47:49Z","series":{"title":"Carrie","path":"/data/media/tv-b/Carrie","rootFolderPath":null}}"#;

fn blocked(message: &str) -> String {
    format!(
        r#"{{"id":7,"seriesId":3,"status":"completed","trackedDownloadStatus":"warning","trackedDownloadState":"importPending",
        "statusMessages":[{{"title":"x","messages":["{message}"]}}],"series":{{"title":"Show","path":"/tv/Show"}}}}"#
    )
}

#[rstest]
#[case::live_not_enough_free_space(LIVE_NO_SPACE.to_string(), 0, Some(Reason::NoSpace))]
#[case::free_space_outranks_a_failed_repair(
    r#"{"id":7,"status":"failed","trackedDownloadState":"failedPending","trackedDownloadStatus":"error",
        "statusMessages":[{"messages":["Not enough free space"]}],"errorMessage":"Repair failed, not enough repair blocks"}"#.to_string(),
    0,
    Some(Reason::NoSpace)
)]
#[case::no_files_eligible(blocked("No files found are eligible for import in /data/usenet/tv/Show.S01E01"), 0, Some(Reason::Unimportable))]
#[case::executable(blocked("Caution: Found executable file with extension: '.exe'"), 0, Some(Reason::Unimportable))]
#[case::sample_only(blocked("Sample"), 0, Some(Reason::Unimportable))]
#[case::missing_files(blocked("Download is missing files"), 0, Some(Reason::Unimportable))]
#[case::unknown_series_by_message(blocked("Unknown Series"), 0, Some(Reason::Unimportable))]
#[case::unknown_series_without_an_item(
    r#"{"id":7,"status":"completed","trackedDownloadStatus":"warning","trackedDownloadState":"importPending","statusMessages":[]}"#.to_string(),
    0,
    Some(Reason::Unimportable)
)]
#[case::failed_usenet_repair(
    r#"{"id":7,"seriesId":3,"status":"failed","trackedDownloadState":"failedPending","trackedDownloadStatus":"warning",
        "errorMessage":"Repair failed, not enough repair blocks (12 short)"}"#.to_string(),
    0,
    Some(Reason::Unimportable)
)]
#[case::failed_download(r#"{"id":7,"seriesId":3,"status":"failed","trackedDownloadState":"failed"}"#.to_string(), 0, Some(Reason::Unimportable))]
#[case::download_without_progress(
    r#"{"id":7,"seriesId":3,"status":"downloading","trackedDownloadState":"downloading","trackedDownloadStatus":"ok","sizeleft":500}"#.to_string(),
    3 * HOUR,
    Some(Reason::StalledDownload)
)]
#[case::download_progressing(
    r#"{"id":7,"seriesId":3,"status":"downloading","trackedDownloadState":"downloading","trackedDownloadStatus":"ok","sizeleft":500}"#.to_string(),
    HOUR,
    None
)]
#[case::queued_in_the_client_is_not_stalled(
    r#"{"id":7,"seriesId":3,"status":"queued","trackedDownloadState":"downloading","trackedDownloadStatus":"ok","sizeleft":500}"#.to_string(),
    9 * HOUR,
    None
)]
#[case::about_to_import(
    r#"{"id":7,"seriesId":3,"status":"completed","trackedDownloadState":"importPending","trackedDownloadStatus":"ok"}"#.to_string(),
    0,
    None
)]
#[case::flagged_for_another_reason(blocked("Episode has a TBA title and recently aired"), 0, Some(Reason::Other))]
fn a_queue_record_is_judged(#[case] json: String, #[case] stalled_secs: u64, #[case] expected: Option<Reason>) {
    let reason = classify(&record(&json), stalled_secs, STALLED_AFTER);
    assert_eq!(reason, expected);
    let removable = reason.is_some_and(Reason::removable);
    assert_eq!(removable, expected == Some(Reason::Unimportable), "only an unimportable item is removable");
}

#[test]
fn the_live_free_space_item_is_published_non_removable_with_its_root_folder() {
    let mut memory = QueueMemory::default();
    let stalled = memory.judge("sonarr", &[record(LIVE_NO_SPACE)], STALLED_AFTER, 1_800_000_000);

    assert_eq!(stalled.len(), 1);
    let item = &stalled[0];
    assert_eq!((item.reason, item.removable), (Reason::NoSpace, false));
    assert_eq!(item.messages, vec!["Not enough free space".to_string()]);
    assert_eq!(item.series_or_movie.as_deref(), Some("Carrie"));
    assert_eq!(item.target_path.as_deref(), Some("/data/media/tv-b"), "no root folder given: the item's own folder's parent");
    assert_eq!(item.since, crate::presence::parse_utc("2026-10-14T19:47:49Z").unwrap_or(0));
}

#[test]
fn a_download_is_stalled_only_after_the_limit_without_fewer_bytes_left() {
    let downloading = |left: u64| {
        record(&format!(r#"{{"id":9,"seriesId":3,"status":"downloading","trackedDownloadState":"downloading","sizeleft":{left}}}"#))
    };
    let start = 1_800_000_000;
    let mut memory = QueueMemory::default();

    assert!(memory.judge("sonarr", &[downloading(500)], STALLED_AFTER, start).is_empty(), "first seen: progressing");
    assert!(memory.judge("sonarr", &[downloading(500)], STALLED_AFTER, start + HOUR).is_empty(), "an hour without progress");
    let stalled = memory.judge("sonarr", &[downloading(500)], STALLED_AFTER, start + 3 * HOUR);
    assert_eq!(
        stalled.iter().map(|item| (item.reason, item.since, item.removable)).collect::<Vec<_>>(),
        [(Reason::StalledDownload, start, false)]
    );
    assert!(memory.judge("sonarr", &[downloading(400)], STALLED_AFTER, start + 4 * HOUR).is_empty(), "progress again");
}

const INDEXERS: &str = r#"[{"id":36,"name":"AniSource","enable":true,"protocol":"torrent"},
    {"id":40,"name":"dmhy","enable":true,"protocol":"torrent"},
    {"id":51,"name":"altHUB","enable":true,"protocol":"usenet"}]"#;
const STATUSES: &str = r#"[{"indexerId":36,"disabledTill":"2026-10-18T18:44:38Z","mostRecentFailure":"2026-10-17T18:44:38Z","initialFailure":"2026-10-09T18:34:33Z"},
    {"indexerId":40,"disabledTill":null,"mostRecentFailure":"2026-10-15T08:00:00Z","initialFailure":"2026-10-15T08:00:00Z"},
    {"indexerId":99,"disabledTill":"2026-10-19T00:00:00Z"}]"#;
const STATS: &str = r#"{"indexers":[{"indexerId":36,"averageResponseTime":1880,"numberOfQueries":1148,"numberOfGrabs":0,"numberOfFailedQueries":0},
    {"indexerId":40,"averageResponseTime":1103,"numberOfQueries":117,"numberOfGrabs":0,"numberOfFailedQueries":11}]}"#;

/// The three Prowlarr reads, as `indexers::join` takes them, at `now`.
fn joined(now: &str) -> (u64, Vec<indexers::IndexerStatus>) {
    let now = crate::presence::parse_utc(now).unwrap_or(0);
    let rows = |json: &str| serde_json::from_str::<Vec<serde_json::Value>>(json).expect("json rows");
    let stats = serde_json::from_str(STATS).expect("json stats");
    (now, indexers::join(rows(INDEXERS), rows(STATUSES), stats, now))
}

#[test]
fn an_indexer_with_a_back_off_is_failing_and_one_that_recovered_is_not() {
    let (now, listed) = joined("2026-10-18T12:00:00Z");

    let failing: Vec<(&str, bool)> = listed.iter().map(|indexer| (indexer.name.as_str(), indexer.failing)).collect();
    assert_eq!(failing, [("AniSource", true), ("dmhy", false), ("altHUB", false)], "a status of an unlisted indexer is dropped");
    let anisource = &listed[0];
    assert_eq!((anisource.queries, anisource.avg_response_ms, anisource.success_rate), (1148, 1880, Some(1.0)));
    assert_eq!(anisource.failing_for(now).map(|secs| secs / 86_400), Some(8), "failing since Oct 9");
    assert_eq!(listed[1].success_rate, Some(106.0 / 117.0));
    assert_eq!(listed[2].success_rate, None, "no query yet: no rate");
}

#[test]
fn a_recent_failure_counts_as_failing_without_a_back_off() {
    let (_, listed) = joined("2026-10-15T10:00:00Z");
    assert!(listed[1].failing, "dmhy failed two hours ago");
}

#[rstest]
#[case::default_sonarr("sonarr", "123", Some(FleetRequest::QueueRemove { app: App::Sonarr, instance: String::new(), id: 123 }))]
#[case::named_radarr("radarr@4k", "9", Some(FleetRequest::QueueRemove { app: App::Radarr, instance: "4k".into(), id: 9 }))]
#[case::not_an_arr("lidarr", "1", None)]
#[case::a_path("..%2F..", "1", None)]
#[case::a_season("sonarr-5-s", "2", None)]
#[case::not_a_number("sonarr", "1a", None)]
#[case::an_upper_case_instance("radarr@4K", "9", None)]
fn only_an_instance_and_a_queue_id_name_a_removal(#[case] key: &str, #[case] id: &str, #[case] expected: Option<FleetRequest>) {
    assert_eq!(FleetRequest::queue_remove(key, id), expected);
}

#[test]
fn queued_requests_are_read_back_as_written() {
    let dir = std::env::temp_dir().join(format!("flinch-fleet-requests-{}", std::process::id()));
    let requests = [FleetRequest::queue_remove("sonarr@anime", "1734954914"), FleetRequest::indexer_test("36")];
    for request in requests.iter().flatten() {
        let path = request.path(&dir);
        std::fs::create_dir_all(path.parent().expect("the queue dir")).expect("queue dir");
        std::fs::write(path, b"").expect("request file");
    }
    std::fs::write(super::requests::requests_dir(&dir).join("stray.txt"), b"").expect("stray file");

    let read: Vec<FleetRequest> = pending(&dir).into_iter().map(|(_, request)| request).collect();
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        read,
        [FleetRequest::IndexerTest { id: 36 }, FleetRequest::QueueRemove { app: App::Sonarr, instance: "anime".into(), id: 1_734_954_914 }]
    );
}
