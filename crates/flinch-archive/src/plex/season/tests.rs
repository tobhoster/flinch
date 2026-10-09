//! A season's completion from Plex's playback history, counted against the
//! episodes still on disk.

use super::*;
use crate::card::LibraryKind;
use crate::ids::ExternalIds;
use crate::plex::history::history_entries;
use crate::plex::{resolve, PlexLibrary, PlexMetadata};
use crate::watch::COMPLETE;
use rstest::rstest;

fn meta(json: &str) -> PlexMetadata {
    serde_json::from_str(json).expect("fixture parses")
}

/// Andor's season 1 with eight files, as Plex holds it (eight episodes).
fn season(on_disk: Option<Vec<u32>>) -> (WatchTarget, PlexLibrary) {
    let target = WatchTarget {
        id: "sonarr-7-s1".to_string(),
        kind: LibraryKind::Season,
        title: "Andor".to_string(),
        year: Some(2022),
        show_title: Some("Andor".to_string()),
        season_index: Some(1),
        episodes_total: Some(8),
        episode_files: Some(8),
        episodes_on_disk: on_disk,
        external: ExternalIds { tvdb: Some(393_189), ..ExternalIds::default() },
        added_epoch: Some(1_700_000_000),
        on_disk: true,
    };
    let shows = [meta(r#"{"ratingKey":"70","librarySectionID":2,"title":"Andor","year":2022,"Guid":[{"id":"tvdb://393189"}]}"#)];
    let seasons = [meta(r#"{"type":"season","ratingKey":"71","parentRatingKey":"70","librarySectionID":2,"index":1,"leafCount":8}"#)];
    (target, PlexLibrary::new(&[], &shows, &seasons))
}

/// History rows of season 1: one counted view per episode number (`None`: a
/// row Plex sent without one).
fn views(episodes: impl IntoIterator<Item = Option<u32>>) -> Vec<PlexMetadata> {
    episodes
        .into_iter()
        .map(|episode| {
            let index = episode.map_or_else(String::new, |number| format!(r#","index":{number}"#));
            meta(&format!(
                r#"{{"type":"episode","ratingKey":"71{}","parentRatingKey":"71","grandparentRatingKey":"70","parentIndex":1{index},"viewedAt":1780000000}}"#,
                episode.unwrap_or(0)
            ))
        })
        .collect()
}

fn viewed(episodes: std::ops::RangeInclusive<u32>) -> Vec<Option<u32>> {
    episodes.map(Some).collect()
}

/// Episodes 5-12 on disk: 1-4 were deleted after they were watched.
fn later_eight() -> Option<Vec<u32>> {
    Some((5..=12).collect())
}

// Eight files on disk; a case says which episode numbers they hold. Deleted by
// hand or by Plex's "Delete episodes after playing", the views of 1-4 must not
// stand in for 9-12.
#[rstest]
#[case::the_watched_episodes_deleted_since(later_eight(), viewed(1..=8), 0.5)]
#[case::every_episode_on_disk_viewed(Some((1..=8).collect()), viewed(1..=8), 1.0)]
// Views of deleted episodes neither help nor hurt.
#[case::every_episode_left_viewed(later_eight(), viewed(1..=12), 1.0)]
// A view without an episode number matches no file: eight views, seven count.
#[case::an_unnumbered_view(later_eight(), viewed(5..=11).into_iter().chain([None]).collect(), 0.875)]
#[case::only_deleted_episodes_viewed(later_eight(), viewed(1..=4), 0.01)]
// Which episodes are on disk is unknown: just short of complete, so the season
// is announced through Leaving Soon instead of leaving unannounced.
#[case::episodes_on_disk_unknown(None, viewed(1..=8), UNVERIFIED)]
#[case::episodes_on_disk_read_as_none(Some(Vec::new()), viewed(1..=8), UNVERIFIED)]
// Partly viewed reads as before: the cap only stops a complete reading.
#[case::partly_viewed_and_unknown(None, viewed(1..=4), 0.5)]
fn history_completes_a_season_only_when_every_episode_on_disk_was_viewed(
    #[case] on_disk: Option<Vec<u32>>,
    #[case] viewed: Vec<Option<u32>>,
    #[case] expected: f32,
) {
    let (target, library) = season(on_disk);
    let resolution = resolve(std::slice::from_ref(&target), &library);
    let entries = history_entries(std::slice::from_ref(&target), &resolution, &views(viewed));
    let progress = entries["sonarr-7-s1"].progress;
    assert!((progress - expected).abs() < 1e-6, "{progress}");
    assert_eq!(progress >= COMPLETE, expected == 1.0, "complete only when every episode on disk was viewed");
}

#[test]
fn a_season_with_nothing_on_disk_keeps_its_share_of_the_files() {
    // Nothing on disk, nothing to delete: the household did watch all of it.
    let (target, _) = season(None);
    let off_disk = WatchTarget { on_disk: false, ..target };
    let mut plays = EpisodePlays::default();
    (1..=8).for_each(|episode| plays.record(Some(episode), true));
    assert_eq!(plays.progress(&off_disk), 1.0);
}
