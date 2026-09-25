//! Tautulli parsing, joins and the absence claim.

use super::*;
use crate::ids::ExternalIds;
use crate::plex::{resolve, PlexLibrary, PlexMetadata, Resolution};
use rstest::rstest;

const DAY: u64 = 86_400;
const NOW: u64 = 1_800_000_000;

fn stream(json: &str) -> TautulliRow {
    serde_json::from_str(json).expect("stream fixture")
}

fn movie_stream(rating_key: &str, date: u64, percent: u32) -> TautulliRow {
    stream(&format!(
        r#"{{"media_type":"movie","rating_key":"{rating_key}","title":"Film","date":"{date}","percent_complete":"{percent}"}}"#
    ))
}

fn target(id: &str, tmdb: u32, added_days_ago: u64) -> WatchTarget {
    WatchTarget {
        id: id.to_string(),
        kind: LibraryKind::Movie,
        title: "Film".to_string(),
        year: Some(2020),
        show_title: None,
        season_index: None,
        episodes_total: None,
        episode_files: None,
        external: ExternalIds { tmdb: Some(tmdb), ..ExternalIds::default() },
        added_epoch: Some(NOW - added_days_ago * DAY),
        on_disk: true,
    }
}

/// Plex holds tmdb 1 as ratingKey "1" and tmdb 2 as ratingKey "2".
fn resolved(targets: &[WatchTarget]) -> Resolution {
    let rows: Vec<PlexMetadata> = (1..=2)
        .map(|id| {
            serde_json::from_str(&format!(r#"{{"ratingKey":"{id}","title":"Film {id}","Guid":[{{"id":"tmdb://{id}"}}]}}"#))
                .expect("plex row")
        })
        .collect();
    resolve(targets, &PlexLibrary::new(&rows, &[], &[]))
}

fn healthy() -> EvidenceHealth {
    EvidenceHealth {
        plex_configured: true,
        plex_items_ok: true,
        plex_history_complete: true,
        tautulli_configured: true,
        tautulli_complete: true,
        multi_account: false,
        plex_settings_unpaired: false,
    }
}

/// Tautulli recording steadily from 400 days ago until yesterday, on another item.
fn background() -> Vec<TautulliRow> {
    (0..=40).map(|step| movie_stream("999", NOW - DAY - step * 10 * DAY, 100)).collect()
}

#[test]
fn the_paged_and_plain_response_shapes_both_parse() {
    let paged = r#"{"response":{"result":"success","data":{"recordsFiltered":1204,"recordsTotal":1300,"data":[{"media_type":"movie","title":"Superman","date":"1790000000","percent_complete":"100","rating_key":"78"}]}}}"#;
    let page = parse_history_page(paged).expect("paged shape");
    assert_eq!(page.records_filtered, Some(1204));
    assert_eq!(page.rows[0].rating_key, "78");
    let plain = r#"{"response":{"result":"success","data":[{"media_type":"movie","title":"Superman","date":"1790000000"}]}}"#;
    assert_eq!(parse_history_page(plain).expect("plain shape").records_filtered, None);
    // Numbers instead of strings, as some versions send them.
    let numeric = r#"{"response":{"data":{"data":[{"media_type":"movie","date":1790000000,"rating_key":78,"year":1978}]}}}"#;
    let rows = parse_history_page(numeric).expect("numeric shape").rows;
    assert_eq!((rows[0].epoch(), rows[0].rating_key.as_str(), rows[0].year.as_str()), (Some(1_790_000_000), "78", "1978"));
    // A broken body is not a page, never a panic.
    assert!(parse_history_page("<xml>nope</xml>").is_none());
}

#[test]
fn streams_join_by_rating_key_and_a_partial_one_is_partial_evidence() {
    let targets = [target("radarr-1", 1, 400), target("radarr-2", 2, 400)];
    let resolution = resolved(&targets);
    let rows = [movie_stream("1", NOW - 5 * DAY, 100), movie_stream("2", NOW - 3 * DAY, 40), movie_stream("3", NOW - DAY, 100)];
    let entries = plays_by_target(&targets, &resolution, &rows);
    assert!((entries["radarr-1"].progress - 1.0).abs() < 1e-6);
    let partial = &entries["radarr-2"];
    assert_eq!(partial.last_watched_epoch, Some(NOW - 3 * DAY), "the stopped stream still dates the last touch");
    assert!(partial.progress > 0.0 && partial.progress < 0.999, "touched, not finished: {}", partial.progress);
    assert_eq!(entries.len(), 2, "ratingKey 3 belongs to no target");
}

#[test]
fn a_remake_does_not_inherit_the_original_s_streams() {
    // Radarr's 2025 film resolves to ratingKey "2"; the stream is of "1978"'s "1".
    let remake = target("radarr-2025", 2, 400);
    let resolution = resolved(std::slice::from_ref(&remake));
    let original =
        stream(r#"{"media_type":"movie","rating_key":"1","title":"Film","year":"1978","date":"1790000000","percent_complete":"100"}"#);
    assert!(plays_by_target(&[remake], &resolution, &[original]).is_empty());
}

#[test]
fn without_plex_an_unresolved_movie_falls_back_to_title_and_year_only() {
    let unresolved = Resolution::default();
    let film = WatchTarget { title: "Superman".into(), year: Some(2025), ..target("radarr-1", 7, 400) };
    let rows = [
        stream(r#"{"media_type":"movie","title":"Superman","year":"1978","date":"1790000000"}"#),
        stream(r#"{"media_type":"movie","title":"Superman","date":"1790000100"}"#),
        stream(r#"{"media_type":"movie","title":"Superman","year":"2025","date":"1790000200"}"#),
    ];
    let entries = plays_by_target(std::slice::from_ref(&film), &unresolved, &rows);
    assert_eq!(entries["radarr-1"].last_watched_epoch, Some(1_790_000_200), "only the 2025 stream is the 2025 film's");
}

#[test]
fn silence_about_a_resolved_long_held_item_is_absence() {
    let targets = [target("radarr-1", 1, 200)];
    let absence = absence_by_target(&targets, &resolved(&targets), &background(), &healthy(), NOW);
    assert_eq!(absence["radarr-1"].source, WatchSource::TautulliAbsence);
}

#[rstest]
#[case::unresolved_in_plex(target("radarr-9", 9, 200), background(), healthy())]
#[case::a_partial_stream(target("radarr-1", 1, 200), { let mut rows = background(); rows.push(movie_stream("1", NOW - 90 * DAY, 12)); rows }, healthy())]
#[case::plex_failed_this_cycle(target("radarr-1", 1, 200), background(), EvidenceHealth { plex_items_ok: false, ..healthy() })]
#[case::tautulli_truncated(target("radarr-1", 1, 200), background(), EvidenceHealth { tautulli_complete: false, ..healthy() })]
#[case::arrived_before_coverage(target("radarr-1", 1, 500), background(), healthy())]
#[case::arrived_too_recently(target("radarr-1", 1, 10), background(), healthy())]
#[case::no_arrival_date(WatchTarget { added_epoch: None, ..target("radarr-1", 1, 200) }, background(), healthy())]
#[case::tautulli_stopped_recording(target("radarr-1", 1, 200), background().into_iter().filter(|row| row.epoch().is_some_and(|epoch| epoch < NOW - 100 * DAY)).collect(), healthy())]
fn absence_is_never_claimed_without_every_condition(
    #[case] target: WatchTarget,
    #[case] rows: Vec<TautulliRow>,
    #[case] health: EvidenceHealth,
) {
    let targets = [target];
    assert!(absence_by_target(&targets, &resolved(&targets), &rows, &health, NOW).is_empty());
}

#[test]
fn a_title_resolved_item_gets_no_absence() {
    // Resolved by exact title+year only: correlation, not identity.
    let rows = [serde_json::from_str::<PlexMetadata>(r#"{"ratingKey":"5","title":"Film","year":2020}"#).expect("plex row")];
    let targets = [WatchTarget { external: ExternalIds::default(), ..target("radarr-5", 5, 200) }];
    let resolution = resolve(&targets, &PlexLibrary::new(&rows, &[], &[]));
    assert!(resolution.get("radarr-5").is_some() && !resolution.is_guid_resolved("radarr-5"));
    assert!(absence_by_target(&targets, &resolution, &background(), &healthy(), NOW).is_empty());
}

#[test]
fn a_long_silence_moves_coverage_past_the_gap() {
    let mut rows = vec![movie_stream("1", NOW - 500 * DAY, 100), movie_stream("1", NOW - 490 * DAY, 100)];
    // Nothing for 200 days, then steady streams again.
    rows.extend((0..=10).map(|step| movie_stream("2", NOW - 290 * DAY + step * 20 * DAY, 100)));
    let coverage = coverage(&rows).expect("rows exist");
    assert_eq!(coverage.start, NOW - 290 * DAY, "silence before the gap proves nothing");
    assert!(coverage.recording_at(coverage.end + MAX_SILENCE_SECS));
    assert!(!coverage.recording_at(coverage.end + MAX_SILENCE_SECS + 1));
    assert!(super::coverage(&[]).is_none());
}

#[test]
fn a_stream_is_a_watch_only_past_most_of_the_runtime() {
    assert!(movie_stream("1", 1, 100).is_watch());
    assert!(movie_stream("1", 1, 85).is_watch());
    assert!(!movie_stream("1", 1, 12).is_watch(), "stopped at 12% is not a watch");
}

/// `get_users` and `get_libraries_table` answers in Tautulli's documented shape.
fn keep_history(users: &[(&str, u8, u8)], sections: &[(u32, u8)]) -> KeepHistory {
    let users: Vec<String> = users
        .iter()
        .map(|(name, active, keep)| format!(r#"{{"friendly_name":"{name}","is_active":{active},"keep_history":{keep}}}"#))
        .collect();
    let sections: Vec<String> =
        sections.iter().map(|(id, keep)| format!(r#"{{"section_id":"{id}","is_active":1,"keep_history":{keep}}}"#)).collect();
    KeepHistory::parse(
        &format!(r#"{{"response":{{"result":"success","data":[{}]}}}}"#, users.join(",")),
        &format!(
            r#"{{"response":{{"result":"success","data":{{"recordsFiltered":{},"data":[{}]}}}}}}"#,
            sections.len(),
            sections.join(",")
        ),
    )
    .expect("documented shapes parse")
}

#[rstest]
#[case::everyone_and_every_target_section_kept(&[("Admin", 1, 1)], &[(1, 1), (2, 1)], &[1, 2], true)]
#[case::an_active_user_not_kept(&[("Admin", 1, 1), ("Arya", 1, 0)], &[(1, 1)], &[1], false)]
#[case::a_removed_user_does_not_count(&[("Admin", 1, 1), ("Gone", 0, 0)], &[(1, 1)], &[1], true)]
#[case::a_target_section_not_kept(&[("Admin", 1, 1)], &[(1, 1), (2, 0)], &[1, 2], false)]
#[case::a_section_tautulli_does_not_know(&[("Admin", 1, 1)], &[(1, 1)], &[1, 9], false)]
#[case::an_untargeted_section_not_kept(&[("Admin", 1, 1)], &[(1, 1), (3, 0)], &[1], true)]
fn silence_counts_only_where_every_stream_is_kept(
    #[case] users: &[(&str, u8, u8)],
    #[case] sections: &[(u32, u8)],
    #[case] targets: &[u32],
    #[case] covered: bool,
) {
    assert_eq!(keep_history(users, sections).covers(targets.iter().copied()), covered);
}

#[rstest]
#[case::users_not_an_answer("<html>", r#"{"response":{"data":{"data":[]}}}"#)]
#[case::libraries_not_an_answer(r#"{"response":{"data":[]}}"#, r#"{"response":{"result":"error","data":null}}"#)]
#[case::library_table_cut_short(
    r#"{"response":{"data":[]}}"#,
    r#"{"response":{"data":{"recordsFiltered":30,"data":[{"section_id":1,"keep_history":1}]}}}"#
)]
fn unreadable_switches_are_unknown_not_on(#[case] users: &str, #[case] libraries: &str) {
    assert_eq!(KeepHistory::parse(users, libraries), None);
}
