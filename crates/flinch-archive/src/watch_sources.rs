//! Tracearr and Trakt as watch-evidence sources.
//!
//! Both keep a *play log* with catalogue ids on every row, which is exactly
//! FLINCH's identity rule: a movie joins by its TMDB/IMDb id, an episode by its
//! show's TVDB/TMDB/IMDb id and its season number, never by title.
//!
//! - **Tracearr** monitors Plex, Jellyfin and Emby in one place and keeps every
//!   session. Its history is read per Tracearr user, every page to the end, so
//!   a complete read can also prove *absence* (nobody played an item since
//!   Tracearr's record began), the way Tautulli's can.
//! - **Trakt** is one account's scrobble log. One configured entry per
//!   household member; it proves plays, never their absence, because nothing
//!   says every viewer scrobbles.
//!
//! The plays reach the planner as watch entries and the fitter through
//! [`PLAYS_FILE`], the same way Jellyfin's do.

use crate::fit::plays::Viewer;
use crate::ids::ExternalIds;
use serde::{Deserialize, Serialize};

mod join;
mod tracearr;
mod trakt;

#[cfg(test)]
mod tests;

pub use join::{evidence, SourceEvidence};
pub use tracearr::TracearrClient;
pub use trakt::TraktClient;

/// The state-directory file holding the last cycle's watch-source plays by
/// card id (as [`crate::jellyfin::CardPlays`]), read by the fitter.
pub const PLAYS_FILE: &str = "watch-source-plays.json";

/// Trakt's API host ([Trakt API docs](https://trakt.docs.apiary.io/)).
pub const TRAKT_API: &str = "https://api.trakt.tv";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    #[default]
    Tracearr,
    Trakt,
}

impl SourceKind {
    pub fn label(self) -> &'static str {
        match self {
            SourceKind::Tracearr => "tracearr",
            SourceKind::Trakt => "trakt",
        }
    }
}

/// One configured source. Secrets never live in `settings.json`: the token,
/// and Trakt's client id, are named by environment variable.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WatchSourceConfig {
    pub kind: SourceKind,
    /// Shown in logs and status; for Trakt, whose account this is.
    pub name: String,
    /// Tracearr's base URL; Trakt's API host when blank.
    pub url: String,
    /// Variable holding Tracearr's public API key (`trr_pub_…`) or the Trakt
    /// account's OAuth access token (from the device flow).
    pub token_env: String,
    /// Trakt only: variable holding the Trakt application's client id.
    pub client_id_env: String,
    /// Tracearr only: days of history the operator keeps, when sessions are
    /// pruned by hand; 0 = Tracearr's own default, which keeps every session.
    pub retention_days: u32,
}

/// Why a source configuration was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidWatchSource(pub &'static str);

fn env_name_ok(name: &str) -> bool {
    name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl WatchSourceConfig {
    pub fn validate(&self) -> Result<(), InvalidWatchSource> {
        let token_env = self.token_env.trim();
        if token_env.is_empty() || !env_name_ok(token_env) {
            return Err(InvalidWatchSource("a watch source needs its token variable (watch_sources.token_env): letters, digits and _"));
        }
        if !env_name_ok(self.client_id_env.trim()) {
            return Err(InvalidWatchSource("the Trakt client id variable (watch_sources.client_id_env) must be letters, digits and _"));
        }
        if self.url.trim().chars().any(char::is_whitespace) {
            return Err(InvalidWatchSource("a watch source URL (watch_sources.url) must not contain spaces"));
        }
        match self.kind {
            SourceKind::Tracearr if self.url.trim().is_empty() => {
                Err(InvalidWatchSource("a Tracearr source needs its URL (watch_sources.url)"))
            }
            SourceKind::Tracearr if self.retention_days > 36_500 => {
                Err(InvalidWatchSource("Tracearr retention (watch_sources.retention_days) must be at most 36500 days"))
            }
            SourceKind::Trakt if self.client_id_env.trim().is_empty() => {
                Err(InvalidWatchSource("a Trakt source needs its client id variable (watch_sources.client_id_env)"))
            }
            _ => Ok(()),
        }
    }

    /// What logs and status call this source: its name, else its kind.
    pub fn display(&self) -> String {
        match self.name.trim() {
            "" => self.kind.label().to_string(),
            name => format!("{} ({name})", self.kind.label()),
        }
    }

    /// The base URL requests go to.
    pub fn base(&self) -> String {
        match (self.kind, self.url.trim()) {
            (SourceKind::Trakt, "") => TRAKT_API.to_string(),
            (_, url) => url.to_string(),
        }
    }
}

/// Every configured source, each validated.
pub fn validate(sources: &[WatchSourceConfig]) -> Result<(), InvalidWatchSource> {
    sources.iter().try_for_each(WatchSourceConfig::validate)
}

/// A named variable's value, trimmed; `None` when unset or blank.
pub fn env_secret(name: &str) -> Option<String> {
    let name = name.trim();
    (!name.is_empty()).then(|| std::env::var(name).ok()).flatten().map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

/// What went wrong talking to a source. Never carries a URL or a token.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("{source_kind} {endpoint}: request failed: {error}")]
    Transport { source_kind: &'static str, endpoint: &'static str, error: reqwest::Error },
    #[error("{source_kind} {endpoint}: HTTP {status}")]
    Status { source_kind: &'static str, endpoint: &'static str, status: u16 },
    #[error("{source_kind} {endpoint}: {error}")]
    Body { source_kind: &'static str, endpoint: &'static str, error: crate::body::BodyError },
    #[error("{source_kind} {endpoint}: unreadable response: {error}")]
    Parse { source_kind: &'static str, endpoint: &'static str, error: serde_json::Error },
    #[error("{source_kind} {endpoint}: {why}")]
    Incomplete { source_kind: &'static str, endpoint: &'static str, why: &'static str },
}

/// What a play was of, by catalogue id.
#[derive(Debug, Clone, PartialEq)]
pub enum Played {
    Movie(ExternalIds),
    /// The *show's* ids, its season and episode number.
    Episode {
        show: ExternalIds,
        season: u32,
        episode: Option<u32>,
    },
}

/// One dated play.
#[derive(Debug, Clone, PartialEq)]
pub struct SourcePlay {
    pub epoch: u64,
    pub viewer: Viewer,
    pub played: Played,
    /// Share of the runtime watched, 0.0-1.0.
    pub fraction: f32,
}

/// One cycle's read of one source.
#[derive(Debug, Clone, Default)]
pub struct SourceRead {
    pub plays: Vec<SourcePlay>,
    /// Epoch of every history row read, joinable or not: the span the record
    /// demonstrably covers.
    pub epochs: Vec<u64>,
    /// Accounts (Tracearr users, or the one Trakt account) read to the end.
    pub accounts: usize,
    /// Every account was listed and each one's history paged to its end.
    pub complete: bool,
    /// Why the read is incomplete, one line per failure (no URL, no token).
    pub problems: Vec<String>,
    /// Tracearr user id → username, for `ignore_viewers` ([`crate::viewers`]).
    pub usernames: std::collections::HashMap<String, String>,
}

/// One source's part of the status page.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SourceStatus {
    pub source: String,
    pub kind: SourceKind,
    pub complete: bool,
    pub accounts: usize,
    pub plays: usize,
    /// Library items its plays joined by catalogue id.
    pub joined: usize,
    /// Items it claims nobody played (Tracearr only).
    pub never_played: usize,
    pub problems: Vec<String>,
}

/// Every source's status, published as `status.watch_sources`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WatchSourcesStatus {
    pub sources: Vec<SourceStatus>,
}

/// The state-directory file holding the last cycle's [`WatchSourcesStatus`].
pub const STATUS_FILE: &str = "watch-sources.json";

impl WatchSourcesStatus {
    /// The last cycle's status; `None` when no source ran (or the file is
    /// unreadable — the status page then shows no block, nothing decides on it).
    pub fn read(state_dir: &std::path::Path) -> Option<Self> {
        let bytes = std::fs::read(state_dir.join(STATUS_FILE)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

/// `YYYY-MM-DDTHH:MM:SS[.fff][Z|±HH:MM]` as epoch seconds (both sources
/// write ISO 8601 UTC).
pub(crate) fn parse_utc(text: &str) -> Option<u64> {
    crate::jellyfin::parse_utc(text)
}
