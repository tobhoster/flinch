//! Identity across the stack: the ids each system uses for the same item.
//!
//! Titles are not identity — "Superman" is two films, "Invasion" is two shows,
//! and a localized Plex title matches nothing. The *arrs carry external ids
//! (TMDB, TVDB, IMDb), Plex exposes the same ids as GUIDs, and Maintainerr acts
//! only on Plex ratingKeys. Every join between them goes through these types.

use serde::{Deserialize, Serialize};

/// External catalogue ids an *arr knows for an item.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExternalIds {
    #[serde(default)]
    pub tmdb: Option<u32>,
    #[serde(default)]
    pub tvdb: Option<u32>,
    #[serde(default)]
    pub imdb: Option<String>,
}

impl ExternalIds {
    pub fn is_empty(&self) -> bool {
        self.tmdb.is_none() && self.tvdb.is_none() && self.imdb.is_none()
    }
}

/// Where an item lives in Plex — exactly what Maintainerr needs to act on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlexIds {
    /// The movie's ratingKey, or the *show's* ratingKey for a season (the
    /// `mediaId` Maintainerr expects for both).
    pub rating_key: String,
    /// The season's own ratingKey; `None` for movies.
    #[serde(default)]
    pub season_rating_key: Option<String>,
    /// The Plex library section the item belongs to (a Maintainerr collection
    /// is bound to one section).
    #[serde(default)]
    pub section_id: Option<u32>,
}
