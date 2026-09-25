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
    let unknown = SonarrEpisodes::from_rows(&[serde_json::json!({"seasonNumber": 1, "tvdbId": 0, "hasFile": true})]);
    assert_eq!(PlexEpisodes::from_rows(&[tmdb_only]), PlexEpisodes::default());
    assert_eq!(unknown, SonarrEpisodes::default());
}
