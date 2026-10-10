//! The last look before a delete: Plex's own record of the item, read again
//! the moment before the *arr is told to delete it. The cycle's evidence was
//! read minutes earlier; a play that started since must still win. Anything
//! short of a clean read keeps the item.
//!
//! Endpoint and fields from python-plexapi, the reference client for Plex's
//! unofficial API (https://github.com/pkkid/python-plexapi):
//! `PlexServer.fetchItem` reads `/library/metadata/{ratingKey}`; video.py names
//! `lastViewedAt` (epoch seconds) on movies and seasons, `viewOffset`
//! (milliseconds into a movie) and `viewCount`.

use super::http::{as_u64, ExecutorError};

const METADATA: &str = "plex metadata";

/// What Plex says about an item right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlexWatch {
    pub last_viewed_at: Option<u64>,
    /// Milliseconds into the item when someone stopped partway; 0 otherwise.
    pub view_offset_ms: u64,
}

/// Whether a delete may go ahead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recheck {
    Clear,
    /// Played after the decision's evidence was read, at this time.
    PlayedSince(u64),
    /// Someone is partway through it right now.
    InProgress,
    /// Plex no longer lists the item: nothing proves it unplayed.
    Gone,
}

impl std::fmt::Display for Recheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Clear => f.write_str("no play since the decision"),
            Self::PlayedSince(at) => write!(f, "played since the decision (at {at})"),
            Self::InProgress => f.write_str("someone is partway through it"),
            Self::Gone => f.write_str("Plex no longer lists it"),
        }
    }
}

/// `baseline` is when the decision's evidence ends: the cycle's read for a
/// finished item, the announcement for one on the Leaving Soon shelf.
pub fn judge(baseline: u64, watch: Option<PlexWatch>) -> Recheck {
    let Some(watch) = watch else { return Recheck::Gone };
    if watch.view_offset_ms > 0 {
        return Recheck::InProgress;
    }
    match watch.last_viewed_at {
        Some(at) if at > baseline => Recheck::PlayedSince(at),
        _ => Recheck::Clear,
    }
}

/// Read one item; `None` when Plex answers 404. The token travels in a header
/// and errors carry no URL.
pub async fn read(http: &reqwest::Client, base: &str, token: &str, rating_key: &str) -> Result<Option<PlexWatch>, ExecutorError> {
    let url = format!("{}/library/metadata/{rating_key}", base.trim_end_matches('/'));
    let response = http
        .get(url)
        .header("X-Plex-Token", token)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|source| ExecutorError::Transport { endpoint: METADATA, source: source.without_url() })?;
    let status = response.status().as_u16();
    if status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        return Err(ExecutorError::Http { endpoint: METADATA, status, detail: String::new() });
    }
    let body = crate::body::read_text(response).await.map_err(|source| ExecutorError::Body { endpoint: METADATA, source })?;
    let value: serde_json::Value =
        serde_json::from_str(&body).map_err(|error| ExecutorError::Parse { endpoint: METADATA, detail: error.to_string() })?;
    let Some(item) = value["MediaContainer"]["Metadata"].as_array().and_then(|rows| rows.first()) else {
        return Ok(None);
    };
    Ok(Some(PlexWatch { last_viewed_at: as_u64(&item["lastViewedAt"]), view_offset_ms: as_u64(&item["viewOffset"]).unwrap_or(0) }))
}
