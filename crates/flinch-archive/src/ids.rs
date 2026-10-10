//! Identity across the stack: the ids each system uses for the same item.
//!
//! Titles are not identity — "Superman" is two films, "Invasion" is two shows,
//! and a localized Plex title matches nothing. The *arrs carry external ids
//! (TMDB, TVDB, IMDb), Plex exposes the same ids as GUIDs, and Maintainerr acts
//! only on Plex ratingKeys. Every join between them goes through these types.
//!
//! FLINCH's own card ids name an item in its *arr instance: `radarr-<movie>`
//! and `sonarr-<series>-s<season>` (a show's subject is `sonarr-<series>`) for
//! the default instance, the one every install had before several were read,
//! so its ids and every state file keyed by them read on unchanged. Another
//! instance qualifies the app with its name: `radarr@4k-<movie>`,
//! `sonarr@anime-<series>-s<season>`. *arr ids are per instance, so the name
//! is what keeps two instances' movie 7 apart; [`ArrRef::parse`] is the one
//! reader of the format.

use crate::capacity::App;
use serde::{Deserialize, Serialize};

/// The default instance's name: none.
pub const DEFAULT_INSTANCE: &str = "";

/// The longest instance name: it becomes part of ids and file names.
pub const INSTANCE_NAME_MAX: usize = 24;

/// An extra instance's name: 1–24 of `a-z`, `0-9` and `_`. Never `-` (it ends
/// the name in an id) nor anything a file name or URL would have to escape.
pub fn valid_instance_name(name: &str) -> bool {
    (1..=INSTANCE_NAME_MAX).contains(&name.len())
        && name.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

/// `radarr`, or `radarr@4k` for a named instance: what every id of the
/// instance starts with, and how logs and the UI name it.
pub fn instance_key(app: App, instance: &str) -> String {
    if instance.is_empty() {
        app.label().to_string()
    } else {
        format!("{}@{instance}", app.label())
    }
}

/// A movie's card id.
pub fn movie_card_id(instance: &str, movie: u32) -> String {
    format!("{}-{movie}", instance_key(App::Radarr, instance))
}

/// A season's card id.
pub fn season_card_id(instance: &str, series: u32, season: u32) -> String {
    format!("{}-{series}-s{season}", instance_key(App::Sonarr, instance))
}

/// A show's subject: what its seasons share ([`crate::embedding::subject_of`]).
pub fn show_subject(instance: &str, series: u32) -> String {
    format!("{}-{series}", instance_key(App::Sonarr, instance))
}

/// What a card id or show subject names: the app, the instance (empty for
/// the default), the item's id there, and the season of a season card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArrRef<'a> {
    pub app: App,
    pub instance: &'a str,
    pub id: u32,
    pub season: Option<u32>,
}

impl<'a> ArrRef<'a> {
    /// A movie (`radarr[@name]-<id>`), a season (`sonarr[@name]-<id>-s<n>`) or
    /// a show subject (`sonarr[@name]-<id>`); anything else is `None`.
    pub fn parse(text: &'a str) -> Option<Self> {
        let number = |text: &str| -> Option<u32> {
            (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())).then(|| text.parse().ok()).flatten()
        };
        let (app, rest) = match (text.strip_prefix("radarr"), text.strip_prefix("sonarr")) {
            (Some(rest), _) => (App::Radarr, rest),
            (None, Some(rest)) => (App::Sonarr, rest),
            (None, None) => return None,
        };
        let (instance, rest) = match rest.strip_prefix('@') {
            Some(named) => named.split_once('-').filter(|(name, _)| valid_instance_name(name))?,
            None => (DEFAULT_INSTANCE, rest.strip_prefix('-')?),
        };
        let (id, season) = match (app, rest.split_once("-s")) {
            (App::Sonarr, Some((series, season))) => (number(series)?, Some(number(season)?)),
            _ => (number(rest)?, None),
        };
        Some(Self { app, instance, id, season })
    }

    /// A movie or season card id; a show subject is not a card.
    pub fn card(text: &'a str) -> Option<Self> {
        Self::parse(text).filter(|item| item.app == App::Radarr || item.season.is_some())
    }

    /// The card id or subject this names, as [`movie_card_id`],
    /// [`season_card_id`] and [`show_subject`] write it.
    pub fn id_text(&self) -> String {
        match (self.app, self.season) {
            (App::Radarr, _) => movie_card_id(self.instance, self.id),
            (App::Sonarr, Some(season)) => season_card_id(self.instance, self.id, season),
            (App::Sonarr, None) => show_subject(self.instance, self.id),
        }
    }

    /// The show a season belongs to; a movie or show is its own.
    pub fn subject(self) -> Self {
        Self { season: None, ..self }
    }
}

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

#[cfg(test)]
mod tests;
