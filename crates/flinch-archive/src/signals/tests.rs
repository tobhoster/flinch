//! Wire parsing from the apps' real shapes, and the caches' refresh rules.

use super::arr::{parse_grabs, parse_queue, GrabCache, GrabRead, GRAB_REFRESH_SECS, GRAB_WINDOW_SECS};
use super::release::{parse_retention, ReleaseCache, Searched, RELEASE_TTL_SECS, SEARCH_BUDGET};
use super::seerr::{parse_requests, parse_users, parse_watchlist, User};
use super::*;
use crate::card::ArchiveCard;
use crate::golden::{golden_movie, golden_season};
use rstest::rstest;
use serde_json::{json, Value};

const DAY: u64 = 86_400;
/// 2026-10-04T00:00:00Z.
const NOW: u64 = 1_791_072_000;

fn array(value: Value) -> Vec<Value> {
    match value {
        Value::Array(rows) => rows,
        other => vec![other],
    }
}

/// A Sonarr grabbed record as `history/since?includeEpisode=true` writes it.
fn sonarr_grab(id: u64, episode: u32, season: u32, download_id: &str) -> Value {
    json!({
        "episodeId": 1000 + episode, "seriesId": 7, "sourceTitle": "Show.S02.1080p.WEB-DL",
        "quality": {"quality": {"id": 3, "name": "WEBDL-1080p"}},
        "date": "2026-09-30T21:14:05Z", "downloadId": download_id, "eventType": "grabbed",
        "data": {"indexer": "NZBgeek", "size": "48318382080", "protocol": "2", "downloadClient": "SABnzbd"},
        "episode": {"seriesId": 7, "seasonNumber": season, "episodeNumber": episode, "title": "Pilot"},
        "id": id
    })
}

#[test]
fn a_season_pack_counts_once_at_its_string_size() {
    let records = (1..=10).map(|episode| sonarr_grab(100 + u64::from(episode), episode, 2, "SABnzbd_nzo_abc")).collect();
    let grabs = parse_grabs(App::Sonarr, records);
    assert_eq!(
        grabs,
        vec![Grab {
            app: App::Sonarr,
            item: ItemRef::Series { series_id: 7, season: Some(2) },
            epoch: crate::presence::parse_utc("2026-09-30T21:14:05Z").unwrap_or_default(),
            bytes: 48_318_382_080,
        }]
    );
}

#[test]
fn a_multi_season_pack_names_no_season_and_records_without_a_download_stand_alone() {
    let records = vec![sonarr_grab(1, 1, 1, "hash"), sonarr_grab(2, 1, 2, "hash"), sonarr_grab(3, 1, 3, ""), sonarr_grab(4, 2, 3, "")];
    let items: Vec<ItemRef> = parse_grabs(App::Sonarr, records).into_iter().map(|grab| grab.item).collect();
    assert_eq!(
        items,
        [None, Some(3), Some(3)].map(|season| ItemRef::Series { series_id: 7, season }),
        "one pack across seasons 1–2, then two records with no download id"
    );
}

#[test]
fn radarr_grabs_skip_malformed_records_not_the_read() {
    let records = array(json!([
        {"movieId": 12, "date": "2026-09-20T08:00:00Z", "downloadId": "a", "eventType": "grabbed", "data": {"size": "8589934592"}, "id": 1},
        {"movieId": 13, "date": "2026-09-21T08:00:00Z", "downloadId": "b", "eventType": "grabbed", "data": {"size": 1}, "id": 2},
        {"movieId": 14, "date": "2026-09-22T08:00:00Z", "downloadId": "c", "eventType": "grabbed", "data": {}, "id": 3},
        {"movieId": 0, "date": "2026-09-22T08:00:00Z", "downloadId": "d", "eventType": "grabbed", "data": {"size": "5"}, "id": 4},
        "not a record",
        {"movieId": 15, "date": "2026-09-23T08:00:00Z", "downloadId": "e", "eventType": "grabbed", "data": {"size": " 42 "}, "id": 5}
    ]));
    let grabs: Vec<(ItemRef, u64)> = parse_grabs(App::Radarr, records).into_iter().map(|grab| (grab.item, grab.bytes)).collect();
    assert_eq!(grabs, vec![(ItemRef::Movie(12), 8_589_934_592), (ItemRef::Movie(15), 42)]);
}

#[test]
fn the_queue_counts_each_download_once_with_its_bytes_left() {
    let page: crate::arr::history::HistoryPage = serde_json::from_value(json!({
        "page": 1, "pageSize": 500, "sortKey": "timeleft", "sortDirection": "ascending", "totalRecords": 4,
        "records": [
            {"seriesId": 7, "episodeId": 1001, "seasonNumber": 2, "size": 48318382080.0, "sizeleft": 12079595520.5,
             "status": "downloading", "downloadId": "SABnzbd_nzo_abc", "protocol": "usenet", "id": 11},
            {"seriesId": 7, "episodeId": 1002, "seasonNumber": 2, "size": 48318382080.0, "sizeleft": 12079595520.5,
             "status": "downloading", "downloadId": "SABnzbd_nzo_abc", "protocol": "usenet", "id": 12},
            {"seriesId": 9, "episodeId": 2001, "episode": {"seasonNumber": 1}, "size": 100, "sizeleft": -3,
             "status": "completed", "downloadId": "hash", "protocol": "torrent", "id": 13},
            {"seriesId": 9, "size": 100, "sizeleft": "lots", "downloadId": "other", "id": 14}
        ]
    }))
    .unwrap_or_else(|error| panic!("queue page: {error}"));
    assert_eq!(page.total_records, 4);
    assert_eq!(
        parse_queue(App::Sonarr, page.records),
        vec![
            Queued { app: App::Sonarr, item: ItemRef::Series { series_id: 7, season: Some(2) }, bytes_left: 12_079_595_520 },
            Queued { app: App::Sonarr, item: ItemRef::Series { series_id: 9, season: Some(1) }, bytes_left: 0 },
        ]
    );
}

fn grab(epoch: u64) -> Grab {
    Grab { app: App::Radarr, item: ItemRef::Movie(1), epoch, bytes: 1 }
}

#[rstest]
#[case::just_read(NOW, true)]
#[case::almost_stale(NOW - GRAB_REFRESH_SECS + 1, true)]
#[case::stale(NOW - GRAB_REFRESH_SECS, false)]
#[case::from_the_future(NOW + 60, false)]
fn a_grab_read_serves_six_hours(#[case] read_at: u64, #[case] fresh: bool) {
    let cache = GrabCache { radarr: Some(GrabRead { read_at, grabs: Vec::new() }), sonarr: None };
    assert_eq!(cache.is_fresh(App::Radarr, NOW), fresh);
    assert!(!cache.is_fresh(App::Sonarr, NOW), "never read is never fresh");
}

#[test]
fn cached_grabs_leave_the_window_as_it_moves() {
    let since = NOW - GRAB_WINDOW_SECS;
    let cache = GrabCache {
        radarr: Some(GrabRead { read_at: NOW - 3_600, grabs: vec![grab(since - 1), grab(since), grab(NOW - DAY)] }),
        sonarr: None,
    };
    assert_eq!(cache.grabs(NOW), vec![grab(since), grab(NOW - DAY)]);
}

#[test]
fn every_request_but_a_declined_one_counts() {
    let page: seerr::Page = serde_json::from_value(json!({
        "pageInfo": {"pages": 1, "pageSize": 100, "results": 4, "page": 1},
        "results": [
            {"id": 1, "status": 2, "type": "movie", "is4k": false,
             "media": {"id": 5, "mediaType": "movie", "tmdbId": 949, "tvdbId": null, "status": 5},
             "seasons": [], "requestedBy": {"id": 3, "displayName": "Mara", "plexUsername": "mara_p", "email": "mara@example.org"}},
            {"id": 2, "status": 3, "type": "movie",
             "media": {"id": 6, "mediaType": "movie", "tmdbId": 550, "status": 1},
             "seasons": [], "requestedBy": {"id": 3, "displayName": "Mara"}},
            {"id": 3, "status": 1, "type": "tv",
             "media": {"id": 7, "mediaType": "tv", "tmdbId": 1396, "tvdbId": 81189, "status": 3},
             "seasons": [{"id": 9, "seasonNumber": 2, "status": 1}, {"id": 10, "seasonNumber": 3, "status": 1}],
             "requestedBy": {"id": 4, "displayName": "", "username": "theo"}},
            {"id": 4, "status": 5, "media": {"mediaType": "movie"}, "seasons": [], "requestedBy": {"id": 3, "displayName": "Mara"}}
        ]
    }))
    .unwrap_or_else(|error| panic!("request page: {error}"));
    assert!(page.is_last(0));
    assert_eq!(
        parse_requests(page.results),
        vec![
            Request { media: MediaRef::Movie { tmdb: 949 }, seasons: Vec::new(), requester: "Mara".to_owned() },
            Request { media: MediaRef::Show { tvdb: Some(81_189), tmdb: Some(1396) }, seasons: vec![2, 3], requester: "theo".to_owned() },
        ]
    );
}

#[rstest]
#[case::short_page(0, 40, 140, true)]
#[case::full_page_more_left(0, 100, 140, false)]
#[case::full_page_at_the_end(100, 100, 200, true)]
fn a_seerr_page_is_last_when_short_or_at_the_total(#[case] skip: usize, #[case] rows: usize, #[case] total: usize, #[case] last: bool) {
    let page = seerr::Page { page_info: seerr::PageInfo { results: total }, results: vec![Value::Null; rows] };
    assert_eq!(page.is_last(skip), last);
}

#[test]
fn users_and_watchlists_parse_by_display_name() {
    let users = parse_users(array(json!([
        {"id": 1, "displayName": "Mara", "email": "mara@example.org", "userType": 1},
        {"id": 0, "displayName": "Ghost"},
        {"id": 2, "plexUsername": "theo"}
    ])));
    assert_eq!(users, vec![User { id: 1, name: "Mara".to_owned() }, User { id: 2, name: "theo".to_owned() }]);

    let page: seerr::WatchlistPage = serde_json::from_value(json!({
        "page": 1, "totalPages": 1, "totalResults": 3,
        "results": [
            {"id": 1, "ratingKey": "5d77", "title": "Heat", "mediaType": "movie", "tmdbId": 949},
            {"id": 2, "ratingKey": "5d78", "title": "The Wire", "mediaType": "tv", "tmdbId": 1438},
            {"id": 3, "ratingKey": "5d79", "title": "Unmatched", "mediaType": "movie"}
        ]
    }))
    .unwrap_or_else(|error| panic!("watchlist page: {error}"));
    assert_eq!(page.total_pages, 1);
    assert_eq!(
        parse_watchlist("Mara", page.results),
        vec![
            Watchlisted { media: MediaRef::Movie { tmdb: 949 }, user: "Mara".to_owned() },
            Watchlisted { media: MediaRef::Show { tvdb: None, tmdb: Some(1438) }, user: "Mara".to_owned() },
        ]
    );
}

fn sab(servers: Value) -> Value {
    json!({"config": {"servers": servers}})
}

#[rstest]
#[case::one_unlimited_enabled(sab(json!([{"name": "a", "enable": 1, "retention": 1200}, {"name": "b", "enable": 1, "retention": 0}])), Some(0))]
#[case::longest_enabled(sab(json!([{"enable": 1, "retention": 1200}, {"enable": "1", "retention": "3000"}, {"enable": 0, "retention": 5000}])), Some(3000))]
#[case::unlimited_but_disabled(sab(json!([{"enable": false, "retention": 0}, {"enable": true, "retention": 900}])), Some(900))]
#[case::no_enabled_server(sab(json!([{"enable": 0, "retention": 0}])), None)]
#[case::no_servers(sab(json!([])), None)]
#[case::error_answer(json!({"status": false, "error": "API Key Incorrect"}), None)]
fn sab_retention(#[case] config: Value, #[case] retention: Option<u32>) {
    assert_eq!(parse_retention(&config), retention);
}

/// One Prowlarr result as `/api/v1/search` writes it.
fn result(protocol: &str, seeders: Option<u32>, age: u32) -> Value {
    json!({"guid": "x", "age": age, "ageHours": f64::from(age) * 24.0, "size": 8589934592_u64, "indexerId": 1, "indexer": "Idx",
           "title": "Heat.1995.1080p", "protocol": protocol, "seeders": seeders, "leechers": seeders.map(|_| 1)})
}

#[test]
fn seeders_are_the_best_torrent_and_zero_when_searched_and_none_found() {
    let found =
        Searched::from_results(vec![result("torrent", Some(4), 10), result("torrent", None, 3), result("torrent", Some(31), 900)], NOW);
    assert_eq!(found.release(None).seeders, Some(31));
    let usenet_only = Searched::from_results(vec![result("usenet", None, 100), json!({"title": "no protocol"})], NOW);
    assert_eq!(usenet_only.release(None).seeders, Some(0));
    assert_eq!(Searched::from_results(Vec::new(), NOW).release(Some(0)), Release { seeders: Some(0), usenet_out_of_retention: Some(true) });
}

#[rstest]
#[case::retention_unknown(vec![], None, None)]
#[case::unlimited_with_a_post(vec![result("usenet", None, 5000)], Some(0), Some(false))]
#[case::unlimited_without_a_post(vec![result("torrent", Some(9), 1)], Some(0), Some(true))]
#[case::limited_and_inside(vec![result("usenet", None, 4000), result("usenet", None, 1200)], Some(1200), Some(false))]
#[case::limited_and_past(vec![result("usenet", None, 1201)], Some(1200), Some(true))]
#[case::limited_without_a_post(vec![], Some(1200), Some(true))]
fn usenet_retention_verdict(#[case] results: Vec<Value>, #[case] retention: Option<u32>, #[case] out: Option<bool>) {
    assert_eq!(Searched::from_results(results, NOW).release(retention).usenet_out_of_retention, out);
}

fn movie(id: u32) -> ArchiveCard {
    ArchiveCard { id: format!("radarr-{id}"), ..golden_movie() }
}

fn searched(searched_at: u64) -> Searched {
    Searched { searched_at, seeders: 3, youngest_usenet_days: None }
}

#[test]
fn due_searches_take_missing_cards_then_the_oldest_stale_ones_within_the_budget() {
    let cards: Vec<ArchiveCard> = (1..=40).map(movie).collect();
    let mut cache = ReleaseCache::default();
    // 1..=25 searched: 1..=5 fresh, 6..=25 stale and older the higher the id.
    for id in 1..=25_u64 {
        let age = if id <= 5 { DAY } else { RELEASE_TTL_SECS + id * DAY };
        cache.insert(format!("radarr-{id}"), searched(NOW - age));
    }
    let due: Vec<&str> = cache.due(&cards, NOW).into_iter().map(|card| card.id.as_str()).collect();
    let mut expected: Vec<String> = (26..=40).map(|id| format!("radarr-{id}")).collect();
    expected.extend((21..=25).rev().map(|id| format!("radarr-{id}")));
    assert_eq!(due.len(), SEARCH_BUDGET);
    assert_eq!(due, expected);
}

#[rstest]
#[case::fresh(NOW - RELEASE_TTL_SECS + 1, false)]
#[case::expired(NOW - RELEASE_TTL_SECS, true)]
#[case::from_the_future(NOW + 60, true)]
fn a_search_serves_seven_days(#[case] searched_at: u64, #[case] due: bool) {
    let mut cache = ReleaseCache::default();
    cache.insert("radarr-1".to_owned(), searched(searched_at));
    assert_eq!(cache.due(&[movie(1)], NOW).len(), usize::from(due));
}

#[test]
fn a_season_without_its_show_title_is_never_searched_and_gone_cards_are_forgotten() {
    let untitled = ArchiveCard { id: "sonarr-7-s2".to_owned(), show_title: None, ..golden_season() };
    let titled = ArchiveCard { id: "sonarr-7-s3".to_owned(), season_index: Some(3), ..golden_season() };
    let mut cache = ReleaseCache::default();
    let cards = [untitled, titled.clone()];
    let due: Vec<&str> = cache.due(&cards, NOW).into_iter().map(|card| card.id.as_str()).collect();
    assert_eq!(due, ["sonarr-7-s3"]);
    assert_eq!(super::release::search_query(&titled), Some(("The MVP Sessions S03".to_owned(), "tvsearch")));

    cache.insert("sonarr-7-s3".to_owned(), searched(NOW));
    cache.insert("radarr-99".to_owned(), searched(NOW));
    cache.retain_cards(&[titled]);
    assert_eq!(cache.releases(None).into_keys().collect::<Vec<_>>(), ["sonarr-7-s3"]);
}
