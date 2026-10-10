//! An install upgraded from one Radarr and one Sonarr: each state file as it
//! was written before instances existed (no `instance` fields, no `extra`
//! slots) reads on, its card ids still name the default instances, and
//! adding an instance later reads only the new one.

use super::fake::{self, Server};
use super::{args, connection, now, state_dir};
use crate::history;
use flinch_archive::arr::{ArrMovie, ArrSeries};
use flinch_archive::capacity::{App, AppDisks, EvictionLedger, LibraryVolumes, RecycleBin, RootFolder, Volume};
use flinch_archive::executor::NativeState;
use flinch_archive::ids::ArrRef;
use flinch_archive::presence::Span;
use flinch_archive::quality::churn::{self, EventCache};
use flinch_archive::trash::TrashState;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;

fn write(dir: &Path, name: &str, contents: serde_json::Value) {
    std::fs::write(dir.join(name), contents.to_string()).expect("an old state file");
}

fn library() -> (Vec<ArrMovie>, Vec<ArrSeries>) {
    let heat = json!({ "id": 7, "title": "Heat", "hasFile": true, "sizeOnDisk": 5000 });
    let show = json!({
        "id": 3, "title": "Show", "seriesType": "standard",
        "seasons": [{ "seasonNumber": 2, "statistics": { "episodeFileCount": 8, "sizeOnDisk": 900 } }],
    });
    (vec![serde_json::from_value(heat).expect("a movie")], vec![serde_json::from_value(show).expect("a show")])
}

#[tokio::test]
async fn arr_history_from_before_instances_serves_the_defaults_and_a_new_instance_is_read_alone() {
    let (_turn, dir) = state_dir("upgrade-history").await;
    let now = now();
    write(
        &dir,
        "arr-history.json",
        json!({
            "radarr": { "refreshed_at": now, "records": 2, "items": { "radarr-7": [{ "from": 1000 }] },
                        "removals": [], "downloads": { "radarr-7": ["hdhash"] } },
            "sonarr": { "refreshed_at": now, "records": 1, "items": { "sonarr-3-s2": [{ "from": 2000 }] },
                        "removals": [], "downloads": {} },
        }),
    );
    let (radarr, sonarr): (Server, Server) = (fake::empty(), fake::empty());
    let defaults = vec![connection(App::Radarr, "", &radarr.base), connection(App::Sonarr, "", &sonarr.base)];
    let (mut movies, mut series) = library();
    let http = reqwest::Client::new();

    let attached = history::attach(&http, &args(defaults.clone()), &mut movies, &mut series).await;
    assert_eq!(movies[0].on_disk, [Span { from: 1000, to: None }]);
    assert_eq!(series[0].seasons[0].on_disk, [Span { from: 2000, to: None }]);
    assert_eq!(attached.downloads.get("radarr-7"), Some(&vec!["hdhash".to_string()]));
    assert!(radarr.requests().is_empty() && sonarr.requests().is_empty(), "a fresh cache is not read again");

    // A 4K Radarr added later: only it is read, into a slot of its own.
    let uhd = fake::empty();
    let mut arrs = defaults;
    arrs.push(connection(App::Radarr, "4k", &uhd.base));
    history::attach(&http, &args(arrs), &mut movies, &mut series).await;
    assert!(!uhd.requests().is_empty());
    assert!(radarr.requests().is_empty() && sonarr.requests().is_empty());
    let cache: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("arr-history.json")).expect("rewritten")).expect("the cache is JSON");
    assert_eq!(cache["radarr"]["items"]["radarr-7"], json!([{ "from": 1000 }]), "the default slot stays where it was");
    assert!(cache["extra"]["radarr@4k"].is_object(), "{cache}");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn trash_grabs_evictions_and_native_state_from_before_instances_read_as_the_defaults() {
    let (_turn, dir) = state_dir("upgrade-state").await;
    let now = now();
    write(
        &dir,
        "trash.json",
        json!({
            "enabled": true, "guide_commit": "abc", "refreshed_at_unix": 1,
            "apps": [{ "app": "radarr", "compact_profile_id": 9, "managed_profile_ids": [4, 9] }, { "app": "sonarr", "compact_profile_id": 3 }],
            "last_apply": { "at_unix": 1, "requested": ["radarr:sizes:movie"],
                            "apps": [{ "app": "radarr", "applied": ["radarr:sizes:movie"], "failed": [], "unverified": [], "printed": [] }] },
        }),
    );
    let grab = |download: &str| json!({ "card_id": "radarr-7", "download": download, "at": now - 100, "kind": "grab" });
    write(&dir, "arr-grabs.json", json!({ "radarr": { "read_at": now, "events": [grab("a"), grab("b"), grab("c")] }, "sonarr": null }));
    write(
        &dir,
        "evictions.json",
        json!({
            "entries": { "radarr-7": { "app": "radarr", "volume": "/data", "bytes": 5000, "handed_at": now - 86_400, "title": "Heat" } },
            "handoffs": { "radarr-7": now - 86_400 },
        }),
    );
    write(
        &dir,
        "native.json",
        json!({
            "leaving": { "sonarr-3-s2": { "title": "Show S2", "kind": "SEASON", "bytes": 900, "announced_at": now - 10,
                                          "until": now + 86_400, "shelf": "Leaving Soon", "collection": "c1", "rating_key": "55" } },
            "deleted": [{ "id": "radarr-7", "title": "Heat", "kind": "MOVIE", "bytes": 5000, "deleted_at": now - 86_400, "announced": false,
                          "target": { "app": "radarr", "radarr_id": 7, "tmdb_id": 949, "mode": "file_and_unmonitor",
                                      "quality_profile_id": 1, "root_folder_path": "/movies" },
                          "restored_at": null }],
        }),
    );

    let trash = TrashState::read(&dir).expect("trash.json reads");
    assert_eq!(trash.instance(App::Radarr, "").and_then(|state| state.compact_profile_id), Some(9));
    assert_eq!(trash.instance(App::Sonarr, "").and_then(|state| state.compact_profile_id), Some(3));
    assert!(trash.last_apply.as_ref().is_some_and(|apply| apply.apps[0].instance.is_empty()));

    let grabs: EventCache = crate::read_state(&dir.join("arr-grabs.json"));
    assert!(grabs.is_fresh(App::Radarr, "", now) && !grabs.is_fresh(App::Radarr, "4k", now));
    let churned: Vec<String> = churn::detect(grabs.events(), 2, now).into_iter().map(|churn| churn.card_id).collect();
    assert_eq!(churned, ["radarr-7"]);

    // Another filesystem mounted at the same path in a 4K Radarr is a disk
    // of its own; the old entry's volume is still the default instance's,
    // whose bin is off.
    let disk = |instance: &str, total_bytes: u64, recycle| AppDisks {
        app: App::Radarr,
        instance: instance.into(),
        diskspace: vec![Volume { path: "/data".into(), total_bytes, free_bytes: 50_000 }],
        root_folders: vec![RootFolder { path: "/data/movies".into(), free_bytes: None }],
        recycle,
    };
    let volumes = LibraryVolumes::build(&[disk("", 100_000, RecycleBin::Disabled), disk("4k", 400_000, RecycleBin::Days(3))], |_| None);
    assert_eq!(volumes.volume_of(App::Radarr, "", "/data/movies/Heat"), Some("/data"));
    assert_eq!(volumes.volume_of(App::Radarr, "4k", "/data/movies/Heat"), Some("/data (radarr@4k)"));
    let mut ledger = EvictionLedger::read(&dir.join("evictions.json"));
    ledger.observe(|_| false, |id| volumes.recycle_secs_of(id), &BTreeMap::new(), now);
    assert_eq!(ledger.credits().get("/data").map(|credit| credit.pending), Some(5000));

    let native = NativeState::read(&dir);
    let deleted = native.restorable("radarr-7", now).expect("a restorable delete");
    let restore = ArrRef::card(&deleted.id).expect("a card id");
    let defaults = args(vec![connection(App::Radarr, "", "http://radarr"), connection(App::Radarr, "4k", "http://radarr-4k")]);
    assert_eq!(defaults.arr(restore.app, restore.instance).map(|arr| arr.base.as_str()), Some("http://radarr"));
    let leaving = native.leaving.keys().filter_map(|id| ArrRef::card(id)).collect::<Vec<_>>();
    assert_eq!((leaving[0].app, leaving[0].instance, leaving[0].id, leaving[0].season), (App::Sonarr, "", 3, Some(2)));
    std::fs::remove_dir_all(&dir).ok();
}
