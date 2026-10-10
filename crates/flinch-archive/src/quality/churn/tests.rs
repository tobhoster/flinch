use super::*;
use rstest::rstest;
use serde_json::{json, Value};

const NOW: u64 = 1_800_000_000;
const DAY: u64 = 86_400;

/// One `HistoryResource` row as `history/since` writes it.
fn radarr_row(id: u64, movie: u32, days_ago: u64, download: Option<&str>) -> Value {
    json!({"id": id, "movieId": movie, "eventType": "grabbed", "date": presence::format_utc(NOW - days_ago * DAY),
           "downloadId": download, "sourceTitle": "Heat.1995.2160p", "data": {"indexer": "Idx"}})
}

fn sonarr_row(id: u64, series: u32, season: Option<u32>, download: &str) -> Value {
    let episode = season.map(|season| json!({"id": id, "seasonNumber": season, "episodeNumber": id}));
    json!({"id": id, "seriesId": series, "episodeId": id, "episode": episode, "eventType": "grabbed",
           "date": presence::format_utc(NOW - DAY), "downloadId": download})
}

#[test]
fn rows_name_the_card_and_download_and_skip_what_cannot_be_placed() {
    let radarr = parse_events(
        App::Radarr,
        "",
        EventKind::Grab,
        vec![radarr_row(1, 7, 2, Some("ABC")), radarr_row(2, 7, 1, None), radarr_row(3, 0, 1, Some("X")), json!({"id": "junk"})],
    );
    let read: Vec<(&str, &str, u64)> = radarr.iter().map(|event| (event.card_id.as_str(), event.download.as_str(), event.at)).collect();
    assert_eq!(read, [("radarr-7", "ABC", NOW - 2 * DAY), ("radarr-7", "record:2", NOW - DAY)]);

    let sonarr = parse_events(App::Sonarr, "anime", EventKind::Import, vec![sonarr_row(10, 4, Some(2), "P"), sonarr_row(11, 4, None, "P")]);
    assert_eq!(
        sonarr,
        [Event { card_id: "sonarr@anime-4-s2".to_string(), download: "P".to_string(), at: NOW - DAY, kind: EventKind::Import }]
    );
}

fn event(card: &str, download: &str, days_ago: u64, kind: EventKind) -> Event {
    Event { card_id: card.to_string(), download: download.to_string(), at: NOW - days_ago * DAY, kind }
}

fn grabs(card: &str, count: u64, days_ago: u64) -> Vec<Event> {
    (0..count).map(|n| event(card, &format!("{card}-{days_ago}-{n}"), days_ago, EventKind::Grab)).collect()
}

/// Grabs within 30 days above the limit flag the item; what counts is the
/// number of distinct downloads.
#[rstest]
#[case::at_the_limit(grabs("radarr-1", 5, 3), Vec::new())]
#[case::above_the_limit(grabs("radarr-1", 6, 3), vec![("radarr-1", 6)])]
#[case::older_grabs_do_not_count([grabs("radarr-1", 4, 40), grabs("radarr-1", 4, 3)].concat(), Vec::new())]
#[case::a_season_pack_is_one_grab(
    (0..12).map(|_| event("sonarr-4-s1", "PACK", 2, EventKind::Grab)).chain(grabs("sonarr-4-s1", 4, 1)).collect(),
    Vec::new()
)]
#[case::imports_are_not_grabs((0..9).map(|n| event("radarr-2", &n.to_string(), 1, EventKind::Import)).collect(), Vec::new())]
fn churn_counts_distinct_grabs_within_the_window(#[case] events: Vec<Event>, #[case] flagged: Vec<(&str, u32)>) {
    let churn = detect(&events, 5, NOW);
    let read: Vec<(&str, u32)> = churn.iter().map(|churn| (churn.card_id.as_str(), churn.grabs)).collect();
    assert_eq!(read, flagged);
}

#[test]
fn churn_reports_imports_and_the_last_grab_most_grabbed_first() {
    let mut events = [grabs("radarr-1", 7, 10), grabs("radarr-2", 9, 2), grabs("radarr-2", 1, 1)].concat();
    events.extend((0..3).map(|n| event("radarr-1", &format!("radarr-1-10-{n}"), 9, EventKind::Import)));
    let churn = detect(&events, 5, NOW);
    assert_eq!(
        churn,
        [
            Churn { card_id: "radarr-2".to_string(), grabs: 10, imports: 0, last_grab_at: NOW - DAY },
            Churn { card_id: "radarr-1".to_string(), grabs: 7, imports: 3, last_grab_at: NOW - 10 * DAY },
        ]
    );
}

#[rstest]
#[case::never_read(None, false)]
#[case::read_an_hour_ago(Some(NOW - 3_600), true)]
#[case::read_seven_hours_ago(Some(NOW - 7 * 3_600), false)]
#[case::read_in_the_future(Some(NOW + 60), false)]
fn a_read_serves_six_hours(#[case] read_at: Option<u64>, #[case] fresh: bool) {
    let mut cache = EventCache::default();
    if let Some(read_at) = read_at {
        cache.store(App::Sonarr, "anime", EventRead { read_at, events: Vec::new() });
    }
    assert_eq!(cache.is_fresh(App::Sonarr, "anime", NOW), fresh);
    assert!(!cache.is_fresh(App::Sonarr, "", NOW), "the default instance's read is its own");
    assert!(!cache.is_fresh(App::Radarr, "", NOW));
}

#[test]
fn upgrades_off_on_a_profile_is_per_instance() {
    let mut ledger = GuardLedger::default();
    ledger.applied.insert("radarr-1".into(), Applied { action: GuardAction::UpgradesOff, at: NOW, profile: Some(4) });
    assert!(ledger.profile_off("radarr-2", 4));
    assert!(!ledger.profile_off("radarr@4k-2", 4), "4k's profile 4 is another profile");
}

#[test]
fn the_window_query_asks_for_one_event_type_since_thirty_days_ago() {
    let path = events_path(App::Sonarr, EventKind::Grab, NOW);
    assert_eq!(path, format!("/api/v3/history/since?date={}&eventType=1&includeEpisode=true", presence::format_utc(NOW - WINDOW_SECS)));
    assert!(events_path(App::Radarr, EventKind::Import, NOW).ends_with("&eventType=3"));
}
