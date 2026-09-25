//! Which removals are listed as FLINCH's doing, and which as everyone else's.

use super::*;
use crate::arr::{SeasonStats, SeriesSeason};

const DAY: u64 = 86_400;
const NOW: u64 = 100 * DAY;

fn removal(card: &str, at: u64, reason: RemovalReason) -> Removal {
    Removal { card: card.to_string(), at, reason }
}

fn movie(id: u32, title: &str, monitored: Option<bool>, has_file: bool) -> ArrMovie {
    ArrMovie { id, title: title.to_string(), monitored, has_file, ..Default::default() }
}

fn show(id: u32, title: &str, monitored: Option<bool>, season: u32, season_monitored: Option<bool>, files: u32) -> ArrSeries {
    let stats = SeasonStats { episode_file_count: files, ..Default::default() };
    ArrSeries {
        id,
        title: title.to_string(),
        monitored,
        seasons: vec![SeriesSeason { season_number: season, monitored: season_monitored, statistics: stats, ..Default::default() }],
        ..Default::default()
    }
}

fn ids(listed: &[OutsideDeletion]) -> Vec<&str> {
    listed.iter().map(|item| item.id.as_str()).collect()
}

#[test]
fn a_removal_flinch_handed_over_first_is_its_own_and_is_not_listed() {
    let movies = [movie(1, "Handed", Some(false), false), movie(2, "Deleted by hand", Some(true), false)];
    let removals = [removal("radarr-1", NOW - DAY, RemovalReason::Manual), removal("radarr-2", NOW - DAY, RemovalReason::Manual)];
    let handoffs = BTreeMap::from([("radarr-1".to_string(), NOW - 10 * DAY)]);

    let listed = outside_deletions(&removals, &handoffs, &movies, &[], NOW);

    assert_eq!(ids(&listed), ["radarr-2"]);
}

#[test]
fn a_hand_over_after_the_removal_does_not_explain_it() {
    let movies = [movie(1, "Superman", Some(true), false)];
    let removals = [removal("radarr-1", NOW - 5 * DAY, RemovalReason::Manual)];
    let handoffs = BTreeMap::from([("radarr-1".to_string(), NOW - DAY)]);

    assert_eq!(ids(&outside_deletions(&removals, &handoffs, &movies, &[], NOW)), ["radarr-1"]);
}

#[test]
fn a_seasons_episode_removals_are_one_item_counting_every_file() {
    let series = [show(5, "Lioness", Some(true), 3, Some(true), 0)];
    let removals = [
        removal("sonarr-5-s3", NOW - 3 * DAY, RemovalReason::MissingFromDisk),
        removal("sonarr-5-s3", NOW - DAY, RemovalReason::Manual),
        removal("sonarr-5-s3", NOW - 2 * DAY, RemovalReason::Manual),
    ];

    let listed = outside_deletions(&removals, &BTreeMap::new(), &[], &series, NOW);

    assert_eq!(listed.len(), 1);
    let season = &listed[0];
    assert_eq!((season.files, season.at_unix, season.reason), (3, NOW - DAY, RemovalReason::Manual));
    assert_eq!((season.title.as_str(), season.season_label.as_deref(), season.kind.as_str()), ("Lioness", Some("S3"), "season"));
}

#[test]
fn old_removals_and_items_the_arr_no_longer_has_are_left_out() {
    let movies = [movie(1, "Old", Some(true), false)];
    let removals =
        [removal("radarr-1", NOW - WINDOW_SECS - 1, RemovalReason::Manual), removal("radarr-99", NOW - DAY, RemovalReason::Manual)];

    assert!(outside_deletions(&removals, &BTreeMap::new(), &movies, &[], NOW).is_empty());
}

#[test]
fn whether_it_comes_back_is_read_from_the_library_now_newest_first() {
    let movies = [movie(1, "Monitored, no file", Some(true), false), movie(2, "Grabbed again", Some(true), true)];
    let series = [show(5, "Season unmonitored", Some(true), 1, Some(false), 0), show(6, "Unknown", Some(true), 2, None, 0)];
    let removals = [
        removal("radarr-1", NOW - 4 * DAY, RemovalReason::Manual),
        removal("radarr-2", NOW - 3 * DAY, RemovalReason::Manual),
        removal("sonarr-5-s1", NOW - 2 * DAY, RemovalReason::Manual),
        removal("sonarr-6-s2", NOW - DAY, RemovalReason::Manual),
    ];

    let listed = outside_deletions(&removals, &BTreeMap::new(), &movies, &series, NOW);

    let state: Vec<(&str, Option<bool>, bool)> = listed.iter().map(|item| (item.id.as_str(), item.monitored, item.on_disk)).collect();
    assert_eq!(
        state,
        [
            ("sonarr-6-s2", None, false),
            ("sonarr-5-s1", Some(false), false),
            ("radarr-2", Some(true), true),
            ("radarr-1", Some(true), false),
        ]
    );
}
