//! Recognising a library item's plays in play logs (Plex history, Tautulli).
//!
//! Rows join by ratingKey wherever the item resolved, and by Plex's own
//! `plex://` GUID where Plex has since re-added the item under a new ratingKey
//! (see [`super::migration`]). An unresolved movie may fall back to an exact
//! title and a year the row states; an unresolved season joins nothing.

use super::guid::plex_guid;
use super::resolve::PlayKeys;
use super::{normalise, PlexMetadata};
use crate::card::LibraryKind;

/// What kind of play a row records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Media {
    Movie,
    Episode,
}

/// The identifying fields of one play row, from either source.
#[derive(Debug, Clone, PartialEq)]
pub struct RowKey {
    pub media: Media,
    pub rating_key: Option<String>,
    pub parent_rating_key: Option<String>,
    pub grandparent_rating_key: Option<String>,
    /// The played item's `plex://movie/…` or `plex://episode/…` GUID; agent
    /// GUIDs are dropped.
    pub guid: Option<String>,
    /// The episode's season number.
    pub season: Option<u32>,
    /// Normalised movie title.
    pub title: String,
    /// The movie's year, when the row states it.
    pub year: Option<u32>,
}

fn present(key: Option<&str>) -> Option<String> {
    key.map(str::trim).filter(|key| !key.is_empty()).map(str::to_string)
}

impl RowKey {
    /// A Plex history row's key; `None` for anything but a movie or episode.
    pub fn plex(row: &PlexMetadata) -> Option<Self> {
        Self::new(
            &row.media_type,
            [Some(row.rating_key.as_str()), row.parent_rating_key.as_deref(), row.grandparent_rating_key.as_deref()],
            row.guid.as_deref(),
            row.parent_index,
            &row.title,
            row.year,
        )
    }

    /// Build from raw fields: `keys` is (ratingKey, parent, grandparent).
    pub fn new(
        media_type: &str,
        keys: [Option<&str>; 3],
        guid: Option<&str>,
        season: Option<u32>,
        title: &str,
        year: Option<u32>,
    ) -> Option<Self> {
        let (media, kind) = if media_type.eq_ignore_ascii_case("movie") {
            (Media::Movie, "movie")
        } else if media_type.eq_ignore_ascii_case("episode") {
            (Media::Episode, "episode")
        } else {
            return None;
        };
        let [rating_key, parent, grandparent] = keys;
        Some(Self {
            media,
            rating_key: present(rating_key),
            parent_rating_key: present(parent),
            grandparent_rating_key: present(grandparent),
            guid: guid.and_then(|guid| plex_guid(guid, kind)).map(str::to_string),
            season,
            title: normalise(title),
            year,
        })
    }
}

/// How one item's plays are recognised.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayJoin {
    Keys(PlayKeys),
    /// An unresolved movie: exact normalised title and a year the row states.
    MovieTitleYear { title: String, year: u32 },
    /// Nothing identifies its plays safely.
    Unresolved,
}

impl PlayJoin {
    /// The join for an item no Plex resolution covers.
    pub fn fallback(kind: LibraryKind, title: &str, year: Option<u32>) -> Self {
        match (kind, year) {
            (LibraryKind::Movie, Some(year)) => PlayJoin::MovieTitleYear { title: normalise(title), year },
            _ => PlayJoin::Unresolved,
        }
    }

    pub fn matches(&self, row: &RowKey) -> bool {
        self.matches_by_keys(row) || self.matches_by_guid(row)
    }

    /// A match through the ratingKeys (or, unresolved, the title and year).
    pub fn matches_by_keys(&self, row: &RowKey) -> bool {
        let within = |keys: &[String], key: &Option<String>| key.as_ref().is_some_and(|key| keys.contains(key));
        match self {
            PlayJoin::Keys(PlayKeys::Movie { rating_keys, .. }) => row.media == Media::Movie && within(rating_keys, &row.rating_key),
            PlayJoin::Keys(PlayKeys::Season { show_rating_keys, season_rating_keys, season, .. }) => {
                row.media == Media::Episode
                    && (within(season_rating_keys, &row.parent_rating_key)
                        || (within(show_rating_keys, &row.grandparent_rating_key) && row.season == Some(*season)))
            }
            PlayJoin::MovieTitleYear { title, year } => row.media == Media::Movie && row.year == Some(*year) && row.title == *title,
            PlayJoin::Unresolved => false,
        }
    }

    /// A match through the row's `plex://` GUID: the movie's own, or one of
    /// the episodes Plex files under the season now. Never a title.
    pub fn matches_by_guid(&self, row: &RowKey) -> bool {
        let Some(guid) = &row.guid else { return false };
        match self {
            PlayJoin::Keys(PlayKeys::Movie { plex_guids, .. }) => row.media == Media::Movie && plex_guids.contains(guid),
            PlayJoin::Keys(PlayKeys::Season { episode_guids, .. }) => row.media == Media::Episode && episode_guids.contains(guid),
            PlayJoin::MovieTitleYear { .. } | PlayJoin::Unresolved => false,
        }
    }

    /// Whether a row is a play by the item's audience: for a season, an episode
    /// of any season of an accepted show, or one of its own plays; otherwise
    /// the item's own plays.
    pub fn matches_audience(&self, row: &RowKey) -> bool {
        match self {
            PlayJoin::Keys(PlayKeys::Season { show_rating_keys, .. }) => {
                (row.media == Media::Episode
                    && row.grandparent_rating_key.as_ref().is_some_and(|key| show_rating_keys.contains(key)))
                    || self.matches_by_guid(row)
            }
            other => other.matches(row),
        }
    }
}
