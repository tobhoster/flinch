//! One card per library item (a season or a movie): the facts the planner and
//! the item view read, built from the *arr inventory and then the media
//! server's watch state ([`crate::watch::apply`]).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LibraryKind {
    Season,
    Movie,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchiveCard {
    pub id: String,
    pub title: String,
    pub kind: LibraryKind,
    /// Bytes on disk.
    pub size_bytes: u64,
    /// Days since the file arrived.
    pub added_days_ago: f32,
    /// Days since anyone watched any part. `None` = never watched.
    pub last_watched_days: Option<f32>,
    /// On a keep list: a keep collection, the keep tag, or the operator's own
    /// Maintainerr exclusion. Pinned.
    pub in_keep_collection: bool,
    /// A favorite. Pinned.
    pub is_favorite: bool,

    // Season-only
    #[serde(default)]
    pub season_index: Option<u32>,
    #[serde(default)]
    pub episodes_total: Option<u32>,
    #[serde(default)]
    pub episodes_watched: Option<u32>,
    /// Episode numbers with a file, ascending, once Sonarr was asked; `None`
    /// when unknown. Plays of episodes not in it count for nothing: a deleted
    /// episode's play must not make the files that are left read as watched.
    #[serde(default)]
    pub episodes_on_disk: Option<Vec<u32>>,

    // Movie-only
    #[serde(default)]
    pub is_watched: Option<bool>,
    /// Release year (movies) — part of the Plex correlation key.
    #[serde(default)]
    pub movie_year: Option<u32>,
    /// Show title without the season suffix (seasons) — the other half of the
    /// Plex correlation key. Kept separate from `title` so display text stays
    /// "Show S2" while matching uses "Show".
    #[serde(default)]
    pub show_title: Option<String>,
}
