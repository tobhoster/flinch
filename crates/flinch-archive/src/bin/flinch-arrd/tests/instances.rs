//! Two Radarrs, each holding Heat as its movie 7: an HD copy and a 4K one.

use super::fake::{self, Server};
use super::{args, connection, state_dir};
use crate::fetch::fetch_inventory;
use flinch_archive::capacity::App;
use flinch_archive::executor::radarr::Radarr;
use flinch_archive::executor::{DeleteMode, Evicted};
use flinch_archive::ids::ArrRef;
use serde_json::json;

const GIB: u64 = 1 << 30;

/// A Radarr whose movie 7 is Heat at `bytes`, tagged with its tag 1
/// labelled `label`, imported from the download `hash`. Its file 70 goes
/// once deleted, and Heat reads back unmonitored once unmonitored.
fn radarr(bytes: u64, label: &'static str, hash: &'static str) -> Server {
    let (mut monitored, mut deleted) = (true, false);
    fake::serve(move |request| {
        let heat = || {
            json!({
                "id": 7, "title": "Heat", "year": 1995, "tmdbId": 949, "monitored": monitored,
                "hasFile": !deleted, "sizeOnDisk": if deleted { 0 } else { bytes }, "tags": [1],
                "qualityProfileId": 1, "rootFolderPath": "/movies", "path": "/movies/Heat (1995)",
                "movieFile": if deleted { json!(null) } else { json!({ "id": 70, "dateAdded": "2025-01-01T00:00:00Z" }) },
            })
        };
        let answer = match (request.method.as_str(), request.route()) {
            ("GET", "/api/v3/movie") => json!([heat()]),
            ("GET", "/api/v3/movie/7") => heat(),
            ("GET", "/api/v3/tag") => json!([{ "id": 1, "label": label }]),
            ("GET", "/api/v3/history") if request.path.contains("eventType=3") => json!({
                "totalRecords": 1,
                "records": [{ "id": 1, "date": "2025-01-01T00:00:00Z", "eventType": "downloadFolderImported", "movieId": 7, "downloadId": hash }],
            }),
            ("GET", "/api/v3/history") => json!({ "totalRecords": 0, "records": [] }),
            ("PUT", "/api/v3/movie/editor") => {
                monitored = false;
                json!([])
            }
            ("DELETE", "/api/v3/moviefile/70") => {
                deleted = true;
                json!({})
            }
            _ => return (404, "{}".into()),
        };
        (200, answer.to_string())
    })
}

fn deletes(server: &Server) -> Vec<String> {
    server.requests().into_iter().filter(|request| request.method == "DELETE").map(|request| request.path).collect()
}

#[tokio::test]
async fn two_radarrs_land_on_their_own_cards_and_a_delete_reaches_only_the_instance_named() {
    let (_turn, dir) = state_dir("instances").await;
    // Tag 1 is the keep tag in the 4K Radarr only: tag ids are per instance.
    let (hd, uhd, sonarr) = (radarr(10 * GIB, "anime", "HDHASH"), radarr(60 * GIB, "keep", "UHDHASH"), fake::empty());
    let args = args(vec![
        connection(App::Radarr, "", &hd.base),
        connection(App::Sonarr, "", &sonarr.base),
        connection(App::Radarr, "4k", &uhd.base),
    ]);
    let http = reqwest::Client::new();
    let fetched = fetch_inventory(&http, &args, "keep").await.expect("every library reads");

    let cards: Vec<(String, u64, bool)> =
        fetched.movies.iter().filter_map(|movie| movie.to_card()).map(|card| (card.id, card.size_bytes, card.is_favorite)).collect();
    assert_eq!(cards, [("radarr-7".to_string(), 10 * GIB, false), ("radarr@4k-7".to_string(), 60 * GIB, true)]);
    // Each instance's history names the download behind its own copy.
    assert_eq!(fetched.downloads.get("radarr-7"), Some(&vec!["hdhash".to_string()]));
    assert_eq!(fetched.downloads.get("radarr@4k-7"), Some(&vec!["uhdhash".to_string()]));
    for (server, key) in [(&hd, "key-radarr"), (&uhd, "key-radarr@4k"), (&sonarr, "key-sonarr")] {
        let requests = server.requests();
        assert!(!requests.is_empty() && requests.iter().all(|request| request.api_key.as_deref() == Some(key)), "{key}: {requests:?}");
    }

    // The native executor's routing: the card id names the instance.
    let target = ArrRef::card("radarr@4k-7").expect("a card id");
    let arr = args.arr(target.app, target.instance).expect("the 4K Radarr is configured");
    let evicted = Radarr::new(&http, &arr.base, &arr.key, false).evict(target.id, DeleteMode::FileAndUnmonitor, false).await;
    assert!(matches!(evicted, Ok(Evicted::Done(_))), "{evicted:?}");
    assert_eq!(deletes(&uhd), ["/api/v3/moviefile/70"]);
    assert!(deletes(&hd).is_empty(), "the HD copy stays");
    std::fs::remove_dir_all(&dir).ok();
}
