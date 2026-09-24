//! Presence spans from *arr history: what a card's history says about when the
//! household had it on disk.

use super::*;
use crate::arr::history::HistoryRecord;
use crate::outside::RemovalReason;
use rstest::rstest;

const DAY: u64 = 86_400;
/// 2026-03-14T00:00:00Z.
const T0: u64 = 1_773_446_400;

fn at(days: u64) -> u64 {
    T0 + days * DAY
}

fn movie(days: u64, change: Change) -> FileEvent {
    FileEvent { at: at(days), record: days, episode: None, change }
}

fn episode(number: u32, days: u64, change: Change) -> FileEvent {
    FileEvent { at: at(days), record: days * 100 + u64::from(number), episode: Some(number), change }
}

fn open(days: u64) -> Span {
    Span { from: at(days), to: None }
}

fn closed(from: u64, to: u64) -> Span {
    Span { from: at(from), to: Some(at(to)) }
}

/// A movie (id 10) as Radarr's `/api/v3/history` returns it, newest first:
/// grabbed, imported, missing from disk after a library incident, grabbed and
/// imported again.
fn re_downloaded() -> Presence {
    let page = r#"[
        {"id":5,"movieId":10,"date":"2026-08-26T09:00:00Z","eventType":"downloadFolderImported","data":{"droppedPath":"/dl/Movie.mkv"}},
        {"id":4,"movieId":10,"date":"2026-08-21T08:00:00Z","eventType":"grabbed","data":{"indexer":"x"}},
        {"id":3,"movieId":10,"date":"2026-08-20T07:00:00Z","eventType":"movieFileDeleted","data":{"reason":"MissingFromDisk","releaseGroup":null}},
        {"id":2,"movieId":10,"date":"2026-03-14T06:00:00Z","eventType":"downloadFolderImported","data":{}},
        {"id":1,"movieId":10,"date":"2026-02-19T05:00:00Z","eventType":"grabbed","data":{}}
    ]"#;
    let records: Vec<HistoryRecord> = serde_json::from_str(page).expect("history page parses");
    let events: Vec<FileEvent> = records
        .iter()
        .filter_map(HistoryRecord::file_event)
        .map(|(card, event)| {
            assert_eq!(card, "radarr-10");
            event
        })
        .collect();
    derive(events, parse_utc("2026-08-26T09:00:00Z"))
}

#[test]
fn a_re_downloads_history_is_two_spans_split_by_the_incident() {
    let presence = re_downloaded();
    let spans = [
        Span { from: parse_utc("2026-03-14T06:00:00Z").expect("date"), to: parse_utc("2026-08-20T07:00:00Z") },
        Span { from: parse_utc("2026-08-26T09:00:00Z").expect("date"), to: None },
    ];
    assert_eq!(presence.spans, spans);
    assert!(!presence.fallback, "the re-download was recorded");
}

#[rstest]
#[case::before_it_arrived("2026-03-01T00:00:00Z", None)]
#[case::inside_the_first_span("2026-05-01T00:00:00Z", Some("2026-03-14T06:00:00Z"))]
#[case::in_the_gap("2026-08-23T00:00:00Z", None)]
#[case::at_the_removal("2026-08-20T07:00:00Z", None)]
#[case::after_the_re_download("2026-09-10T00:00:00Z", Some("2026-08-26T09:00:00Z"))]
fn a_cut_resolves_to_the_span_covering_it(#[case] cut: &str, #[case] arrived: Option<&str>) {
    let presence = re_downloaded();
    let cut = parse_utc(cut).expect("cut");
    assert_eq!(covering(&presence.spans, cut).map(|span| span.from), arrived.and_then(parse_utc));
}

#[rstest]
#[case::upgrade(Change::Replaced, vec![open(0)])]
#[case::deleted_by_hand(Change::Removed, vec![closed(0, 40), open(41)])]
fn only_a_removal_splits_a_movies_span(#[case] swap: Change, #[case] spans: Vec<Span>) {
    let events = vec![movie(0, Change::Imported), movie(40, swap), movie(41, Change::Imported)];
    assert_eq!(derive(events, Some(at(41))).spans, spans);
}

#[test]
fn radarr_and_sonarr_records_map_to_their_cards() {
    let rows = r#"[
        {"id":7,"movieId":3,"date":"2026-04-01T00:00:00Z","eventType":"movieFileDeleted","data":{"reason":"Upgrade"}},
        {"id":8,"seriesId":4,"episodeId":41,"episode":{"seasonNumber":2},"date":"2026-04-02T00:00:00Z","eventType":"episodeFileDeleted","data":{"reason":"Manual"}},
        {"id":9,"seriesId":4,"episodeId":42,"episode":{"seasonNumber":2},"date":"2026-04-03T00:00:00Z","eventType":"downloadFolderImported","data":{}},
        {"id":10,"seriesId":4,"episodeId":43,"date":"2026-04-04T00:00:00Z","eventType":"downloadFolderImported","data":{}},
        {"id":11,"movieId":3,"date":"2026-04-05T00:00:00Z","eventType":"movieFileRenamed","data":{}}
    ]"#;
    let records: Vec<HistoryRecord> = serde_json::from_str(rows).expect("records parse");
    let mapped: Vec<Option<(String, Option<u32>, Change)>> = records
        .iter()
        .map(|record| record.file_event().map(|(card, event)| (card, event.episode, event.change)))
        .collect();
    assert_eq!(
        mapped,
        [
            Some(("radarr-3".to_string(), None, Change::Replaced)),
            Some(("sonarr-4-s2".to_string(), Some(41), Change::Removed)),
            Some(("sonarr-4-s2".to_string(), Some(42), Change::Imported)),
            None, // no episode: the season is unknown
            None, // a rename moves no file in or out
        ]
    );
}

#[test]
fn only_a_file_that_left_is_a_removal() {
    let rows = r#"[
        {"id":1,"movieId":3,"date":"2026-09-20T16:07:46Z","eventType":"movieFileDeleted","data":{"reason":"Manual"}},
        {"id":2,"movieId":3,"date":"2026-09-20T16:07:46Z","eventType":"movieFileDeleted","data":{"reason":"Upgrade"}},
        {"id":3,"seriesId":4,"episodeId":41,"episode":{"seasonNumber":2},"date":"2026-08-17T02:20:40Z","eventType":"episodeFileDeleted","data":{"reason":"MissingFromDisk"}},
        {"id":4,"movieId":3,"date":"2026-08-29T06:06:01Z","eventType":"downloadFolderImported","data":{}}
    ]"#;
    let records: Vec<HistoryRecord> = serde_json::from_str(rows).expect("records parse");
    let removals: Vec<Option<(String, RemovalReason)>> =
        records.iter().map(|record| record.removal().map(|removal| (removal.card, removal.reason))).collect();
    assert_eq!(
        removals,
        [
            Some(("radarr-3".to_string(), RemovalReason::Manual)),
            None, // an upgrade swaps the file: nothing left
            Some(("sonarr-4-s2".to_string(), RemovalReason::MissingFromDisk)),
            None,
        ]
    );
}

#[test]
fn a_season_opens_at_its_first_episode_and_closes_when_every_episode_is_gone() {
    let events = vec![
        episode(1, 0, Change::Imported),
        episode(2, 7, Change::Imported),
        episode(1, 50, Change::Removed),
        episode(2, 60, Change::Removed),
        episode(1, 90, Change::Imported),
    ];
    assert_eq!(derive(events, Some(at(90))).spans, [closed(0, 60), open(90)]);
}

#[test]
fn a_season_that_removed_episodes_it_never_saw_arrive_stays_open_while_it_has_files() {
    // Episode 9 came in by disk scan (no record), so emptying the recorded
    // episodes at day 30 does not prove the season was ever empty.
    let events = vec![
        episode(1, 0, Change::Imported),
        episode(1, 30, Change::Removed),
        episode(9, 40, Change::Removed),
        episode(1, 60, Change::Imported),
    ];
    assert_eq!(derive(events, Some(at(60))).spans, [open(0)]);
}

#[rstest]
#[case::only_an_unrecorded_file_removed(vec![movie(10, Change::Removed)], Some(20), vec![open(20)], 1)]
#[case::re_added_by_disk_scan(vec![movie(0, Change::Imported), movie(10, Change::Removed)], Some(20), vec![closed(0, 10), open(20)], 0)]
#[case::a_file_older_than_the_removal_never_left(vec![movie(0, Change::Imported), movie(10, Change::Removed)], Some(5), vec![open(0)], 0)]
fn a_file_on_disk_with_no_open_span_opens_one_at_its_date(
    #[case] events: Vec<FileEvent>,
    #[case] file_days: Option<u64>,
    #[case] spans: Vec<Span>,
    #[case] undated: u32,
) {
    let presence = derive(events, file_days.map(at));
    assert_eq!(presence.spans, spans);
    assert!(presence.fallback);
    assert_eq!(presence.undated, undated);
}

#[test]
fn a_file_with_no_date_and_no_open_span_stays_undated() {
    let presence = derive(vec![movie(0, Change::Imported), movie(10, Change::Removed)], None);
    assert_eq!(presence, Presence { spans: vec![closed(0, 10)], fallback: false, undated: 1 });
}

#[test]
fn no_history_means_no_spans_so_callers_keep_todays_rule() {
    assert_eq!(derive(Vec::new(), Some(at(20))), Presence::default());
}

#[rstest]
#[case("2026-03-14", Some(T0))]
#[case("2026-01-01T00:00:00Z", Some(1_767_225_600))]
#[case("2024-02-29T12:34:56Z", Some(1_709_210_096))]
#[case("2026-08-20T10:11:12.1234567Z", Some(1_787_220_672))]
#[case("2026-08-20T10:11:12.5+02:00", Some(1_787_213_472))]
#[case::the_null_date("0001-01-01T00:00:00Z", None)]
#[case::before_1970("1969-12-31T23:59:59Z", None)]
#[case::not_a_date("yesterday", None)]
fn arr_timestamps_parse_to_unix_seconds(#[case] text: &str, #[case] epoch: Option<u64>) {
    assert_eq!(parse_utc(text), epoch);
}
