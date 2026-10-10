//! Season identity from TVDB episode ids.

use super::*;
use rstest::rstest;

/// Plex episode rows (`allLeaves`) for `(season, tvdb ids)` pairs.
fn plex(seasons: &[(u32, Vec<u32>)]) -> PlexEpisodes {
    let rows: Vec<PlexMetadata> = seasons
        .iter()
        .flat_map(|(season, ids)| ids.iter().map(move |id| (*season, *id)))
        .map(|(season, id)| {
            let row = serde_json::json!({"type": "episode", "parentIndex": season, "Guid": [{"id": format!("tvdb://{id}")}]});
            serde_json::from_value(row).expect("episode row")
        })
        .collect();
    PlexEpisodes::from_rows(&rows)
}

/// Sonarr episode rows for season 1: every numbered episode, `files` with a file.
fn sonarr(numbered: Vec<u32>, files: Vec<u32>) -> SonarrEpisodes {
    let rows: Vec<serde_json::Value> =
        numbered.iter().map(|id| serde_json::json!({"seasonNumber": 1, "tvdbId": id, "hasFile": files.contains(id)})).collect();
    SonarrEpisodes::from_rows(&rows)
}

#[rstest]
#[case::plex_has_not_scanned_the_newest_file(vec![(1, (1..=12).collect())], (1..=13).collect(), (1..=13).collect(), true)]
#[case::plex_holds_an_episode_sonarr_has_no_file_for(vec![(1, (1..=13).collect())], (1..=13).collect(), (1..=12).collect(), true)]
#[case::another_grouping_moved_files_to_the_next_season(
    vec![(1, (1..=8).collect()), (2, (9..=20).collect())],
    (1..=10).collect(),
    (1..=10).collect(),
    false
)]
#[case::plex_files_a_foreign_episode_under_it(vec![(1, (1..=10).chain([99]).collect())], (1..=10).collect(), (1..=10).collect(), false)]
#[case::no_file_in_common(vec![(1, (1..=3).collect())], (1..=6).collect(), (4..=6).collect(), false)]
#[case::plex_has_no_such_season(vec![(2, (1..=10).collect())], (1..=10).collect(), (1..=10).collect(), false)]
fn a_season_is_the_same_only_when_its_episodes_agree(
    #[case] plex_seasons: Vec<(u32, Vec<u32>)>,
    #[case] numbered: Vec<u32>,
    #[case] files: Vec<u32>,
    #[case] same: bool,
) {
    assert_eq!(same_season(1, &plex(&plex_seasons), &sonarr(numbered, files)), same);
}

#[test]
fn an_episode_tvdb_does_not_know_confirms_nothing() {
    // Sonarr reports 0 for an episode TVDB has no id for; a tmdb-only Plex
    // episode has no TVDB id either. Treating either as an id would merge
    // unrelated episodes into one.
    let tmdb_only: PlexMetadata =
        serde_json::from_str(r#"{"type":"episode","parentIndex":1,"Guid":[{"id":"tmdb://55"}]}"#).expect("episode row");
    let unknown = SonarrEpisodes::from_rows(&[serde_json::json!({"seasonNumber": 1, "episodeNumber": 1, "tvdbId": 0, "hasFile": true})]);
    assert_eq!(PlexEpisodes::from_rows(&[tmdb_only]), PlexEpisodes::default());
    assert!(unknown.numbered.is_empty() && unknown.with_files.is_empty(), "no TVDB id taken from a 0");
    assert_eq!(unknown.on_disk(1, 1), Some(vec![1]), "its file still counts as on disk");
}

/// Sonarr episode rows of season 1, `(episode number, has a file)`.
fn numbered(rows: &[(Option<u32>, bool)]) -> Vec<serde_json::Value> {
    rows.iter()
        .map(|(number, has_file)| serde_json::json!({"seasonNumber": 1, "episodeNumber": number, "tvdbId": 100, "hasFile": has_file}))
        .collect()
}

#[rstest]
#[case::the_episodes_with_a_file(numbered(&[(Some(8), true), (Some(5), true), (Some(2), false), (Some(6), true), (Some(7), true)]), 4, Some(vec![5, 6, 7, 8]))]
// A double episode is one file.
#[case::a_file_holding_two_episodes(numbered(&[(Some(1), true), (Some(2), true)]), 1, Some(vec![1, 2]))]
// Each case below could leave out an unwatched episode on disk, and the plays
// of the others would then complete the season.
#[case::fewer_numbers_than_files(numbered(&[(Some(5), true), (Some(6), true)]), 3, None)]
#[case::a_file_without_an_episode_number(numbered(&[(Some(5), true), (None, true), (Some(6), true)]), 2, None)]
#[case::an_unreadable_row([numbered(&[(Some(5), true)]), vec![serde_json::json!({"episodeNumber": 6, "hasFile": true})]].concat(), 1, None)]
fn episodes_on_disk_are_trusted_only_when_every_file_is_numbered(
    #[case] rows: Vec<serde_json::Value>,
    #[case] files: u32,
    #[case] on_disk: Option<Vec<u32>>,
) {
    assert_eq!(SonarrEpisodes::from_rows(&rows).on_disk(1, files), on_disk);
}
