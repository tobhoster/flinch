//! Plays from before a library re-add: they reach the item by `plex://` GUID,
//! and by nothing weaker.

use super::*;
use crate::card::LibraryKind;
use crate::ids::ExternalIds;
use crate::plex::{resolve, PlayKeys, WatchTarget};
use crate::tautulli::{plays_by_target, TautulliRow};
use rstest::rstest;

fn meta(json: &str) -> PlexMetadata {
    serde_json::from_str(json).expect("fixture parses")
}

fn stream(json: &str) -> TautulliRow {
    serde_json::from_str(json).expect("stream fixture")
}

fn target(id: &str, kind: LibraryKind, title: &str, year: u32, season: Option<u32>, external: ExternalIds) -> WatchTarget {
    WatchTarget {
        id: id.to_string(),
        kind,
        title: title.to_string(),
        year: Some(year),
        show_title: season.map(|_| title.to_string()),
        season_index: season,
        episodes_total: season.map(|_| 2),
        episode_files: season.map(|_| 2),
        external,
        added_epoch: Some(1_700_000_000),
        on_disk: true,
    }
}

const HEAT_GUID: &str = "plex://movie/5d776b59ad5437001f79c6f8";

#[rstest]
#[case::its_plex_guid(HEAT_GUID, false, true)]
#[case::a_legacy_agent_guid("com.plexapp.agents.imdb://tt0113277?lang=en", false, false)]
#[case::the_same_title_and_year_only("", false, false)]
#[case::another_film_s_plex_guid("plex://movie/5d7768ba96b655001fdc0408", false, false)]
#[case::a_plex_guid_two_library_items_carry(HEAT_GUID, true, false)]
fn a_play_under_a_replaced_rating_key_joins_its_movie_only_by_plex_guid(
    #[case] guid: &str,
    #[case] twin_in_library: bool,
    #[case] joins: bool,
) {
    let mut rows = vec![meta(&format!(r#"{{"ratingKey":"100","title":"Heat","year":1995,"guid":"{HEAT_GUID}","Guid":[{{"id":"tmdb://949"}}]}}"#))];
    if twin_in_library {
        rows.push(meta(&format!(r#"{{"ratingKey":"200","title":"Heat","year":1986,"guid":"{HEAT_GUID}","Guid":[{{"id":"tmdb://26306"}}]}}"#)));
    }
    let library = PlexLibrary::new(&rows, &[], &[]);
    let heat = target("radarr-1", LibraryKind::Movie, "Heat", 1995, None, ExternalIds { tmdb: Some(949), ..ExternalIds::default() });
    let resolution = resolve(std::slice::from_ref(&heat), &library);
    assert!(resolution.is_guid_resolved("radarr-1"));

    // Streamed under ratingKey 7, which the re-add replaced with 100.
    let played = stream(&format!(
        r#"{{"media_type":"movie","rating_key":"7","title":"Heat","year":"1995","guid":"{guid}","date":"1700000100","percent_complete":"100"}}"#
    ));
    let entries = plays_by_target(std::slice::from_ref(&heat), &resolution, std::slice::from_ref(&played));
    assert_eq!(entries.contains_key("radarr-1"), joins);

    let keys: Vec<RowKey> = played.key().into_iter().collect();
    let counted = guid_joins(&[resolution.join(&heat)], &keys);
    assert_eq!(counted, GuidJoins { movies: usize::from(joins), episodes: 0 }, "the log counts exactly the GUID joins");
}

fn episode(rating_key: &str, parent: &str, grandparent: &str, season: u32, number: u32, guid: &str, date: u64) -> TautulliRow {
    stream(&format!(
        r#"{{"media_type":"episode","rating_key":"{rating_key}","parent_rating_key":"{parent}","grandparent_rating_key":"{grandparent}","parent_media_index":"{season}","media_index":"{number}","guid":"{guid}","date":"{date}","percent_complete":"100"}}"#
    ))
}

#[test]
fn an_episode_play_under_replaced_rating_keys_joins_the_season_plex_files_it_under_now() {
    let shows = [meta(r#"{"ratingKey":"70","librarySectionID":2,"title":"Andor","year":2022,"Guid":[{"id":"tvdb://393189"}]}"#)];
    let seasons = [meta(r#"{"ratingKey":"71","parentRatingKey":"70","index":1,"leafCount":2,"librarySectionID":2}"#)];
    let library = PlexLibrary::new(&[], &shows, &seasons);
    let season_one = target("sonarr-7-s1", LibraryKind::Season, "Andor", 2022, Some(1), ExternalIds { tvdb: Some(393_189), ..ExternalIds::default() });
    let targets = std::slice::from_ref(&season_one);
    let mut resolution = resolve(targets, &library);

    let streams = [
        // Played after the re-add: joins by ratingKey as always.
        episode("702", "71", "70", 1, 2, "plex://episode/e2", 200),
        // Played before it, when the show was ratingKey 50.
        episode("501", "51", "50", 1, 1, "plex://episode/e1", 100),
        episode("511", "52", "50", 2, 1, "plex://episode/e3", 300),
        // A legacy agent's episode GUID names the show, not the episode.
        episode("502", "51", "50", 1, 2, "com.plexapp.agents.thetvdb://393189/1/2?lang=en", 400),
    ];
    let keys: Vec<RowKey> = streams.iter().filter_map(TautulliRow::key).collect();
    assert_eq!(unjoined_episodes(&library, &keys), 2, "only plays outside the library with a plex GUID ask for the index");
    let before = &plays_by_target(targets, &resolution, &streams)["sonarr-7-s1"];
    assert!((before.progress - 0.5).abs() < 1e-6, "without the index only the post-re-add play joins");

    let mut index = EpisodeGuids::new(1_700_000_000);
    index.add_show(
        "70",
        &[
            meta(r#"{"ratingKey":"701","type":"episode","parentIndex":1,"index":1,"guid":"plex://episode/e1"}"#),
            meta(r#"{"ratingKey":"702","type":"episode","parentIndex":1,"index":2,"guid":"plex://episode/e2"}"#),
            meta(r#"{"ratingKey":"711","type":"episode","parentIndex":2,"index":1,"guid":"plex://episode/e3"}"#),
        ],
    );
    let cached: EpisodeGuids = serde_json::from_slice(&serde_json::to_vec(&index).expect("index serialises")).expect("cache reads back");
    assert_eq!(cached, index, "episode-guids.json reads back as written");
    assert_eq!(resolution.attach_episode_guids(&cached), 1);

    let after = &plays_by_target(targets, &resolution, &streams)["sonarr-7-s1"];
    assert!((after.progress - 1.0).abs() < 1e-6, "the pre-re-add play of episode 1 completes the season");
    assert_eq!(after.last_watched_epoch, Some(200), "season 2's play and the legacy-GUID play are not season 1's");
    let joins = [resolution.join(&season_one)];
    assert_eq!(guid_joins(&joins, &keys), GuidJoins { movies: 0, episodes: 1 });
    match resolution.play_keys().remove("sonarr-7-s1") {
        Some(PlayKeys::Season { episode_guids, .. }) => {
            assert_eq!(episode_guids, ["plex://episode/e1", "plex://episode/e2"], "persisted for the fitter")
        }
        other => panic!("season keys expected, got {other:?}"),
    }
}
