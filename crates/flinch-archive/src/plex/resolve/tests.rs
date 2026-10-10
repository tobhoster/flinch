//! One title in two *arr instances: two cards, one Plex item.

use super::super::history::history_entries;
use super::super::{PlexLibrary, PlexMetadata, WatchTarget};
use super::*;
use crate::ids::ExternalIds;

fn target(id: &str, external: ExternalIds) -> WatchTarget {
    WatchTarget {
        id: id.to_string(),
        kind: LibraryKind::Movie,
        title: "Dune".to_string(),
        year: Some(2021),
        show_title: None,
        season_index: None,
        episodes_total: None,
        episode_files: None,
        episodes_on_disk: None,
        external,
        added_epoch: Some(1_700_000_000),
        on_disk: true,
    }
}

fn row(json: &str) -> PlexMetadata {
    serde_json::from_str(json).unwrap_or_else(|error| panic!("fixture: {error}"))
}

#[test]
fn an_hd_and_a_4k_copy_of_one_film_share_its_plays_and_are_no_ambiguity() {
    let library = PlexLibrary::new(
        &[
            row(r#"{"ratingKey":"10","librarySectionID":1,"title":"Dune","year":2021,"Guid":[{"id":"tmdb://438631"}]}"#),
            row(r#"{"ratingKey":"40","librarySectionID":4,"title":"Dune","year":2021,"Guid":[{"id":"tmdb://438631"}]}"#),
        ],
        &[],
        &[],
    );
    let dune = ExternalIds { tmdb: Some(438_631), ..ExternalIds::default() };
    let targets = [target("radarr-3", dune.clone()), target("radarr@4k-7", dune)];
    let resolution = resolve(&targets, &library);
    assert!(resolution.get("radarr-3").is_some() && resolution.get("radarr@4k-7").is_some(), "each copy's card resolves");
    assert!(resolution.ambiguous_movies.is_empty(), "one film in two instances is not two films");
    // A play of the 4K copy is a play of the film: both cards read it.
    let history = [row(r#"{"type":"movie","ratingKey":"40","viewedAt":1710000000}"#)];
    let played = history_entries(&targets, &resolution, &history);
    assert_eq!(played["radarr-3"].last_watched_epoch, Some(1_710_000_000));
    assert_eq!(played["radarr@4k-7"].last_watched_epoch, Some(1_710_000_000));
}

#[test]
fn two_films_of_one_title_and_year_still_never_guess_by_title() {
    let library = PlexLibrary::new(&[row(r#"{"ratingKey":"1","title":"Dune","year":2021}"#)], &[], &[]);
    let one = ExternalIds { tmdb: Some(1), ..ExternalIds::default() };
    let other = ExternalIds { tmdb: Some(2), ..ExternalIds::default() };
    let resolution = resolve(&[target("radarr-1", one), target("radarr@4k-2", other)], &library);
    assert!(resolution.is_empty(), "different catalogue ids: two films, no title fallback");
    assert_eq!(resolution.ambiguous_movies.len(), 1);
}
