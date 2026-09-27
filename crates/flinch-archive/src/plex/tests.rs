//! Plex parsing, identity and correlation tests.
//!
//! Every fixture is a payload shape the live server returns; the identity tests
//! pin the wrong-hit and wrong-miss cases a title join produced.

use super::history::{history_entries, merge_history};
use super::*;
use crate::card::{ArchiveCard, LibraryKind, SeasonState};
use crate::ids::{ExternalIds, PlexIds};
use crate::watch::{EvidenceHealth, WatchEntry, WatchSource};
use proptest::prelude::*;
use std::collections::{BTreeSet, HashMap, HashSet};

fn meta(json: &str) -> PlexMetadata {
    serde_json::from_str(json).expect("fixture parses")
}

fn tmdb(id: u32) -> ExternalIds {
    ExternalIds { tmdb: Some(id), ..ExternalIds::default() }
}

fn tvdb(id: u32) -> ExternalIds {
    ExternalIds { tvdb: Some(id), ..ExternalIds::default() }
}

fn movie_target(id: &str, title: &str, year: Option<u32>, external: ExternalIds) -> WatchTarget {
    WatchTarget {
        id: id.to_string(),
        kind: LibraryKind::Movie,
        title: title.to_string(),
        year,
        show_title: None,
        season_index: None,
        episodes_total: None,
        episode_files: None,
        external,
        added_epoch: Some(1_700_000_000),
        on_disk: true,
    }
}

fn season_target(id: &str, show: &str, season: u32, files: u32, external: ExternalIds) -> WatchTarget {
    WatchTarget {
        id: id.to_string(),
        kind: LibraryKind::Season,
        title: show.to_string(),
        year: Some(2022),
        show_title: Some(show.to_string()),
        season_index: Some(season),
        episodes_total: Some(files),
        episode_files: Some(files),
        external,
        added_epoch: Some(1_700_000_000),
        on_disk: true,
    }
}

fn movies(rows: &[&str]) -> PlexLibrary {
    PlexLibrary::new(&rows.iter().map(|json| meta(json)).collect::<Vec<_>>(), &[], &[])
}

const SUPERMAN_1978: &str = r#"{"ratingKey":"78","title":"Superman","year":1978,"viewCount":2,"lastViewedAt":1600000000,"Guid":[{"id":"tmdb://1924"},{"id":"imdb://tt0078346"}]}"#;

#[test]
fn a_remake_never_inherits_the_original_s_plays_without_a_shared_id() {
    // Only the 1978 film is in Plex, watched. Radarr's 2025 film has another id.
    let library = movies(&[SUPERMAN_1978]);
    let remake = movie_target("radarr-2025", "Superman", Some(2025), tmdb(1_061_474));
    let resolution = resolve(std::slice::from_ref(&remake), &library);
    assert!(resolution.get("radarr-2025").is_none(), "different ids: not the same film, whatever the title");

    // Even with no ids at all on the *arr side, the year keeps them apart.
    let bare = movie_target("radarr-2025", "Superman", Some(2025), ExternalIds::default());
    assert!(resolve(std::slice::from_ref(&bare), &library).get("radarr-2025").is_none());

    // And the original's history play does not reach the remake either.
    let history = [meta(r#"{"type":"movie","ratingKey":"78","title":"Superman","viewedAt":1600000000}"#)];
    assert!(history_entries(std::slice::from_ref(&remake), &resolution, &history).is_empty());
}

#[test]
fn a_localized_title_matches_through_its_guid() {
    let library = movies(&[
        r#"{"ratingKey":"5","title":"Die Verurteilten","year":1994,"viewCount":1,"lastViewedAt":1650000000,"Guid":[{"id":"imdb://tt0111161"}]}"#,
    ]);
    let target = movie_target(
        "radarr-1",
        "The Shawshank Redemption",
        Some(1994),
        ExternalIds { imdb: Some("tt0111161".into()), ..ExternalIds::default() },
    );
    let resolution = resolve(std::slice::from_ref(&target), &library);
    let found = resolution.get("radarr-1").expect("resolved by imdb id");
    assert_eq!(found.by, MatchedBy::Guid);
    assert_eq!(found.primary.rating_key, "5");
    assert!(found.watch.is_watched());
}

#[test]
fn a_legacy_agent_guid_resolves_like_a_modern_one() {
    let library = movies(&[r#"{"ratingKey":"8","title":"Heat","year":1995,"guid":"com.plexapp.agents.imdb://tt0113277?lang=en"}"#]);
    let target = movie_target("radarr-8", "Heat", Some(1995), ExternalIds { imdb: Some("tt0113277".into()), ..ExternalIds::default() });
    assert_eq!(resolve(&[target], &library).get("radarr-8").map(|found| found.by), Some(MatchedBy::Guid));
}

#[test]
fn a_title_fallback_needs_both_years_a_unique_item_and_a_unique_target() {
    let library = movies(&[r#"{"ratingKey":"1","title":"Animals","year":2026}"#]);
    let exact = movie_target("radarr-1", "Animals", Some(2026), ExternalIds::default());
    let resolution = resolve(std::slice::from_ref(&exact), &library);
    assert_eq!(resolution.get("radarr-1").map(|found| found.by), Some(MatchedBy::TitleYear));
    assert!(resolution.plex_ids().is_empty(), "a title match is never enough to act on");

    let no_year = movie_target("radarr-1", "Animals", None, ExternalIds::default());
    assert!(resolve(&[no_year], &library).is_empty(), "no year, no fallback");

    let twin_items = movies(&[r#"{"ratingKey":"1","title":"Animals","year":2026}"#, r#"{"ratingKey":"2","title":"Animals","year":2026}"#]);
    assert!(resolve(std::slice::from_ref(&exact), &twin_items).is_empty(), "two Plex candidates: no guess");

    let twin_targets = [exact.clone(), movie_target("radarr-2", "Animals", Some(2026), ExternalIds::default())];
    assert!(resolve(&twin_targets, &library).is_empty(), "two library items claim it: no guess");
}

#[test]
fn copies_in_two_sections_merge_into_one_item_that_keeps_both_keys() {
    let rows: Vec<PlexMetadata> = [
        r#"{"ratingKey":"10","librarySectionID":1,"title":"Dune","year":2021,"viewCount":0,"Guid":[{"id":"tmdb://438631"}]}"#,
        r#"{"ratingKey":"40","librarySectionID":4,"title":"Dune","year":2021,"viewCount":1,"lastViewedAt":1700000000,"Guid":[{"id":"tmdb://438631"},{"id":"imdb://tt1160419"}]}"#,
    ]
    .iter()
    .map(|json| meta(json))
    .collect();
    let library = PlexLibrary::new(&rows, &[], &[]);
    assert_eq!(library.movies.len(), 1, "one film, not an ambiguity");
    let dune = &library.movies[0];
    assert_eq!(dune.placements.len(), 2);
    assert_eq!(dune.watch.last_viewed_unix, Some(1_700_000_000), "the 4K copy's play counts for the film");
    assert!(dune.watch.is_watched());

    let target = movie_target("radarr-3", "Dune", Some(2021), ExternalIds { imdb: Some("tt1160419".into()), ..ExternalIds::default() });
    let resolution = resolve(&[target], &library);
    let ids = resolution.plex_ids();
    assert_eq!(
        ids["radarr-3"],
        PlexIds { rating_key: "10".into(), season_rating_key: None, section_id: Some(1) },
        "lowest section is primary"
    );
    let history = [meta(r#"{"type":"movie","ratingKey":"40","viewedAt":1710000000}"#)];
    let played = history_entries(&[movie_target("radarr-3", "Dune", Some(2021), tmdb(438_631))], &resolution, &history);
    assert_eq!(played["radarr-3"].last_watched_epoch, Some(1_710_000_000), "plays of either copy join");
}

fn show_library(leaf_count: u32) -> PlexLibrary {
    let shows =
        [meta(r#"{"ratingKey":"70","librarySectionID":2,"title":"Andor","year":2022,"leafCount":24,"Guid":[{"id":"tvdb://393189"}]}"#)];
    let seasons = [
        meta(&format!(
            r#"{{"type":"season","ratingKey":"71","parentRatingKey":"70","librarySectionID":2,"index":1,"leafCount":{leaf_count},"viewedLeafCount":6,"lastViewedAt":1690000000}}"#
        )),
        meta(
            r#"{"type":"season","ratingKey":"72","parentRatingKey":"70","librarySectionID":2,"index":2,"leafCount":12,"viewedLeafCount":0}"#,
        ),
    ];
    PlexLibrary::new(&[], &shows, &seasons)
}

#[test]
fn a_season_resolves_only_when_plex_holds_as_many_episodes_as_sonarr_has_files() {
    let target = season_target("sonarr-7-s1", "Andor", 1, 12, tvdb(393_189));
    let agreed = resolve(std::slice::from_ref(&target), &show_library(12));
    let found = agreed.get("sonarr-7-s1").expect("12 files, 12 leaves");
    assert_eq!(found.primary, PlexIds { rating_key: "70".into(), season_rating_key: Some("71".into()), section_id: Some(2) });
    assert!((found.watch.watched_fraction - 0.5).abs() < 1e-6);

    // Plex numbers this season differently (13 leaves under index 1): the
    // evidence could belong to another season, so the season is unresolved.
    let disagreed = resolve(std::slice::from_ref(&target), &show_library(13));
    assert!(disagreed.get("sonarr-7-s1").is_none());
    assert!(disagreed.plex_ids().is_empty());

    let unknown_files = WatchTarget { episode_files: None, ..target };
    assert!(resolve(&[unknown_files], &show_library(12)).is_empty(), "no file count, nothing to check against");
}

#[test]
fn a_season_plex_has_not_finished_scanning_resolves_by_episode_ids_and_reads_the_rest_unwatched() {
    // Sonarr has 13 files; Plex has scanned 12 of them, 6 watched.
    let target = season_target("sonarr-7-s1", "Andor", 1, 13, tvdb(393_189));
    let library = show_library(12);
    let mut resolution = resolve(std::slice::from_ref(&target), &library);
    assert!(resolution.get("sonarr-7-s1").is_none());
    assert_eq!(resolution.unconfirmed_seasons(), [Unconfirmed { target_id: "sonarr-7-s1".into(), show_rating_keys: vec!["70".into()] }]);

    let episode = |season: u32, id: u32| meta(&format!(r#"{{"type":"episode","parentIndex":{season},"Guid":[{{"id":"tvdb://{id}"}}]}}"#));
    let files: Vec<serde_json::Value> = (1..=13).map(|id| serde_json::json!({"seasonNumber": 1, "tvdbId": id, "hasFile": true})).collect();
    let ids = |plex_rows: Vec<PlexMetadata>| EpisodeIds {
        plex: HashMap::from([("70".to_string(), PlexEpisodes::from_rows(&plex_rows))]),
        sonarr: HashMap::from([("sonarr-7-s1".to_string(), SonarrEpisodes::from_rows(&files))]),
    };

    // A different grouping put two of Sonarr's season-1 files into Plex's season 2.
    let regrouped = ids((1..=10).map(|id| episode(1, id)).chain((11..=12).map(|id| episode(2, id))).collect());
    assert_eq!(resolution.confirm_seasons(std::slice::from_ref(&target), &library, &regrouped), 0);
    assert_eq!(resolution.unconfirmed_seasons().len(), 1, "still waiting, never guessed");

    let lagging = ids((1..=12).map(|id| episode(1, id)).collect());
    assert_eq!(resolution.confirm_seasons(std::slice::from_ref(&target), &library, &lagging), 1);
    let found = resolution.get("sonarr-7-s1").expect("confirmed by episode ids");
    assert_eq!(found.primary.season_rating_key.as_deref(), Some("71"));
    assert!((found.watch.watched_fraction - 6.0 / 13.0).abs() < 1e-6, "the unscanned file is unwatched, not skipped");
    assert!(resolution.unconfirmed_seasons().is_empty());
}

#[test]
fn a_season_plex_sent_without_episode_counts_is_never_read_as_unwatched() {
    // Live shape: the section's season listing drops `leafCount` and
    // `viewedLeafCount`. Read as zero, the counts "differed", episode ids then
    // confirmed the season, and a fully watched season read as never played.
    let shows = [meta(
        r#"{"ratingKey":"70","librarySectionID":2,"title":"Andor","year":2022,"leafCount":12,"viewedLeafCount":12,"Guid":[{"id":"tvdb://393189"}]}"#,
    )];
    let seasons = [meta(
        r#"{"type":"season","ratingKey":"71","parentRatingKey":"70","librarySectionID":2,"index":1,"viewCount":12,"lastViewedAt":1690000000}"#,
    )];
    let library = PlexLibrary::new(&[], &shows, &seasons);
    let target = season_target("sonarr-7-s1", "Andor", 1, 12, tvdb(393_189));
    let mut resolution = resolve(std::slice::from_ref(&target), &library);
    let episodes: Vec<PlexMetadata> =
        (1..=12).map(|id| meta(&format!(r#"{{"type":"episode","parentIndex":1,"Guid":[{{"id":"tvdb://{id}"}}]}}"#))).collect();
    let files: Vec<serde_json::Value> = (1..=12).map(|id| serde_json::json!({"seasonNumber": 1, "tvdbId": id, "hasFile": true})).collect();
    let ids = EpisodeIds {
        plex: HashMap::from([("70".to_string(), PlexEpisodes::from_rows(&episodes))]),
        sonarr: HashMap::from([("sonarr-7-s1".to_string(), SonarrEpisodes::from_rows(&files))]),
    };
    resolution.confirm_seasons(std::slice::from_ref(&target), &library, &ids);
    assert!(
        resolution.get("sonarr-7-s1").is_none_or(|found| found.watch.watched_fraction > 0.0),
        "a count Plex did not send is not a zero"
    );
}

#[test]
fn episode_plays_join_by_season_key_or_show_key_and_number() {
    let target = season_target("sonarr-7-s1", "Andor", 1, 12, tvdb(393_189));
    let resolution = resolve(std::slice::from_ref(&target), &show_library(12));
    let history = [
        meta(
            r#"{"type":"episode","ratingKey":"711","parentRatingKey":"71","grandparentRatingKey":"70","parentIndex":1,"index":1,"viewedAt":100}"#,
        ),
        meta(r#"{"type":"episode","ratingKey":"712","grandparentRatingKey":"70","parentIndex":1,"index":2,"viewedAt":300}"#),
        meta(
            r#"{"type":"episode","ratingKey":"721","parentRatingKey":"72","grandparentRatingKey":"70","parentIndex":2,"index":1,"viewedAt":900}"#,
        ),
        // Same title, another show.
        meta(
            r#"{"type":"episode","ratingKey":"911","grandparentRatingKey":"90","grandparentTitle":"Andor","parentIndex":1,"index":3,"viewedAt":950}"#,
        ),
    ];
    let entries = history_entries(&[target], &resolution, &history);
    let s1 = &entries["sonarr-7-s1"];
    assert_eq!(s1.last_watched_epoch, Some(300), "season 2 and the other show do not count");
    assert!((s1.progress - 2.0 / 12.0).abs() < 1e-6);
    assert_eq!(s1.source, WatchSource::PlexHistory);
}

#[test]
fn admin_only_zeros_are_dropped_on_a_shared_server_but_plays_are_kept() {
    let library = movies(&[
        r#"{"ratingKey":"1","title":"A","year":2020,"Guid":[{"id":"tmdb://1"}]}"#,
        r#"{"ratingKey":"2","title":"B","year":2020,"lastViewedAt":1700000000,"Guid":[{"id":"tmdb://2"}]}"#,
    ]);
    let targets = [movie_target("radarr-1", "A", Some(2020), tmdb(1)), movie_target("radarr-2", "B", Some(2020), tmdb(2))];
    let resolution = resolve(&targets, &library);
    let alone = EvidenceHealth { plex_configured: true, plex_items_ok: true, plex_history_complete: true, ..EvidenceHealth::default() };
    assert_eq!(resolution.item_entries(&alone).len(), 2);
    let shared = EvidenceHealth { multi_account: true, ..alone };
    let entries = resolution.item_entries(&shared);
    assert!(!entries.contains_key("radarr-1"), "the admin not having played it says nothing about the household");
    assert_eq!(entries["radarr-2"].source, WatchSource::Plex);
}

#[test]
fn on_a_shared_server_history_alone_brings_in_what_item_state_left_out() {
    // The admin never played it; another account did. Item state drops the
    // admin's silence on a shared server, so the play has to arrive through
    // history — what the daemon logs as "from playback history".
    let library = movies(&[r#"{"ratingKey":"1","title":"A","year":2020,"Guid":[{"id":"tmdb://1"}]}"#]);
    let targets = [movie_target("radarr-1", "A", Some(2020), tmdb(1))];
    let resolution = resolve(&targets, &library);
    let shared = EvidenceHealth {
        plex_configured: true,
        plex_items_ok: true,
        plex_history_complete: true,
        multi_account: true,
        ..EvidenceHealth::default()
    };
    let mut entries = resolution.item_entries(&shared);
    assert!(entries.is_empty());
    let history = [meta(r#"{"type":"movie","ratingKey":"1","viewedAt":1710000000,"accountID":2}"#)];
    merge_history(&mut entries, history_entries(&targets, &resolution, &history));
    assert_eq!(entries["radarr-1"].source, WatchSource::PlexHistory);
    assert_eq!(entries["radarr-1"].last_watched_epoch, Some(1_710_000_000));
}

#[test]
fn a_keep_marker_on_any_copy_the_show_or_the_season_keeps_the_item() {
    let keys = |key: &str| HashSet::from([key.to_string()]);
    let season = season_target("sonarr-7-s1", "Andor", 1, 12, tvdb(393_189));
    let seasons = resolve(std::slice::from_ref(&season), &show_library(12));
    assert_eq!(seasons.marked(&keys("70")), BTreeSet::from(["sonarr-7-s1".to_string()]), "a label on the show keeps its seasons");
    assert_eq!(seasons.marked(&keys("71")).len(), 1, "a collection holding the season keeps it");
    assert!(seasons.marked(&keys("72")).is_empty(), "another season's marker does not");

    let copies = movies(&[
        r#"{"ratingKey":"10","librarySectionID":1,"title":"Dune","year":2021,"Guid":[{"id":"tmdb://438631"}]}"#,
        r#"{"ratingKey":"11","librarySectionID":2,"title":"Dune","year":2021,"Guid":[{"id":"tmdb://438631"}]}"#,
    ]);
    let film = resolve(&[movie_target("radarr-3", "Dune", Some(2021), tmdb(438_631))], &copies);
    assert_eq!(film.marked(&keys("11")).len(), 1, "a label on the second copy keeps the film");
}

#[test]
fn newer_history_overrides_silent_item_state_but_not_newer_evidence() {
    let entry =
        |epoch: Option<u64>, source| WatchEntry { id: "x".into(), last_watched_epoch: epoch, progress: 1.0, rewatch_score: None, source };
    let mut entries = HashMap::from([("x".to_string(), entry(None, WatchSource::Plex))]);
    merge_history(&mut entries, HashMap::from([("x".to_string(), entry(Some(500), WatchSource::PlexHistory))]));
    assert_eq!(entries["x"].source, WatchSource::PlexHistory);
    let mut newer = HashMap::from([("x".to_string(), entry(Some(900), WatchSource::Plex))]);
    merge_history(&mut newer, HashMap::from([("x".to_string(), entry(Some(500), WatchSource::PlexHistory))]));
    assert_eq!(newer["x"].source, WatchSource::Plex);
}

#[test]
fn the_media_container_envelope_parses_paging_accounts_and_guids() {
    let body = r#"{"MediaContainer":{"size":1,"totalSize":1203,"Metadata":[{"ratingKey":"9","type":"movie","title":"Heat","Guid":[{"id":"tmdb://949"}]}],"Account":[{"id":0},{"id":1},{"id":7}]}}"#;
    let container = serde_json::from_str::<PlexEnvelope>(body).expect("envelope").container;
    assert_eq!(container.total_size, Some(1203));
    assert_eq!(container.account.len(), 3);
    assert_eq!(container.metadata[0].external_ids().tmdb, Some(949));
}

#[test]
fn a_movie_is_finished_only_when_plex_counted_a_view() {
    let counted = meta(r#"{"ratingKey":"1","title":"X","viewCount":1,"lastViewedAt":100}"#).movie_watch();
    assert!(counted.is_watched());
    assert!(meta(r#"{"ratingKey":"1","title":"X","viewCount":"1"}"#).movie_watch().is_watched(), "a count sent as a string");

    // A stamp without a counted view: started, not finished. A movie marked
    // unplayed can keep its stamp with a count of 0.
    for json in [r#"{"ratingKey":"1","title":"X","lastViewedAt":100}"#, r#"{"ratingKey":"1","title":"X","viewCount":0,"lastViewedAt":100}"#]
    {
        let started = meta(json).movie_watch();
        assert!(!started.is_watched(), "a started movie is not a watched one: {json}");
        assert!(started.watched_fraction > 0.0, "and not a never-played one either: {json}");
        assert_eq!(started.last_viewed_unix, Some(100), "the play still counts for recency: {json}");
    }

    let untouched = meta(r#"{"ratingKey":"1","title":"X"}"#).movie_watch();
    assert_eq!((untouched.watched_fraction, untouched.last_viewed_unix), (0.0, None));
}

/// Several accounts and no Tautulli: the admin's "nothing played" is no evidence.
fn shared_server() -> EvidenceHealth {
    EvidenceHealth {
        plex_configured: true,
        plex_items_ok: true,
        plex_history_complete: true,
        multi_account: true,
        ..EvidenceHealth::default()
    }
}

#[test]
fn undated_rows_read_as_before() {
    // Counted with no date: still watched, and the policy holds it as undated.
    let counted = meta(r#"{"ratingKey":"1","title":"X","viewCount":1,"viewOffset":600000}"#).movie_watch();
    assert!(counted.is_watched());
    assert_eq!(counted.last_viewed_unix, None);

    // A resume point alone is not read: the admin's row says nothing on a shared server.
    let library = movies(&[r#"{"ratingKey":"1","title":"A","year":2020,"viewOffset":600000,"Guid":[{"id":"tmdb://1"}]}"#]);
    let resolution = resolve(&[movie_target("radarr-1", "A", Some(2020), tmdb(1))], &library);
    assert_eq!(resolution.get("radarr-1").map(|found| found.watch.watched_fraction), Some(0.0));
    assert!(resolution.item_entries(&shared_server()).is_empty());
}

#[test]
fn a_started_movie_reaches_the_card_played_but_unwatched() {
    let library = movies(&[r#"{"ratingKey":"1","title":"A","year":2020,"lastViewedAt":1780000000,"Guid":[{"id":"tmdb://1"}]}"#]);
    let resolution = resolve(&[movie_target("radarr-1", "A", Some(2020), tmdb(1))], &library);
    let entries = resolution.item_entries(&shared_server());
    assert!(entries.contains_key("radarr-1"), "a dated play is evidence on any server");

    let mut cards = [ArchiveCard { id: "radarr-1".into(), ..crate::golden::golden_movie() }];
    crate::watch::apply(&mut cards, &entries);
    assert_eq!(cards[0].is_watched, Some(false), "started, not finished");
    assert!(cards[0].last_watched_days.is_some(), "the play still dates it");
}

#[test]
fn a_started_season_reads_partly_played_never_unplayed() {
    // The admin started an episode Plex did not count: the season row is dated
    // with no episode watched. Another account had finished four episodes
    // before that. The newer record decides, and read as "no play at all" it
    // let never-played reclaim take a season the household was watching.
    let shows =
        [meta(r#"{"ratingKey":"70","librarySectionID":2,"title":"Andor","year":2022,"leafCount":12,"Guid":[{"id":"tvdb://393189"}]}"#)];
    let seasons = [meta(
        r#"{"type":"season","ratingKey":"71","parentRatingKey":"70","librarySectionID":2,"index":1,"leafCount":12,"viewedLeafCount":0,"lastViewedAt":1787000000}"#,
    )];
    let targets = [season_target("sonarr-7-s1", "Andor", 1, 12, tvdb(393_189))];
    let resolution = resolve(&targets, &PlexLibrary::new(&[], &shows, &seasons));
    let history: Vec<PlexMetadata> = (1..=4)
        .map(|episode| {
            meta(&format!(
                r#"{{"type":"episode","ratingKey":"71{episode}","parentRatingKey":"71","grandparentRatingKey":"70","parentIndex":1,"index":{episode},"viewedAt":1783000000,"accountID":2}}"#
            ))
        })
        .collect();
    let mut entries = resolution.item_entries(&shared_server());
    merge_history(&mut entries, history_entries(&targets, &resolution, &history));
    assert_eq!(entries["sonarr-7-s1"].source, WatchSource::Plex, "the admin's start is the newer record");

    let mut cards = [ArchiveCard { id: "sonarr-7-s1".into(), episodes_total: Some(12), ..crate::golden::golden_season() }];
    crate::watch::apply(&mut cards, &entries);
    assert_eq!(cards[0].season_state, Some(SeasonState::Partial), "started, not unplayed");

    // Partial is never reclaimed, whatever the score.
    let armed = crate::policy::ArchivePolicy {
        unwatched_reclaim: crate::policy::UnwatchedReclaim { enabled: true, ..Default::default() },
        ..Default::default()
    };
    let verdict = crate::policy::ScoreVerdict { p_safe: 0.99, hard_guard: false, sibling_played: false };
    assert_eq!(crate::policy::decide(&cards[0], &armed, Some(verdict)), crate::policy::Reason::KeepBecauseNotCompleted);
}

#[test]
fn a_start_plex_dated_after_a_finished_play_reads_as_started() {
    // The newest record decides, and it errs toward keeping: the movie waits
    // for never-played reclaim instead of leaving as watched.
    let library = movies(&[r#"{"ratingKey":"1","title":"A","year":2020,"lastViewedAt":1700007200,"Guid":[{"id":"tmdb://1"}]}"#]);
    let targets = [movie_target("radarr-1", "A", Some(2020), tmdb(1))];
    let resolution = resolve(&targets, &library);

    // Tautulli dates a stream by its start, so its 87% of a session is older
    // than the stamp Plex set for the same session without counting a view.
    let stream: crate::tautulli::TautulliRow =
        serde_json::from_str(r#"{"media_type":"movie","rating_key":"1","title":"A","date":"1700000000","percent_complete":"87"}"#)
            .expect("stream fixture");
    let mut entries = resolution.item_entries(&shared_server());
    merge_history(&mut entries, crate::tautulli::plays_by_target(&targets, &resolution, &[stream]));
    assert_eq!(entries["radarr-1"].source, WatchSource::Plex);
    assert!(entries["radarr-1"].progress < 0.999, "a later start outweighs Tautulli's finished stream");

    // Another account's counted view, then the admin started it again.
    let history = [meta(r#"{"type":"movie","ratingKey":"1","viewedAt":1690000000,"accountID":2}"#)];
    let mut entries = resolution.item_entries(&shared_server());
    merge_history(&mut entries, history_entries(&targets, &resolution, &history));
    assert_eq!(entries["radarr-1"].source, WatchSource::Plex);
    assert!(entries["radarr-1"].progress < 0.999, "a later start outweighs an earlier counted view");
}

#[test]
fn the_fitter_trains_as_finished_a_movie_the_daemon_scores_as_started() {
    // The known disagreement fit/panel.rs records: the daemon lets Plex's
    // newer, uncounted stamp decide; the panel replays the play log, which
    // holds only the finished play. Pinned, so that aligning the two sides
    // flips these assertions on purpose rather than by drift.
    let library = movies(&[r#"{"ratingKey":"1","title":"A","year":2020,"lastViewedAt":1700007200,"Guid":[{"id":"tmdb://1"}]}"#]);
    let targets = [movie_target("radarr-1", "A", Some(2020), tmdb(1))];
    let resolution = resolve(&targets, &library);
    let stream: crate::tautulli::TautulliRow =
        serde_json::from_str(r#"{"media_type":"movie","rating_key":"1","title":"A","date":"1700000000","percent_complete":"87"}"#)
            .expect("stream fixture");
    let view = meta(r#"{"type":"movie","ratingKey":"1","viewedAt":1690000000,"accountID":2}"#);

    for (history, streams) in [(vec![], vec![stream]), (vec![view], vec![])] {
        // The daemon: item state, then both play logs, and the newest record decides.
        let mut entries = resolution.item_entries(&shared_server());
        merge_history(&mut entries, history_entries(&targets, &resolution, &history));
        merge_history(&mut entries, crate::tautulli::plays_by_target(&targets, &resolution, &streams));
        let source = entries["radarr-1"].source;
        let mut cards = [ArchiveCard { id: "radarr-1".into(), ..crate::golden::golden_movie() }];
        crate::watch::apply(&mut cards, &entries);
        let live = crate::score::HouseholdContext { watch_source: Some(source), ..Default::default() };
        let scored: Vec<&str> = crate::score::features(&cards[0], live).into_iter().map(|feature| feature.name).collect();
        assert_eq!(cards[0].is_watched, Some(false), "the daemon reads a start");
        assert!(scored.contains(&"partially_played") && !scored.contains(&"completed_cold"), "{scored:?}");

        // The fitter: the same plays, replayed at a cut after them.
        let log = crate::fit::plays::PlayLog::new(&history, &streams);
        let join = resolution.join(&targets[0]);
        let item = crate::fit::FitItem {
            id: "radarr-1".into(),
            title: "A".into(),
            kind: LibraryKind::Movie,
            size_bytes: 4_200_000_000,
            age_days: 2000.0,
            episodes_total: None,
            season_index: None,
            show_title: None,
            plays: log.item_plays(&join).into_iter().cloned().collect(),
            audience_plays: log.audience_plays(&join).into_iter().cloned().collect(),
            watch_source: Some(source),
            is_newest_season: false,
            series_status: None,
            last_aired_epoch: None,
            guid_resolved: true,
            genres: Vec::new(),
            on_disk: Vec::new(),
        };
        let spec =
            crate::fit::panel::PanelSpec { now: 1_800_000_000, cuts_days: &[100.0], horizon_days: 30.0, tautulli_coverage_start: None };
        let row = crate::fit::panel::build_dataset(&[item], &spec).remove(0);
        assert_eq!(row.card.is_watched, Some(true), "the fitter reads a finish");
        assert!(row.values.contains_key("completed_cold") && !row.values.contains_key("partially_played"), "{:?}", row.values);
    }
}

prop_compose! {
    /// A small library where titles, years and ids collide often. The
    /// ratingKey is set by position in the test: it names one Plex item.
    fn colliding_movie()(tmdb in proptest::option::of(1u32..4), title in 0usize..2, year in proptest::option::of(2000u32..2002))
        -> PlexMetadata {
        PlexMetadata {
            title: ["Superman", "Heat"][title].to_string(),
            year,
            guids: tmdb.map(|id| PlexGuid { id: format!("tmdb://{id}") }).into_iter().collect(),
            ..PlexMetadata::default()
        }
    }
}

proptest! {
    /// Whatever the library holds, a target resolves only to an item it shares
    /// an id with, or — with no id on either side to compare — to the one item
    /// carrying its exact title and a year both state. Never to a contradicting id.
    #[test]
    fn resolution_never_joins_contradicting_or_yearless_items(
        mut rows in proptest::collection::vec(colliding_movie(), 0..8),
        tmdb in proptest::option::of(1u32..4),
        title in 0usize..2,
        year in proptest::option::of(2000u32..2002),
    ) {
        for (index, row) in rows.iter_mut().enumerate() {
            row.rating_key = index.to_string();
        }
        let library = PlexLibrary::new(&rows, &[], &[]);
        let target = movie_target("radarr-1", ["Superman", "Heat"][title], year, ExternalIds { tmdb, ..ExternalIds::default() });
        let resolution = resolve(std::slice::from_ref(&target), &library);
        if let Some(found) = resolution.get("radarr-1") {
            let item = library.movies.iter().find(|item| item.holds(&found.primary.rating_key)).expect("resolved to a library item");
            match found.by {
                MatchedBy::Guid => prop_assert_eq!(guid::relate(&item.ids, &target.external), guid::Relation::Same),
                MatchedBy::TitleYear => {
                    prop_assert_eq!(guid::relate(&item.ids, &target.external), guid::Relation::Unknown);
                    prop_assert!(year.is_some() && item.year == year && item.title == target.title);
                }
            }
        }
    }
}

#[test]
fn numbers_sent_as_strings_parse_and_a_garbled_one_reads_as_absent() {
    // History rows may carry numbers as strings ("librarySectionID":"1").
    let row = meta(r#"{"ratingKey":"5","librarySectionID":"1","viewedAt":"1690000000","accountID":1,"leafCount":"n/a"}"#);
    assert_eq!(row.library_section_id, Some(1));
    assert_eq!(row.viewed_at, Some(1_690_000_000));
    assert_eq!(row.account_id, Some(1));
    assert_eq!(row.leaf_count, None, "one unreadable field must not fail the row, and with it the page");
}
