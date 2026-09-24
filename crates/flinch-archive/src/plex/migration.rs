//! Plays recorded before a library migration.
//!
//! Removing media from Plex and adding it again gives every item a new
//! ratingKey. Plays join by ratingKey, so everything the household watched
//! before the re-add stopped counting: one live library had 12 of 41 on-disk
//! items played "before they arrived". Plex's own `plex://` GUIDs name the
//! catalogue entry and survive the re-add, so they are the second join key:
//! - a movie play through its `plex://movie/…` GUID, to the one library movie
//!   carrying it ([`super::library`] drops a GUID two items share);
//! - an episode play through its `plex://episode/…` GUID, to the season Plex
//!   files that episode under now, read from each show's `allLeaves`.
//!
//! Agent GUIDs never join (a legacy episode GUID names its show, and a re-match
//! replaces it), and neither do titles.

use super::guid::plex_guid;
use super::join::{Media, PlayJoin, RowKey};
use super::library::PlexLibrary;
use super::PlexMetadata;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// How long a read of the episode GUIDs is reused. Reading them costs one
/// request per show, and a re-add is rare.
pub const REFRESH_SECS: u64 = 86_400;

/// Where Plex files each episode now: show ratingKey → season number →
/// `plex://episode/…` GUIDs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpisodeGuids {
    /// When the shows were read.
    pub read_unix: u64,
    shows: BTreeMap<String, BTreeMap<u32, BTreeSet<String>>>,
}

impl EpisodeGuids {
    pub fn new(read_unix: u64) -> Self {
        Self { read_unix, shows: BTreeMap::new() }
    }

    /// Index one show's `/library/metadata/{show}/allLeaves` rows. An episode
    /// without a season number or a `plex://episode/…` GUID is skipped.
    pub fn add_show(&mut self, show_rating_key: &str, rows: &[PlexMetadata]) {
        let seasons = self.shows.entry(show_rating_key.to_string()).or_default();
        for row in rows {
            let guid = row.guid.as_deref().and_then(|guid| plex_guid(guid, "episode"));
            if let (Some(season), Some(guid)) = (row.parent_index, guid) {
                seasons.entry(season).or_default().insert(guid.to_string());
            }
        }
    }

    /// The episode GUIDs Plex files under `season` of the show.
    pub fn season(&self, show_rating_key: &str, season: u32) -> Option<&BTreeSet<String>> {
        self.shows.get(show_rating_key)?.get(&season)
    }

    /// Shows read.
    pub fn shows(&self) -> usize {
        self.shows.len()
    }

    /// Read less than [`REFRESH_SECS`] before `now`.
    pub fn is_fresh(&self, now: u64) -> bool {
        now.saturating_sub(self.read_unix) < REFRESH_SECS
    }
}

/// Episode plays carrying a `plex://episode/…` GUID whose season and show
/// are both gone from the library: plays of shows Plex has re-added (or
/// deleted) since. Only these make the episode GUID index worth reading.
pub fn unjoined_episodes<'a>(library: &PlexLibrary, rows: impl IntoIterator<Item = &'a RowKey>) -> usize {
    let seasons: HashSet<&str> = library.seasons.iter().map(|season| season.placement.rating_key.as_str()).collect();
    let shows: HashSet<&str> =
        library.shows.iter().flat_map(|show| &show.placements).map(|placement| placement.rating_key.as_str()).collect();
    let known = |key: &Option<String>, keys: &HashSet<&str>| key.as_deref().is_some_and(|key| keys.contains(key));
    rows.into_iter()
        .filter(|row| {
            row.media == Media::Episode
                && row.guid.is_some()
                && !known(&row.parent_rating_key, &seasons)
                && !known(&row.grandparent_rating_key, &shows)
        })
        .count()
}

/// Play rows some item claims only through a `plex://` GUID.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GuidJoins {
    pub movies: usize,
    pub episodes: usize,
}

pub fn guid_joins(joins: &[PlayJoin], rows: &[RowKey]) -> GuidJoins {
    let mut counted = GuidJoins::default();
    for row in rows.iter().filter(|row| row.guid.is_some()) {
        if joins.iter().any(|join| join.matches_by_guid(row) && !join.matches_by_keys(row)) {
            match row.media {
                Media::Movie => counted.movies += 1,
                Media::Episode => counted.episodes += 1,
            }
        }
    }
    counted
}

#[cfg(test)]
mod tests;
