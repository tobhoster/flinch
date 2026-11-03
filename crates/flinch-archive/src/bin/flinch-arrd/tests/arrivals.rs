//! A cycle's arrivals against a fake Sonarr: an import cache written before
//! grabs were read is read again at once, the flush it held stops counting as
//! arrival while a fresh import counts, and a download held complete is
//! queued whole.

use super::fake::{self, Server};
use super::{args, connection, now, state_dir};
use crate::signals;
use flinch_archive::capacity::App;
use flinch_archive::presence::format_utc;
use serde_json::json;

const GIB: u64 = 1 << 30;
const HOUR: u64 = 3_600;

/// A Sonarr whose last hour imported a flushed episode (grabbed six days
/// earlier) and a fresh one (grabbed four hours earlier), and whose queue
/// holds a downloaded episode waiting for space.
fn sonarr(now: u64) -> Server {
    let date = move |ago: u64| format_utc(now - ago);
    fake::serve(move |request| {
        let since = request.route() == "/api/v3/history/since";
        let body = if since && request.path.contains("eventType=3") {
            json!([
                {"seriesId": 36, "date": date(HOUR), "downloadId": "flushed", "data": {"size": (10 * GIB).to_string()},
                 "episode": {"seasonNumber": 1}, "eventType": "downloadFolderImported", "id": 1},
                {"seriesId": 40, "date": date(HOUR), "downloadId": "fresh", "data": {"size": (5 * GIB).to_string()},
                 "episode": {"seasonNumber": 1}, "eventType": "downloadFolderImported", "id": 2},
            ])
        } else if since && request.path.contains("eventType=1") {
            json!([
                {"seriesId": 36, "date": date(6 * 24 * HOUR), "downloadId": "flushed", "eventType": "grabbed", "id": 3},
                {"seriesId": 40, "date": date(4 * HOUR), "downloadId": "fresh", "eventType": "grabbed", "id": 4},
            ])
        } else if request.route() == "/api/v3/queue" {
            json!({"page": 1, "pageSize": 500, "totalRecords": 1, "records": [
                {"id": 5, "seriesId": 38, "seasonNumber": 1, "size": 6 * GIB, "sizeleft": 0, "status": "completed",
                 "trackedDownloadStatus": "warning", "trackedDownloadState": "importPending", "downloadId": "held",
                 "statusMessages": [{"messages": ["Not enough free space"]}]}
            ]})
        } else {
            return (404, "{}".into());
        };
        (200, body.to_string())
    })
}

fn history_reads(server: &Server) -> usize {
    server.requests().iter().filter(|request| request.route() == "/api/v3/history/since").count()
}

#[tokio::test]
async fn an_import_cache_from_before_grabs_is_read_again_and_its_flush_stops_counting() {
    let (_turn, dir) = state_dir("arrivals").await;
    let now = now();
    // As FLINCH wrote it before reading grabs, a minute ago: fresh by age alone.
    let older = json!({"sonarr": {"read_at": now - 60, "imports": [
        {"app": "sonarr", "item": {"series": {"series_id": 36, "season": 1}}, "epoch": now - HOUR, "bytes": 10 * GIB}
    ]}});
    std::fs::write(dir.join("arr-imports.json"), older.to_string()).expect("an old import cache");
    let sonarr = sonarr(now);
    let args = args(vec![connection(App::Sonarr, "", &sonarr.base)]);
    let http = reqwest::Client::new();

    let gathered = signals::gather(&http, &args, &[], now).await;
    let arrivals: Vec<(u64, bool)> = gathered.imports.iter().map(|import| (import.bytes, import.is_backlog())).collect();
    assert_eq!(arrivals, [(10 * GIB, true), (5 * GIB, false)]);
    assert_eq!(gathered.queue.iter().map(|queued| queued.bytes).collect::<Vec<_>>(), [6 * GIB]);
    assert_eq!(history_reads(&sonarr), 2, "imports and grabs, once each");

    // The read with its grabs serves the next cycle as it is.
    let again = signals::gather(&http, &args, &[], now + 60).await;
    assert_eq!(history_reads(&sonarr), 2);
    assert_eq!(again.imports, gathered.imports);
    std::fs::remove_dir_all(&dir).ok();
}
