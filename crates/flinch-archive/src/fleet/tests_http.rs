//! The fleet against scripted apps over a real socket: what a read
//! publishes, which removals are refused before anything is sent, and that a
//! dry run sends reads only.

use super::act::{self, ActionError, Sent};
use super::queue::{Reason, StalledItem};
use super::requests::FleetRequest;
use super::{observe, Fleet, FleetApp, FleetConfig, FleetStatus, InstanceStatus, QueueMemory, QueueStatus};
use crate::arr::instances::Connection;
use crate::capacity::App;
use crate::executor::tests_http::{client, fake};
use crate::executor::ExecutorError;

const NOW: u64 = 1_800_000_000;

fn sonarr(base: &str) -> Connection {
    Connection {
        app: App::Sonarr,
        name: String::new(),
        base: base.to_string(),
        key: "secret".to_string(),
        archive_root: String::new(),
        compact_profile: String::new(),
        public_url: String::new(),
    }
}

const SYSTEM: &str = r#"{"version":"4.0.20.3014","branch":"main","osName":"alpine","startTime":"2026-10-04T07:14:30Z"}"#;
const HEALTH: &str = r#"[{"source":"IndexerLongTermStatusCheck","type":"warning","message":"Indexers unavailable due to failures for more than 6 hours: AniSource (Prowlarr)","wikiUrl":"https://wiki.servarr.com/sonarr/system#indexers-are-unavailable-due-to-failures"}]"#;
const DISKS: &str = r#"[{"path":"/","label":"","freeSpace":789815353344,"totalSpace":1021400207360}]"#;
const QUEUE: &str = r#"{"page":1,"pageSize":500,"totalRecords":2,"records":[
    {"id":1734954914,"seriesId":38,"title":"Carrie S01E06","size":6445614213,"sizeleft":0,"status":"completed",
     "trackedDownloadStatus":"warning","trackedDownloadState":"importPending",
     "statusMessages":[{"title":"Carrie S01E06","messages":["Not enough free space"]}],"errorMessage":"",
     "series":{"title":"Carrie","path":"/data/media/tv-b/Carrie","rootFolderPath":null}},
    {"id":42,"seriesId":36,"title":"Dark.Matter.S01E01.exe","size":1000,"sizeleft":0,"status":"completed",
     "trackedDownloadStatus":"warning","trackedDownloadState":"importBlocked",
     "statusMessages":[{"title":"x","messages":["Caution: Found executable file with extension: '.exe'"]}],
     "series":{"title":"Dark Matter (2024)","path":"/data/media/tv/Dark Matter (2024)"}}]}"#;

#[tokio::test]
async fn a_read_publishes_each_stuck_item_with_its_reason() {
    let (base, handle) = fake(vec![(200, SYSTEM), (200, HEALTH), (200, DISKS), (200, QUEUE)]);
    let http = client();
    let arrs = [sonarr(&base)];
    let fleet = Fleet { http: &http, arrs: &arrs, prowlarr: None, dry_run: true };
    let status = observe(&fleet, &mut QueueMemory::default(), &FleetConfig::default(), NOW).await;
    let seen = handle.join().expect("the fake app");

    assert_eq!(
        seen.iter().map(|(line, _)| line.as_str()).collect::<Vec<_>>(),
        [
            "GET /api/v3/system/status",
            "GET /api/v3/health",
            "GET /api/v3/diskspace",
            "GET /api/v3/queue?page=1&pageSize=500&includeSeries=true&includeUnknownSeriesItems=true",
        ]
    );
    let sonarr = &status.instances[0];
    assert!(sonarr.reachable && sonarr.error.is_none(), "{sonarr:?}");
    assert_eq!(sonarr.version.as_deref(), Some("4.0.20.3014"));
    assert_eq!(sonarr.health[0].source, "IndexerLongTermStatusCheck");
    assert_eq!(sonarr.disks[0].free_bytes, 789_815_353_344);
    let queue = sonarr.queue.as_ref().expect("the queue was read");
    let judged: Vec<(u64, Reason, bool)> = queue.stalled.iter().map(|item| (item.id, item.reason, item.removable)).collect();
    assert_eq!(judged, [(1_734_954_914, Reason::NoSpace, false), (42, Reason::Unimportable, true)]);
    assert!(status.indexers.is_empty(), "no Prowlarr, no indexers");
}

#[tokio::test]
async fn an_app_that_does_not_answer_is_published_unreachable() {
    let (base, handle) = fake(vec![(401, r#"{"error":"Unauthorized"}"#)]);
    let http = client();
    let arrs = [sonarr(&base)];
    let fleet = Fleet { http: &http, arrs: &arrs, prowlarr: None, dry_run: false };
    let status = observe(&fleet, &mut QueueMemory::default(), &FleetConfig::default(), NOW).await;
    handle.join().expect("the fake app");

    let sonarr = &status.instances[0];
    assert!(!sonarr.reachable);
    assert_eq!(sonarr.error.as_deref(), Some(r#"system/status: HTTP 401: {"error":"Unauthorized"}"#));
    assert_eq!(status.problems(), ["Sonarr does not answer"]);
}

const PROWLARR_SYSTEM: &str = r#"{"version":"2.6.5.5623","branch":"master","osName":"alpine"}"#;
const INDEXER: &str = r#"{"id":36,"name":"AniSource","enable":true,"protocol":"torrent","implementation":"Cardigann","fields":[]}"#;

#[tokio::test]
async fn prowlarr_reports_its_failing_indexer() {
    let script = vec![
        (200, PROWLARR_SYSTEM),
        (200, "[]"),
        (200, r#"[{"id":36,"name":"AniSource","enable":true,"protocol":"torrent"}]"#),
        (
            200,
            r#"[{"indexerId":36,"disabledTill":"2027-01-15T08:00:00Z","mostRecentFailure":"2027-01-15T07:00:00Z","initialFailure":"2027-01-01T00:00:00Z"}]"#,
        ),
        (200, r#"{"indexers":[{"indexerId":36,"averageResponseTime":1880,"numberOfQueries":1148}]}"#),
    ];
    let (base, handle) = fake(script);
    let http = client();
    let fleet = Fleet { http: &http, arrs: &[], prowlarr: Some((&base, "secret")), dry_run: true };
    let status = observe(&fleet, &mut QueueMemory::default(), &FleetConfig::default(), NOW).await;
    handle.join().expect("the fake app");

    assert_eq!(status.instances[0].app, FleetApp::Prowlarr);
    assert_eq!(status.indexers.iter().map(|indexer| (indexer.name.as_str(), indexer.failing)).collect::<Vec<_>>(), [("AniSource", true)]);
    assert_eq!(status.problems(), ["Prowlarr indexer AniSource is failing"]);
}

fn item(id: u64, reason: Reason) -> StalledItem {
    StalledItem {
        id,
        title: format!("item {id}"),
        series_or_movie: None,
        state: "importPending".into(),
        status: "warning".into(),
        messages: Vec::new(),
        reason,
        removable: reason.removable(),
        since: NOW,
        size_bytes: 1,
        target_path: None,
    }
}

/// Sonarr as last read: item 1 waits for space, item 2 never imports.
fn last_read(base: &str) -> FleetStatus {
    FleetStatus {
        checked_at: NOW,
        instances: vec![InstanceStatus {
            app: FleetApp::Sonarr,
            instance: String::new(),
            url: base.to_string(),
            reachable: true,
            error: None,
            version: None,
            branch: None,
            os: None,
            started_at: None,
            health: Vec::new(),
            queue: Some(QueueStatus { total: 2, stalled: vec![item(1, Reason::NoSpace), item(2, Reason::Unimportable)] }),
            disks: Vec::new(),
        }],
        indexers: Vec::new(),
        actions: Vec::new(),
    }
}

#[tokio::test]
async fn removing_an_item_that_waits_for_space_is_refused_before_anything_is_sent() {
    let (base, handle) = fake(Vec::new());
    let http = client();
    let arrs = [sonarr(&base)];
    let fleet = Fleet { http: &http, arrs: &arrs, prowlarr: None, dry_run: false };
    let status = last_read(&base);

    let request = FleetRequest::QueueRemove { app: App::Sonarr, instance: String::new(), id: 1 };
    let action = act::requested(&fleet, &status, &request, NOW).await;
    let direct = act::remove_queue_item(&fleet, &status, App::Sonarr, "", 1).await;

    assert!(handle.join().expect("the fake app").is_empty(), "nothing reached Sonarr");
    assert!(matches!(direct, Err(ActionError::NotRemovable(Reason::NoSpace))), "{direct:?}");
    assert_eq!(action.target, "sonarr 1 (item 1)");
    assert!(action.result.starts_with("failed: refused: queue items that wait for free space"), "{}", action.result);
}

#[tokio::test]
async fn a_dry_run_removal_sends_nothing_and_is_recorded_as_such() {
    let (base, handle) = fake(Vec::new());
    let http = client();
    let arrs = [sonarr(&base)];
    let fleet = Fleet { http: &http, arrs: &arrs, prowlarr: None, dry_run: true };
    let config = FleetConfig { auto_remove_unimportable: true, ..FleetConfig::default() };

    let actions = act::auto(&fleet, &last_read(&base), &config, NOW).await;

    assert!(handle.join().expect("the fake app").is_empty(), "a dry run sends nothing");
    assert_eq!(
        actions.iter().map(|action| (action.target.as_str(), action.result.as_str())).collect::<Vec<_>>(),
        [("sonarr 2 (item 2)", "dry-run")]
    );
}

#[tokio::test]
async fn a_live_clean_up_removes_and_blocklists_only_the_unimportable_item() {
    let (base, handle) = fake(vec![(200, "")]);
    let http = client();
    let arrs = [sonarr(&base)];
    let fleet = Fleet { http: &http, arrs: &arrs, prowlarr: None, dry_run: false };
    let config = FleetConfig { auto_remove_unimportable: true, ..FleetConfig::default() };

    let actions = act::auto(&fleet, &last_read(&base), &config, NOW).await;
    let seen = handle.join().expect("the fake app");

    assert_eq!(
        seen.iter().map(|(line, _)| line.as_str()).collect::<Vec<_>>(),
        ["DELETE /api/v3/queue/2?removeFromClient=true&blocklist=true"]
    );
    assert_eq!(actions.iter().map(|action| action.result.as_str()).collect::<Vec<_>>(), ["done"]);
}

#[tokio::test]
async fn an_item_gone_from_the_queue_is_not_removed() {
    let (base, handle) = fake(Vec::new());
    let http = client();
    let arrs = [sonarr(&base)];
    let fleet = Fleet { http: &http, arrs: &arrs, prowlarr: None, dry_run: false };

    let result = act::remove_queue_item(&fleet, &last_read(&base), App::Sonarr, "", 99).await;

    assert!(handle.join().expect("the fake app").is_empty());
    assert!(matches!(result, Err(ActionError::NotQueued)), "{result:?}");
}

#[tokio::test]
async fn a_dry_run_indexer_test_reads_the_indexer_and_sends_no_test() {
    let (base, handle) = fake(vec![(200, INDEXER)]);
    let http = client();
    let fleet = Fleet { http: &http, arrs: &[], prowlarr: Some((&base, "secret")), dry_run: true };

    let result = act::test_indexer(&fleet, 36).await;

    let seen = handle.join().expect("the fake app");
    assert_eq!(seen.iter().map(|(line, _)| line.as_str()).collect::<Vec<_>>(), ["GET /api/v1/indexer/36"]);
    assert!(matches!(result, Ok(Sent::DryRun)), "{result:?}");
}

#[tokio::test]
async fn a_live_indexer_test_sends_the_indexers_own_resource_and_reports_prowlarrs_cause() {
    // Prowlarr's answer as seen live: the cause sits past the first 200 bytes of the JSON.
    let refused = r#"[{"isWarning":false,"propertyName":"","errorMessage":"Unable to connect to indexer, indexer's server is unavailable. Try again later. HTTP request failed: [504:GatewayTimeout] [GET] at [https://asnet.pw/]","severity":"error"}]"#;
    let (base, handle) = fake(vec![(200, INDEXER), (400, refused)]);
    let http = client();
    let fleet = Fleet { http: &http, arrs: &[], prowlarr: Some((&base, "secret")), dry_run: false };

    let result = act::test_indexer(&fleet, 36).await;

    let seen = handle.join().expect("the fake app");
    assert_eq!(seen[1].0, "POST /api/v1/indexer/test");
    let sent: serde_json::Value = serde_json::from_str(&seen[1].1).expect("a JSON body");
    assert_eq!(sent, serde_json::from_str::<serde_json::Value>(INDEXER).expect("json"));
    assert!(matches!(result, Err(ActionError::Api(ExecutorError::Http { status: 400, .. }))), "{result:?}");
    let said = result.expect_err("refused").to_string();
    assert!(said.contains("indexer's server is unavailable") && said.ends_with("[GET] at [https://asnet.pw/]"), "the whole cause: {said}");
}

#[tokio::test]
async fn an_indexer_failing_past_the_limit_is_disabled_and_read_back() {
    let disabled = r#"{"id":36,"name":"AniSource","enable":false,"protocol":"torrent"}"#;
    let (base, handle) = fake(vec![(200, INDEXER), (202, disabled)]);
    let http = client();
    let fleet = Fleet { http: &http, arrs: &[], prowlarr: Some((&base, "secret")), dry_run: false };
    let indexers = super::indexers::join(
        serde_json::from_str(r#"[{"id":36,"name":"AniSource","enable":true,"protocol":"torrent"}]"#).expect("json"),
        serde_json::from_str(r#"[{"indexerId":36,"disabledTill":"2027-01-15T08:00:00Z","initialFailure":"2027-01-01T00:00:00Z"}]"#)
            .expect("json"),
        serde_json::Value::Null,
        NOW,
    );
    let status = FleetStatus { indexers, ..FleetStatus::default() };
    let config = FleetConfig { disable_failing_indexers_after_hours: 24, ..FleetConfig::default() };

    let actions = act::auto(&fleet, &status, &config, crate::presence::parse_utc("2027-01-15T00:00:00Z").unwrap_or(0)).await;

    let seen = handle.join().expect("the fake app");
    assert_eq!(seen[1].0, "PUT /api/v1/indexer/36");
    assert!(seen[1].1.contains(r#""enable":false"#), "{}", seen[1].1);
    assert_eq!(
        actions.iter().map(|action| (action.target.as_str(), action.result.as_str())).collect::<Vec<_>>(),
        [("indexer 36 (AniSource)", "done")]
    );
}
