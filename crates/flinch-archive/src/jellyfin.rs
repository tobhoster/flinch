//! Jellyfin and Emby watch-state source.
//!
//! Plex keeps one borrowed account's item state plus a server-wide history;
//! Jellyfin and Emby keep neither a history nor a household view, but they
//! answer per user: every user's `UserData` (`Played`, `PlayCount`,
//! `LastPlayedDate`, `PlaybackPositionTicks`) on every item. Reading all users
//! therefore gives the household's own record — as long as *every* user and
//! every item was read, which [`JellyfinRead::complete`] records.
//!
//! What this record lacks next to Plex history or Tautulli: one date per item
//! and user (the *last* play), no earlier plays, no stream lengths, no plays of
//! items removed from the server. A rewatch is therefore invisible, and a play
//! row exists only where `LastPlayedDate` does. Jellyfin's optional Playback
//! Reporting plugin keeps a full log, but it is not read here.
//!
//! Identity is by catalogue id (`ProviderIds` Tmdb/Tvdb/Imdb), exactly like
//! Plex GUIDs: a movie by its TMDB or IMDb id, an episode by its series' ids
//! and its season number. Titles are never used.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

mod client;
pub mod collections;
mod map;
mod recheck;

#[cfg(test)]
mod tests;

pub use client::JellyfinClient;
pub use collections::JellyfinCollections;
pub(crate) use map::parse_utc;
pub use map::{evidence, CardPlays, JellyfinEvidence, PLAYS_FILE};
pub use recheck::last_look;

/// Which server speaks: the two share the item model but differ in how the
/// token is sent and where per-user items are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerKind {
    #[default]
    Jellyfin,
    Emby,
}

impl ServerKind {
    pub fn label(self) -> &'static str {
        match self {
            ServerKind::Jellyfin => "jellyfin",
            ServerKind::Emby => "emby",
        }
    }
}

/// Operator settings for a Jellyfin or Emby server. Off while `url` is blank.
///
/// The key is either typed into Settings (`token`, never sent back to the
/// browser) or named by environment variable (`api_key_env`), so it can live in
/// a secret instead of `settings.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct JellyfinConfig {
    pub url: String,
    pub token: String,
    pub api_key_env: String,
    pub kind: ServerKind,
}

/// Why a Jellyfin configuration was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidJellyfinConfig(pub &'static str);

impl JellyfinConfig {
    pub fn enabled(&self) -> bool {
        !self.url.trim().is_empty()
    }

    pub fn validate(&self) -> Result<(), InvalidJellyfinConfig> {
        let env = self.api_key_env.trim();
        if !env.is_empty() && !env.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(InvalidJellyfinConfig("the Jellyfin API key variable (jellyfin.api_key_env) must be letters, digits and _"));
        }
        if self.url.trim().chars().any(char::is_whitespace) {
            return Err(InvalidJellyfinConfig("the Jellyfin URL (jellyfin.url) must not contain spaces"));
        }
        Ok(())
    }

    /// The API key: the Settings token, else the named variable. `None` when
    /// neither holds one.
    pub fn api_key(&self) -> Option<String> {
        let token = self.token.trim();
        if !token.is_empty() {
            return Some(token.to_string());
        }
        let env = self.api_key_env.trim();
        (!env.is_empty()).then(|| std::env::var(env).ok()).flatten().map(|key| key.trim().to_string()).filter(|key| !key.is_empty())
    }
}

/// What went wrong talking to the server. Never carries the URL or the key.
#[derive(Debug, thiserror::Error)]
pub enum JellyfinError {
    #[error("jellyfin {endpoint}: request failed: {source}")]
    Transport { endpoint: &'static str, source: reqwest::Error },
    #[error("jellyfin {endpoint}: HTTP {status}")]
    Status { endpoint: &'static str, status: u16 },
    #[error("jellyfin {endpoint}: {source}")]
    Body { endpoint: &'static str, source: crate::body::BodyError },
    #[error("jellyfin {endpoint}: unreadable response: {source}")]
    Parse { endpoint: &'static str, source: serde_json::Error },
    #[error("jellyfin items: {0}")]
    Incomplete(&'static str),
    #[error("jellyfin {endpoint}: unexpected response: {detail}")]
    Unexpected { endpoint: &'static str, detail: String },
    /// The server answered 2xx to a write its read-back does not show.
    #[error("jellyfin {endpoint}: write not applied: {detail}")]
    NotApplied { endpoint: &'static str, detail: String },
}

/// One server user.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct JellyfinUser {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// Absent on servers that omit it: such a user is never taken for an admin.
    #[serde(default)]
    pub policy: Option<UserPolicy>,
}

/// The part of a user's policy FLINCH reads.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct UserPolicy {
    pub is_administrator: bool,
}

/// One user's state on one item.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct UserData {
    pub played: bool,
    pub play_count: u32,
    /// ISO 8601, UTC.
    pub last_played_date: Option<String>,
    pub playback_position_ticks: u64,
}

impl UserData {
    /// The user touched the item at all.
    pub fn touched(&self) -> bool {
        self.played || self.play_count > 0 || self.playback_position_ticks > 0
    }
}

/// One `BaseItemDto` row, only the fields the join reads.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct JellyfinItem {
    pub id: String,
    /// `Movie`, `Series` or `Episode`.
    #[serde(rename = "Type")]
    pub kind: String,
    /// Keys as the server writes them (`Tmdb`, `Tvdb`, `Imdb`); read
    /// case-insensitively.
    pub provider_ids: HashMap<String, String>,
    pub series_id: Option<String>,
    /// An episode's season number.
    pub parent_index_number: Option<u32>,
    pub index_number: Option<u32>,
    /// `Virtual` marks a missing episode the server only knows from metadata.
    pub location_type: Option<String>,
    pub run_time_ticks: Option<u64>,
    pub user_data: Option<UserData>,
    /// An episode's season item: the id a Season card is shelved under.
    pub season_id: Option<String>,
    /// The item's display name; read for collections, which have no ids.
    pub name: String,
}

impl JellyfinItem {
    pub fn provider(&self, name: &str) -> Option<&str> {
        self.provider_ids.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.trim()).filter(|v| !v.is_empty())
    }

    fn is_virtual(&self) -> bool {
        self.location_type.as_deref().is_some_and(|kind| kind.eq_ignore_ascii_case("virtual"))
    }
}

/// Everything one user can see, with their state on it.
#[derive(Debug, Clone, Default)]
pub struct UserItems {
    pub user_id: String,
    /// The user's display name, for `ignore_viewers` ([`crate::viewers`]).
    pub name: String,
    pub items: Vec<JellyfinItem>,
}

/// One cycle's read of the server.
#[derive(Debug, Clone, Default)]
pub struct JellyfinRead {
    pub users: Vec<UserItems>,
    /// Every user was listed and each one's items paged to the server's own
    /// total. Only then is "nobody played it" a claim about the whole record.
    pub complete: bool,
    /// Why the read is incomplete, one line per failed user (no URL, no key).
    pub problems: Vec<String>,
}
