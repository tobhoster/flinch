//! Seerr (Overseerr / Jellyseerr) cleanup after a native delete: clear what
//! marks the title as available or requested, so the household can request it
//! again at once instead of waiting for Seerr's availability sync.
//!
//! Endpoints from Overseerr's API document
//! (https://github.com/sct/overseerr/blob/develop/overseerr-api.yml; Jellyseerr
//! keeps the same routes): `GET /api/v1/movie/{tmdbId}` and
//! `GET /api/v1/tv/{tmdbId}` (their `mediaInfo`: `id`, `requests[].id`,
//! `requests[].seasons[].seasonNumber`), `DELETE /api/v1/media/{mediaId}`
//! (Seerr's own media id, never the TMDB id: Seerr answers 204 for an id it
//! does not hold) and `DELETE /api/v1/request/{requestId}`. A season follows
//! Maintainerr's rule (apps/server/src/modules/api/seerr-api/seerr-api.service.ts,
//! `removeSeasonRequest`): requests for that season alone are deleted, a
//! request that also covers other seasons is kept, and the media record is
//! cleared only when nothing at all is requested, since deleting it cascades
//! into every season's requests.

use super::http::{as_u64, Api, ExecutorError};
use reqwest::Method;
use serde_json::Value;

const MOVIE: &str = "seerr movie";
const TV: &str = "seerr tv";
const MEDIA: &str = "seerr delete media";
const REQUEST: &str = "seerr delete request";

/// What the cleanup did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cleared {
    /// Seerr does not track the title: nothing to clear.
    NotTracked,
    /// The media record (and with it its requests) was cleared.
    Media,
    /// This many requests for the season alone were deleted.
    Requests(usize),
    /// Every request for the season also covers other seasons: kept.
    Shared(usize),
    Simulated,
}

impl std::fmt::Display for Cleared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotTracked => f.write_str("not in Seerr"),
            Self::Media => f.write_str("Seerr record cleared"),
            Self::Requests(n) => write!(f, "{n} Seerr request(s) deleted"),
            Self::Shared(n) => write!(f, "{n} Seerr request(s) kept: they cover other seasons too"),
            Self::Simulated => f.write_str("Seerr cleanup shown, not sent"),
        }
    }
}

pub struct Seerr<'a> {
    api: Api<'a>,
}

fn media_id(info: &Value) -> Option<u64> {
    as_u64(&info["id"])
}

fn request_ids(info: &Value) -> Vec<(u64, Vec<u64>)> {
    info["requests"]
        .as_array()
        .map(|requests| {
            requests
                .iter()
                .filter_map(|request| {
                    let seasons =
                        request["seasons"].as_array().map(|rows| rows.iter().filter_map(|row| as_u64(&row["seasonNumber"])).collect());
                    Some((as_u64(&request["id"])?, seasons.unwrap_or_default()))
                })
                .collect()
        })
        .unwrap_or_default()
}

impl<'a> Seerr<'a> {
    pub fn new(http: &'a reqwest::Client, base: &'a str, key: &'a str, dry_run: bool) -> Self {
        Self { api: Api::new(http, base, key, "seerr", dry_run) }
    }

    async fn info(&self, endpoint: &'static str, path: &str) -> Result<Option<Value>, ExecutorError> {
        Ok(self.api.find(endpoint, path).await?.map(|title| title["mediaInfo"].clone()).filter(|info| media_id(info).is_some()))
    }

    /// Clear a deleted movie's record, then read it back gone.
    pub async fn clear_movie(&self, tmdb_id: u32) -> Result<Cleared, ExecutorError> {
        let path = format!("/api/v1/movie/{tmdb_id}");
        let Some(id) = self.info(MOVIE, &path).await?.as_ref().and_then(media_id) else { return Ok(Cleared::NotTracked) };
        self.api.write(MEDIA, Method::DELETE, &format!("/api/v1/media/{id}"), &[], None).await?;
        if self.api.dry_run {
            return Ok(Cleared::Simulated);
        }
        match self.info(MOVIE, &path).await? {
            Some(info) if media_id(&info) == Some(id) => {
                Err(ExecutorError::NotApplied { endpoint: MEDIA, detail: format!("media {id} still listed") })
            }
            _ => Ok(Cleared::Media),
        }
    }

    /// Clear what keeps a deleted season from being requested again.
    pub async fn clear_season(&self, tmdb_id: u32, season: u32) -> Result<Cleared, ExecutorError> {
        let path = format!("/api/v1/tv/{tmdb_id}");
        let Some(info) = self.info(TV, &path).await? else { return Ok(Cleared::NotTracked) };
        let season = u64::from(season);
        let requests = request_ids(&info);
        let covering: Vec<&(u64, Vec<u64>)> = requests.iter().filter(|(_, seasons)| seasons.contains(&season)).collect();
        let own: Vec<u64> = covering.iter().filter(|(_, seasons)| seasons.iter().all(|n| *n == season)).map(|(id, _)| *id).collect();
        let shared = covering.len() - own.len();
        if requests.is_empty() {
            let Some(id) = media_id(&info) else { return Ok(Cleared::NotTracked) };
            self.api.write(MEDIA, Method::DELETE, &format!("/api/v1/media/{id}"), &[], None).await?;
            if self.api.dry_run {
                return Ok(Cleared::Simulated);
            }
            return match self.info(TV, &path).await? {
                Some(after) if media_id(&after) == Some(id) => {
                    Err(ExecutorError::NotApplied { endpoint: MEDIA, detail: format!("media {id} still listed") })
                }
                _ => Ok(Cleared::Media),
            };
        }
        if own.is_empty() {
            return Ok(if shared > 0 { Cleared::Shared(shared) } else { Cleared::Requests(0) });
        }
        for id in &own {
            self.api.write(REQUEST, Method::DELETE, &format!("/api/v1/request/{id}"), &[], None).await?;
        }
        if self.api.dry_run {
            return Ok(Cleared::Simulated);
        }
        let left = self.info(TV, &path).await?.map(|after| request_ids(&after)).unwrap_or_default();
        if let Some((id, _)) = left.iter().find(|(id, _)| own.contains(id)) {
            return Err(ExecutorError::NotApplied { endpoint: REQUEST, detail: format!("request {id} still listed") });
        }
        Ok(Cleared::Requests(own.len()))
    }
}
