//! The live Trakt reader: one account's whole watched history.
//!
//! API shapes, from the Trakt API docs (<https://trakt.docs.apiary.io/>,
//! blueprint at <https://jsapi.apiary.io/apis/trakt.apib>):
//! - required headers: `Content-Type: application/json`, a `User-Agent`,
//!   `trakt-api-key: <client_id>`, `trakt-api-version: 2`, and for this
//!   OAuth-required call `Authorization: Bearer <access_token>` ("Required
//!   Headers");
//! - `GET /sync/history?page=…&limit=…` ("Get watched history"): movies and
//!   episodes, most recent first, each `{"watched_at", "type": "movie" |
//!   "episode", "movie": {"ids": {"tmdb", "imdb"}}}` or `{"episode":
//!   {"season", "number"}, "show": {"ids": {"tvdb", "tmdb", "imdb"}}}`;
//! - pagination headers `X-Pagination-Page-Count` and `X-Pagination-Item-Count`
//!   ("Pagination").
//!
//! The access token comes from Trakt's OAuth device flow
//! (`POST /oauth/device/code`, then `POST /oauth/device/token`), run once by
//! the operator; FLINCH only reads it from the environment.

use super::{parse_utc, Played, SourceError, SourcePlay, SourceRead};
use crate::fit::plays::Viewer;
use crate::ids::ExternalIds;
use serde::Deserialize;
use std::time::Duration;

const KIND: &str = "trakt";
const ENDPOINT: &str = "history";
const TIMEOUT: Duration = Duration::from_secs(30);
const LIMIT: u32 = 100;
/// A runaway page count stops here: 1,000,000 plays.
const MAX_PAGES: u32 = 10_000;

pub struct TraktClient {
    http: reqwest::Client,
    base: String,
    token: String,
    client_id: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct Item {
    pub(super) watched_at: Option<String>,
    #[serde(rename = "type")]
    pub(super) kind: String,
    pub(super) movie: Option<Media>,
    pub(super) show: Option<Media>,
    pub(super) episode: Option<Episode>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct Media {
    pub(super) ids: Ids,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct Ids {
    pub(super) tmdb: Option<u32>,
    pub(super) tvdb: Option<u32>,
    pub(super) imdb: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct Episode {
    pub(super) season: Option<u32>,
    pub(super) number: Option<u32>,
}

impl From<&Ids> for ExternalIds {
    fn from(ids: &Ids) -> Self {
        ExternalIds { tmdb: ids.tmdb, tvdb: ids.tvdb, imdb: ids.imdb.clone() }
    }
}

fn header(response: &reqwest::Response, name: &str) -> Option<u32> {
    response.headers().get(name)?.to_str().ok()?.trim().parse().ok()
}

impl TraktClient {
    /// Redirects are not followed, so the token never reaches another host.
    pub fn new(base: &str, token: &str, client_id: &str) -> Result<Self, SourceError> {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("flinch/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| SourceError::Transport { source_kind: KIND, endpoint: "client setup", error })?;
        Ok(Self { http, base: base.trim().trim_end_matches('/').to_string(), token: token.to_string(), client_id: client_id.to_string() })
    }

    /// One page, with the page and item counts Trakt reports.
    async fn page(&self, page: u32) -> Result<(Vec<Item>, u32, u32), SourceError> {
        let response = self
            .http
            .get(format!("{}/sync/history", self.base))
            .query(&[("page", page), ("limit", LIMIT)])
            .bearer_auth(&self.token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header("trakt-api-key", &self.client_id)
            .header("trakt-api-version", "2")
            .send()
            .await
            .map_err(|error| SourceError::Transport { source_kind: KIND, endpoint: ENDPOINT, error: error.without_url() })?;
        let status = response.status();
        if !status.is_success() {
            return Err(SourceError::Status { source_kind: KIND, endpoint: ENDPOINT, status: status.as_u16() });
        }
        let missing = SourceError::Incomplete { source_kind: KIND, endpoint: ENDPOINT, why: "no pagination headers" };
        let (Some(pages), Some(items)) = (header(&response, "x-pagination-page-count"), header(&response, "x-pagination-item-count"))
        else {
            return Err(missing);
        };
        let body = crate::body::read(response).await.map_err(|error| SourceError::Body { source_kind: KIND, endpoint: ENDPOINT, error })?;
        let rows = serde_json::from_slice(&body).map_err(|error| SourceError::Parse { source_kind: KIND, endpoint: ENDPOINT, error })?;
        Ok((rows, pages, items))
    }

    /// Every page to Trakt's own page count. Ending with fewer rows than its
    /// item count is an error, so a truncated read never passes as complete.
    /// `viewer` names the account in the plays.
    pub async fn read(&self, viewer: &str) -> Result<SourceRead, SourceError> {
        let mut items = Vec::new();
        let (mut page, mut pages, mut total) = (1u32, 1u32, 0u32);
        while page <= pages {
            if page > MAX_PAGES {
                return Err(SourceError::Incomplete { source_kind: KIND, endpoint: ENDPOINT, why: "too many pages" });
            }
            let (rows, page_count, item_count) = self.page(page).await?;
            (pages, total) = (page_count, item_count);
            let empty = rows.is_empty();
            items.extend(rows);
            if empty {
                break;
            }
            page += 1;
        }
        if items.len() != total as usize {
            return Err(SourceError::Incomplete {
                source_kind: KIND,
                endpoint: ENDPOINT,
                why: "pages disagree with X-Pagination-Item-Count",
            });
        }
        let mut read = SourceRead { accounts: 1, complete: true, ..SourceRead::default() };
        for item in &items {
            let Some(epoch) = item.watched_at.as_deref().and_then(parse_utc) else { continue };
            read.epochs.push(epoch);
            read.plays.extend(play(item, epoch, viewer));
        }
        Ok(read)
    }
}

/// A history item as a play. Trakt history records a watch, so it is a
/// whole play.
pub(super) fn play(item: &Item, epoch: u64, viewer: &str) -> Option<SourcePlay> {
    let played = match item.kind.as_str() {
        "movie" => Played::Movie(ExternalIds::from(&item.movie.as_ref()?.ids)),
        "episode" => {
            let episode = item.episode.as_ref()?;
            Played::Episode { show: ExternalIds::from(&item.show.as_ref()?.ids), season: episode.season?, episode: episode.number }
        }
        _ => return None,
    };
    Some(SourcePlay { epoch, viewer: Viewer::TraktUser(viewer.to_string()), played, fraction: 1.0 })
}
