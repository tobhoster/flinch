//! Plex watch-state source.
//!
//! The archive guard's sharpest input is "has anyone actually watched this?",
//! and the media server is where that lives. This module parses its payloads;
//! [`library`] indexes them by external catalogue id, [`resolve`] joins *arr
//! items to them, [`history`] reads server-wide playback, and [`migration`]
//! keeps plays recorded before a library re-add joined.
//!
//! Identity is by GUID (`tmdb://`, `tvdb://`, `imdb://`), never by title alone:
//! a title join turned a 1978 play into a watch of the 2025 remake and handed a
//! never-watched film to the reclaimer. A title is only accepted together with
//! a year both sides state, and only when exactly one item carries it.

use crate::ids::ExternalIds;
use serde::{Deserialize, Serialize};

pub mod episodes;
pub mod guid;
pub mod history;
pub mod join;
pub mod library;
pub mod migration;
pub mod resolve;

pub use episodes::{EpisodeIds, PlexEpisodes, SonarrEpisodes};
pub use join::{PlayJoin, RowKey};
pub use library::{Placement, PlexLibrary};
pub use migration::EpisodeGuids;
pub use resolve::{resolve, MatchedBy, PlayKeys, PlexMatch, Resolution, Unconfirmed};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WatchInfo {
    /// 0.0 when nothing was ever played, 1.0 when every child is watched.
    pub watched_fraction: f32,
    /// Unix seconds of the most recent play; `None` when never played.
    pub last_viewed_unix: Option<u64>,
}

impl WatchInfo {
    pub fn is_watched(&self) -> bool {
        self.watched_fraction >= 0.999
    }

    /// The same item seen twice (two libraries): whatever either copy saw.
    pub fn merge(self, other: WatchInfo) -> WatchInfo {
        WatchInfo {
            watched_fraction: self.watched_fraction.max(other.watched_fraction),
            last_viewed_unix: self.last_viewed_unix.max(other.last_viewed_unix),
        }
    }
}

/// Normalise a title for the exact title+year fallback: case, punctuation and
/// spacing only. A trailing `(YYYY)` is kept — it is the one thing that tells a
/// remake from its original.
fn normalise(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Title normalisation exposed for diagnostics and for the play logs.
pub fn public_normalise(title: &str) -> String {
    normalise(title)
}

/// Plex wraps every response in a capitalized `MediaContainer` envelope;
/// reading the root directly yields an empty container, which is exactly how
/// the first live run reported "0 movies, 0 shows, 0 seasons" against a library
/// that plainly has both.
#[derive(Debug, Clone, Deserialize)]
pub struct PlexEnvelope {
    #[serde(rename = "MediaContainer")]
    pub container: PlexContainer,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlexContainer {
    /// Rows in this page.
    #[serde(default)]
    pub size: u32,
    /// Rows the whole listing holds; paging is complete once this many are read.
    #[serde(rename = "totalSize", default)]
    pub total_size: Option<u64>,
    #[serde(rename = "Directory", default)]
    pub directory: Vec<PlexDirectory>,
    #[serde(rename = "Metadata", default)]
    pub metadata: Vec<PlexMetadata>,
    /// `/accounts`: the server's local accounts.
    #[serde(rename = "Account", default)]
    pub account: Vec<PlexAccount>,
}

/// Directory rows appear with different fields depending on the endpoint —
/// `/library/sections` fills `type`, other listings may omit it. Parse
/// tolerantly and filter on what the caller needs.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlexDirectory {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub title: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    /// Filter choices (`/library/sections/{id}/label`): the listing of every
    /// item carrying this one, e.g. `/library/sections/1/all?label=4466`.
    #[serde(default)]
    pub fast_key: String,
}

/// One local account on the server. Id 0 is the server's own system account.
#[derive(Debug, Clone, Deserialize)]
pub struct PlexAccount {
    #[serde(default)]
    pub id: u64,
}

/// A `Guid` element (`includeGuids=1`): `{"id": "tmdb://603"}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PlexGuid {
    #[serde(default)]
    pub id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlexMetadata {
    /// Absent on some rows of mixed payloads; entries without it are skipped by
    /// the caller rather than failing the whole response.
    #[serde(default)]
    pub rating_key: String,
    /// The season's key on an episode row, the show's key on a season row.
    #[serde(default)]
    pub parent_rating_key: Option<String>,
    /// The show's key on an episode row.
    #[serde(default)]
    pub grandparent_rating_key: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default, deserialize_with = "lenient")]
    pub year: Option<u32>,
    /// Legacy agent GUID (`com.plexapp.agents.imdb://tt…`) or Plex's own
    /// `plex://movie/…` one, which outlives a library re-add.
    #[serde(default)]
    pub guid: Option<String>,
    /// External ids from `includeGuids=1`.
    #[serde(rename = "Guid", default)]
    pub guids: Vec<PlexGuid>,
    #[serde(rename = "librarySectionID", default, deserialize_with = "lenient")]
    pub library_section_id: Option<u32>,
    #[serde(default, deserialize_with = "lenient")]
    pub leaf_count: Option<u32>,
    #[serde(default, deserialize_with = "lenient")]
    pub viewed_leaf_count: Option<u32>,
    #[serde(default, deserialize_with = "lenient")]
    pub view_count: Option<u32>,
    #[serde(default, deserialize_with = "lenient")]
    pub last_viewed_at: Option<u64>,
    /// A season row's own number; an episode row's episode number.
    #[serde(default, deserialize_with = "lenient")]
    pub index: Option<u32>,
    /// An episode row's season number.
    #[serde(default, deserialize_with = "lenient")]
    pub parent_index: Option<u32>,
    /// Plex's `type` discriminator (`"season"`, `"episode"`, `"movie"`, `"show"`).
    #[serde(rename = "type", default)]
    pub media_type: String,
    /// Show title on history episode rows.
    #[serde(default)]
    pub grandparent_title: Option<String>,
    /// When a history row happened (`viewedAt`). Distinct from an item's
    /// `lastViewedAt`: this one is a record of one play, not item state.
    #[serde(default, deserialize_with = "lenient")]
    pub viewed_at: Option<u64>,
    /// Local account that played a history row (`accountID`): the viewer.
    #[serde(rename = "accountID", default, deserialize_with = "lenient")]
    pub account_id: Option<u64>,
}

/// A number Plex may render as a string: history rows carry
/// `"librarySectionID": "1"` where listings carry `1`. One such field must not
/// fail a whole page (and with it the history read and never-played reclaim);
/// an unparseable value reads as absent.
fn lenient<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: std::str::FromStr + serde::de::DeserializeOwned,
{
    Ok(match Option::<serde_json::Value>::deserialize(deserializer)? {
        Some(serde_json::Value::String(text)) => text.trim().parse().ok(),
        Some(value) => serde_json::from_value(value).ok(),
        None => None,
    })
}

impl PlexMetadata {
    /// Watch state of a single movie.
    ///
    /// Watched if EITHER signal says so: Plex omits `viewCount` entirely when it
    /// is 1 (observed live on a movie that had only `lastViewedAt`), so trusting
    /// the count alone would silently classify watched movies as unwatched.
    pub fn movie_watch(&self) -> WatchInfo {
        let watched = self.view_count.unwrap_or(0) > 0 || self.last_viewed_at.is_some();
        WatchInfo { watched_fraction: if watched { 1.0 } else { 0.0 }, last_viewed_unix: self.last_viewed_at }
    }

    /// Watch state of a show or season from its leaf counts (episodes watched / held).
    pub fn leaf_watch(&self) -> WatchInfo {
        let total = self.leaf_count.unwrap_or(0);
        let viewed = self.viewed_leaf_count.unwrap_or(0);
        WatchInfo {
            watched_fraction: if total == 0 { 0.0 } else { (viewed as f32 / total as f32).min(1.0) },
            last_viewed_unix: self.last_viewed_at,
        }
    }

    /// The external catalogue ids this row's GUIDs name.
    pub fn external_ids(&self) -> ExternalIds {
        guid::external_ids(self.guids.iter().map(|guid| guid.id.as_str()).chain(self.guid.as_deref()))
    }
}

/// One library item the media server can be asked about.
///
/// Deliberately not `ArchiveCard`: watch evidence belongs to the *library*, not
/// to what happens to be on disk. Reading it off cards meant an item whose file
/// was already gone — "I did watch that" — could show no watch state at all.
#[derive(Debug, Clone)]
pub struct WatchTarget {
    /// Owned throughout: the list is built before the watch map is merged into
    /// the cards, so borrowing card fields here would freeze the cards for the
    /// rest of the cycle. One small allocation per library item, once per run.
    pub id: String,
    pub kind: crate::card::LibraryKind,
    /// The movie title, or the show title for a season.
    pub title: String,
    /// The movie's year, or the show's first-air year for a season.
    pub year: Option<u32>,
    pub show_title: Option<String>,
    pub season_index: Option<u32>,
    pub episodes_total: Option<u32>,
    /// Episode files Sonarr holds for this season. A Plex season is accepted
    /// only when its `leafCount` agrees: the two sides can number seasons
    /// differently (TVDB vs TMDB ordering), and a disagreement means the
    /// evidence would land on the wrong season.
    pub episode_files: Option<u32>,
    /// The *arr's catalogue ids (a season carries its show's).
    pub external: ExternalIds,
    /// When the item arrived, as an epoch. Used to decide whether "no stream"
    /// is evidence or merely a blind spot.
    pub added_epoch: Option<u64>,
    /// Whether the item has files on disk.
    pub on_disk: bool,
}

impl From<&crate::card::ArchiveCard> for WatchTarget {
    fn from(card: &crate::card::ArchiveCard) -> Self {
        let (title, year) = match card.kind {
            crate::card::LibraryKind::Movie => (card.title.clone(), card.movie_year),
            crate::card::LibraryKind::Season => (card.show_title.clone().unwrap_or_else(|| card.title.clone()), None),
        };
        Self {
            id: card.id.clone(),
            kind: card.kind,
            title,
            year,
            show_title: card.show_title.clone(),
            season_index: card.season_index,
            episodes_total: card.episodes_total,
            episode_files: None,
            external: ExternalIds::default(),
            added_epoch: None,
            on_disk: true,
        }
    }
}

/// Days since a play, or `None` when never played.
pub fn age_days(last_viewed_unix: Option<u64>, now_unix: u64) -> Option<f32> {
    last_viewed_unix.map(|t| now_unix.saturating_sub(t) as f32 / 86_400.0)
}

#[cfg(test)]
mod tests;
