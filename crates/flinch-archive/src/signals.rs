//! What the household and the download stack say beyond the *arr libraries:
//! what was imported lately and what is still downloading (Radarr/Sonarr),
//! who asked for what and who wants it (Seerr requests and watchlists), and
//! how hard a title would be to get back (Prowlarr search results against the
//! SABnzbd servers' retention).
//!
//! This module holds the types and the pure wire parsers; the daemon does the
//! reading. Every source is best-effort: one that cannot be read leaves its
//! field empty and adds one sentence to [`Signals::problems`].

pub mod arr;
pub mod release;
pub mod seerr;

use crate::capacity::App;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Which *arr item an event concerns. Sonarr events carry the season when known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemRef {
    Movie(u64),
    Series { series_id: u64, season: Option<u32> },
}

/// A Seerr/TMDB-side identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaRef {
    Movie { tmdb: u64 },
    Show { tvdb: Option<u64>, tmdb: Option<u64> },
}

/// One file that landed in a library: a movie, or one episode of a season.
/// Imports measure growth; grabs do not — a download can be grabbed many
/// times and never import (seen live: 143 grabs, 10 files).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Import {
    pub app: App,
    pub item: ItemRef,
    /// When it was imported, unix seconds.
    pub epoch: u64,
    /// The file's size.
    pub bytes: u64,
}

/// One download in an app's queue, counted once however many episodes it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Queued {
    pub app: App,
    pub item: ItemRef,
    pub bytes_left: u64,
}

/// A Seerr request that was not declined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub media: MediaRef,
    /// The seasons asked for; empty = the whole show, or a movie.
    pub seasons: Vec<u32>,
    /// The requester's Seerr display name.
    pub requester: String,
}

/// A title on a Seerr user's (Plex) watchlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watchlisted {
    pub media: MediaRef,
    /// The user's Seerr display name.
    pub user: String,
}

/// How available a title is to download again. `None` = not known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Release {
    /// The best-seeded torrent's seeders; `Some(0)` when searched and none found.
    pub seeders: Option<u32>,
    /// Whether no usenet post is within the servers' retention; `None` when
    /// the retention is unknown.
    pub usenet_out_of_retention: Option<bool>,
}

/// Everything gathered for one cycle.
#[derive(Debug, Default)]
pub struct Signals {
    /// Imports of the last [`arr::IMPORT_WINDOW_SECS`].
    pub imports: Vec<Import>,
    pub queue: Vec<Queued>,
    /// Every request not declined.
    pub requests: Vec<Request>,
    pub watchlists: Vec<Watchlisted>,
    /// Card id → availability, from the release cache.
    pub releases: HashMap<String, Release>,
    /// Each degraded source, one short sentence.
    pub problems: Vec<String>,
}

/// Parse raw rows one by one: a malformed row is skipped, never the page.
fn rows<T: serde::de::DeserializeOwned>(rows: Vec<serde_json::Value>) -> impl Iterator<Item = T> {
    crate::arr::parse_rows(rows).parsed.into_iter()
}

#[cfg(test)]
mod tests;
