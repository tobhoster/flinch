//! Which removals are listed as FLINCH's doing, and which as everyone else's.

use super::*;
use crate::arr::{SeasonStats, SeriesSeason};
use rstest::rstest;
use serde_json::{json, Value};

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

/// Air dates either side of `NOW` (day 100, 1970-04-11).
const AIRED: Option<&str> = Some("1970-01-05T02:00:00Z");
const UPCOMING: Option<&str> = Some("1970-06-01T02:00:00Z");

/// Sonarr `/api/v3/episode` rows: `(season, monitored, has a file, air date)`.
fn episodes(rows: &[(u32, bool, bool, Option<&str>)]) -> Vec<Value> {
    rows.iter()
        .map(|(season, monitored, has_file, airs)| {
            json!({"seasonNumber": season, "episodeNumber": 1, "monitored": monitored, "hasFile": has_file, "airDateUtc": airs})
        })
        .collect()
}

#[rstest]
// What Maintainerr leaves behind: every episode unmonitored, the season flag on.
#[case::the_season_flag_on_but_no_episode_monitored(Some(episodes(&[(1, false, false, AIRED), (1, false, false, AIRED)])), Some(false))]
#[case::an_aired_monitored_episode_without_a_file(Some(episodes(&[(1, false, false, AIRED), (1, true, false, AIRED)])), Some(true))]
// Not yet aired, or no air date at all: a new download, not one coming back.
#[case::only_episodes_yet_to_air_are_monitored(
    Some(episodes(&[(1, false, false, AIRED), (1, true, false, UPCOMING), (1, true, false, None)])),
    Some(false)
)]
#[case::the_monitored_episode_has_its_file_back(Some(episodes(&[(1, true, true, AIRED), (1, false, false, AIRED)])), Some(false))]
#[case::only_another_season_wants_an_episode(Some(episodes(&[(1, false, false, AIRED), (2, true, false, AIRED)])), Some(false))]
#[case::the_episode_read_failed(None, Some(true))]
#[case::a_row_that_does_not_say_whether_it_is_monitored(
    Some(vec![json!({"seasonNumber": 1, "hasFile": false, "airDateUtc": AIRED})]),
    Some(true)
)]
fn a_deleted_season_comes_back_only_when_an_aired_episode_is_still_wanted(
    #[case] rows: Option<Vec<Value>>,
    #[case] monitored: Option<bool>,
) {
    let series = [show(31, "The Day of the Jackal", Some(true), 1, Some(true), 0)];
    let removals = [removal("sonarr-31-s1", NOW - DAY, RemovalReason::Manual)];
    let mut listed = outside_deletions(&removals, &BTreeMap::new(), &[], &series, NOW);
    let to_read: Vec<u32> = shows_to_settle(&listed, &series).iter().map(|show| show.id).collect();
    assert_eq!(to_read, [31], "the monitored show is read once");
    let read: HashMap<String, SonarrEpisodes> =
        rows.map(|rows| (series[0].subject(), SonarrEpisodes::from_rows(&rows))).into_iter().collect();

    settle_seasons(&mut listed, &series, &read, NOW);

    assert_eq!(listed[0].monitored, monitored);
}

#[test]
fn an_unmonitored_show_is_not_read_and_stays_unmonitored() {
    let series = [show(9, "For All Mankind", Some(false), 5, Some(true), 0)];
    let removals = [removal("sonarr-9-s5", NOW - DAY, RemovalReason::Manual)];
    let mut listed = outside_deletions(&removals, &BTreeMap::new(), &[], &series, NOW);
    assert!(shows_to_settle(&listed, &series).is_empty());
    let read = HashMap::from([(series[0].subject(), SonarrEpisodes::from_rows(&episodes(&[(5, true, false, AIRED)])))]);

    settle_seasons(&mut listed, &series, &read, NOW);

    assert_eq!(listed[0].monitored, Some(false), "Sonarr fetches nothing of an unmonitored show");
}
