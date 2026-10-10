//! The live Tracearr reader: every user, then each user's whole history.
//!
//! API shapes, from Tracearr's source (public API v2,
//! <https://github.com/connorgallopo/Tracearr>, `apps/server/src/routes/publicV2/`;
//! reference at <https://docs.tracearr.com/api>):
//! - auth: `Authorization: Bearer trr_pub_…` (`plugins/auth.ts`,
//!   `authenticatePublicApi`);
//! - `GET /api/v2/public/users?include_removed=true&pageSize=…&cursor=…` →
//!   `{"data":[{"id":…,"username":…}],"meta":{"nextCursor":…|null}}`
//!   (`users.ts`): one row per Tracearr identity, removed ones included so
//!   their plays still count;
//! - `GET /api/v2/public/history?user_id=…&pageSize=…&cursor=…` → the same
//!   envelope of plays (resume chains) with `started_at`, `watched`,
//!   `percent_complete`, `media_type`, `season_number`, `episode_number`,
//!   `imdb_id`/`tmdb_id`/`tvdb_id` and `show_media_id` (`history.ts`,
//!   `shared.ts` `mapHistoryRow`); `nextCursor` is null on the last page;
//! - `GET /api/v2/public/media/{id}` → the canonical item's
//!   `imdb_id`/`tmdb_id`/`tvdb_id` (`media.ts`). An episode row carries the
//!   *episode's* ids, so its show's ids come from here, once per show.
//!
//! `pageSize` is capped at 100 by the server (`cursorPaginationSchema`).

use super::{parse_utc, Played, SourceError, SourcePlay, SourceRead};
use crate::fit::plays::Viewer;
use crate::ids::ExternalIds;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

const KIND: &str = "tracearr";
const TIMEOUT: Duration = Duration::from_secs(30);
const PAGE: &str = "100";
/// A runaway cursor stops here: far beyond any household's history.
const MAX_PAGES: usize = 10_000;

pub struct TracearrClient {
    http: reqwest::Client,
    base: String,
    key: String,
}

#[derive(Deserialize)]
struct Page<T> {
    #[serde(default = "Vec::new")]
    data: Vec<T>,
    meta: Meta,
}

#[derive(Deserialize)]
struct Meta {
    #[serde(rename = "nextCursor")]
    next_cursor: Option<String>,
}

#[derive(Deserialize)]
struct User {
    id: String,
    #[serde(default)]
    username: Option<String>,
}

/// One play, only the fields the join reads.
#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct Row {
    pub(super) started_at: Option<String>,
    pub(super) watched: bool,
    pub(super) percent_complete: Option<f64>,
    pub(super) media_type: String,
    pub(super) season_number: Option<u32>,
    pub(super) episode_number: Option<u32>,
    pub(super) imdb_id: Option<String>,
    pub(super) tmdb_id: Option<u32>,
    pub(super) show_media_id: Option<String>,
}

#[derive(Deserialize)]
struct Media {
    #[serde(default)]
    imdb_id: Option<String>,
    #[serde(default)]
    tmdb_id: Option<u32>,
    #[serde(default)]
    tvdb_id: Option<u32>,
}

impl TracearrClient {
    /// Redirects are not followed, so the key never reaches another host.
    pub fn new(base: &str, key: &str) -> Result<Self, SourceError> {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| SourceError::Transport { source_kind: KIND, endpoint: "client setup", error })?;
        Ok(Self { http, base: base.trim().trim_end_matches('/').to_string(), key: key.to_string() })
    }

    async fn json<T: serde::de::DeserializeOwned>(
        &self,
        endpoint: &'static str,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, SourceError> {
        let response = self
            .http
            .get(format!("{}/api/v2/public{path}", self.base))
            .query(query)
            .bearer_auth(&self.key)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|error| SourceError::Transport { source_kind: KIND, endpoint, error: error.without_url() })?;
        let status = response.status();
        if !status.is_success() {
            return Err(SourceError::Status { source_kind: KIND, endpoint, status: status.as_u16() });
        }
        let body = crate::body::read(response).await.map_err(|error| SourceError::Body { source_kind: KIND, endpoint, error })?;
        serde_json::from_slice(&body).map_err(|error| SourceError::Parse { source_kind: KIND, endpoint, error })
    }

    /// Every page of a cursor listing. A cursor seen twice, or a listing
    /// past [`MAX_PAGES`], is an error: a loop must never pass as the end.
    async fn pages<T: serde::de::DeserializeOwned>(
        &self,
        endpoint: &'static str,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<Vec<T>, SourceError> {
        let mut rows = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen: HashSet<String> = HashSet::new();
        for _ in 0..MAX_PAGES {
            let mut full: Vec<(&str, &str)> = query.to_vec();
            full.push(("pageSize", PAGE));
            if let Some(cursor) = &cursor {
                full.push(("cursor", cursor));
            }
            let page: Page<T> = self.json(endpoint, path, &full).await?;
            rows.extend(page.data);
            match page.meta.next_cursor.filter(|next| !next.is_empty()) {
                None => return Ok(rows),
                Some(next) if !seen.insert(next.clone()) => {
                    return Err(SourceError::Incomplete { source_kind: KIND, endpoint, why: "the cursor repeated" })
                }
                Some(next) => cursor = Some(next),
            }
        }
        Err(SourceError::Incomplete { source_kind: KIND, endpoint, why: "too many pages" })
    }

    /// Every user and each user's history to its end; a show whose ids
    /// cannot be read leaves the read incomplete, since its plays join nothing.
    pub async fn read(&self) -> Result<SourceRead, SourceError> {
        let users: Vec<User> = self.pages("users", "/users", &[("include_removed", "true")]).await?;
        let mut read = SourceRead { complete: !users.is_empty(), ..SourceRead::default() };
        if users.is_empty() {
            read.problems.push("Tracearr listed no users".to_string());
        }
        let mut shows: HashMap<String, Option<ExternalIds>> = HashMap::new();
        for user in users {
            if let Some(name) = user.username.as_deref().filter(|name| !name.trim().is_empty()) {
                read.usernames.insert(user.id.clone(), name.to_string());
            }
            let rows: Vec<Row> = match self.pages("history", "/history", &[("user_id", &user.id)]).await {
                Ok(rows) => rows,
                Err(error) => {
                    read.complete = false;
                    read.problems.push(format!("user {}: {error}", user.username.as_deref().unwrap_or(&user.id)));
                    continue;
                }
            };
            read.accounts += 1;
            for row in rows {
                let Some(epoch) = row.started_at.as_deref().and_then(parse_utc) else { continue };
                read.epochs.push(epoch);
                let show = match (row.media_type.as_str(), row.show_media_id.as_deref()) {
                    ("episode", Some(id)) => {
                        if !shows.contains_key(id) {
                            let ids = match self.json::<Media>("media", &format!("/media/{id}"), &[]).await {
                                Ok(media) => Some(ExternalIds { tmdb: media.tmdb_id, tvdb: media.tvdb_id, imdb: media.imdb_id }),
                                Err(error) => {
                                    read.complete = false;
                                    read.problems.push(format!("show {id}: {error}"));
                                    None
                                }
                            };
                            shows.insert(id.to_string(), ids);
                        }
                        shows.get(id).cloned().flatten()
                    }
                    _ => None,
                };
                if let Some(play) = play(&row, epoch, &user.id, show) {
                    read.plays.push(play);
                }
            }
        }
        Ok(read)
    }
}

/// A history row as a play, when it is a movie or an episode whose show is
/// known by catalogue id.
pub(super) fn play(row: &Row, epoch: u64, user: &str, show: Option<ExternalIds>) -> Option<SourcePlay> {
    let played = match row.media_type.as_str() {
        "movie" => Played::Movie(ExternalIds { tmdb: row.tmdb_id, tvdb: None, imdb: row.imdb_id.clone() }),
        "episode" => Played::Episode { show: show?, season: row.season_number?, episode: row.episode_number },
        _ => return None,
    };
    let fraction = if row.watched { 1.0 } else { row.percent_complete.map_or(0.0, |percent| (percent / 100.0).clamp(0.0, 1.0) as f32) };
    Some(SourcePlay { epoch, viewer: Viewer::TracearrUser(user.to_string()), played, fraction })
}
