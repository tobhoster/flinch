//! Seerr (Overseerr/Jellyseerr): who asked for what, and who wants what.
//!
//! Wire shape, from the Overseerr sources (`develop`; Jellyseerr keeps it):
//! `GET /api/v1/request?take=&skip=&filter=all` and `GET /api/v1/user?take=&skip=`
//! answer `{pageInfo: {pages, pageSize, results, page}, results: [...]}`, where
//! `pageInfo.results` is the total count. A `MediaRequest` carries its
//! `status` (`MediaRequestStatus`: 1 pending, 2 approved, 3 declined,
//! 4 failed, 5 completed), its `media` (`mediaType` `movie` or `tv`, `tmdbId`,
//! `tvdbId`), the requested `seasons` and the `requestedBy` user, whose
//! `displayName` is computed when the user loads. `GET
//! /api/v1/user/{id}/watchlist?page=` answers `{page, totalPages,
//! totalResults, results: [{mediaType, tmdbId, ...}]}` from the user's Plex
//! watchlist.

use super::{rows, MediaRef, Request, Watchlisted};
use serde::Deserialize;

/// Rows asked for per request and user page.
pub const TAKE: usize = 100;

/// `MediaRequestStatus.DECLINED`.
const DECLINED: u8 = 3;

/// The request page `skip` rows in, of every status.
pub fn requests_path(skip: usize) -> String {
    format!("/api/v1/request?take={TAKE}&skip={skip}&filter=all")
}

/// The user page `skip` rows in.
pub fn users_path(skip: usize) -> String {
    format!("/api/v1/user?take={TAKE}&skip={skip}")
}

/// One page of a user's watchlist, from 1.
pub fn watchlist_path(member: u64, page: u64) -> String {
    format!("/api/v1/user/{member}/watchlist?page={page}")
}

/// One page of requests or users. Rows stay raw so one malformed row is
/// skipped, not the page.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    #[serde(default)]
    pub page_info: PageInfo,
    #[serde(default)]
    pub results: Vec<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
pub struct PageInfo {
    /// Rows in all pages together.
    #[serde(default)]
    pub results: usize,
}

impl Page {
    /// Whether the page read `skip` rows in is the last one.
    pub fn is_last(&self, skip: usize) -> bool {
        self.results.len() < TAKE || skip + TAKE >= self.page_info.results
    }
}

/// One page of a watchlist.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchlistPage {
    #[serde(default)]
    pub total_pages: u64,
    #[serde(default)]
    pub results: Vec<serde_json::Value>,
}

/// A Seerr user, by the name FLINCH keys weights on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub member: u64,
    pub name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserRecord {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    plex_username: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

impl UserRecord {
    /// `displayName`, or what Seerr itself falls back to when it is unset.
    fn name(self) -> String {
        [self.display_name, self.username, self.plex_username, self.email]
            .into_iter()
            .flatten()
            .find(|name| !name.is_empty())
            .unwrap_or_default()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestRecord {
    status: u8,
    media: MediaRecord,
    #[serde(default)]
    seasons: Vec<SeasonRecord>,
    requested_by: UserRecord,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaRecord {
    media_type: String,
    #[serde(default)]
    tmdb_id: Option<u64>,
    #[serde(default)]
    tvdb_id: Option<u64>,
}

impl MediaRecord {
    /// The title's identity; `None` for an unknown type or no usable id.
    fn media_ref(&self) -> Option<MediaRef> {
        let (tmdb, tvdb) = (self.tmdb_id.filter(|id| *id > 0), self.tvdb_id.filter(|id| *id > 0));
        match self.media_type.as_str() {
            "movie" => Some(MediaRef::Movie { tmdb: tmdb? }),
            "tv" if tmdb.is_some() || tvdb.is_some() => Some(MediaRef::Show { tvdb, tmdb }),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SeasonRecord {
    season_number: u32,
}

/// Every request not declined.
pub fn parse_requests(records: Vec<serde_json::Value>) -> Vec<Request> {
    rows::<RequestRecord>(records)
        .filter(|record| record.status != DECLINED)
        .filter_map(|record| {
            Some(Request {
                media: record.media.media_ref()?,
                seasons: record.seasons.iter().map(|season| season.season_number).collect(),
                requester: record.requested_by.name(),
            })
        })
        .collect()
}

/// The users with a usable id.
pub fn parse_users(records: Vec<serde_json::Value>) -> Vec<User> {
    rows::<UserRecord>(records).filter(|user| user.id > 0).map(|user| User { member: user.id, name: user.name() }).collect()
}

/// The titles on one page of `user`'s watchlist.
pub fn parse_watchlist(user: &str, records: Vec<serde_json::Value>) -> Vec<Watchlisted> {
    rows::<MediaRecord>(records).filter_map(|item| Some(Watchlisted { media: item.media_ref()?, user: user.to_owned() })).collect()
}
