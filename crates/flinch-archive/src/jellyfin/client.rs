//! The live Jellyfin/Emby reader: every user, then every user's items.
//!
//! API shapes:
//! - Jellyfin `GET /Users` and `GET /Items?userId=…`:
//!   <https://api.jellyfin.org/#tag/User/operation/GetUsers>,
//!   <https://api.jellyfin.org/#tag/Items/operation/GetItems>; the token goes in
//!   `Authorization: MediaBrowser Token="…"`
//!   (<https://github.com/jellyfin/jellyfin/blob/master/Jellyfin.Api/Auth/CustomAuthenticationHandler.cs>,
//!   parsed by `AuthorizationContext`).
//! - Emby `GET /Users` and `GET /Users/{Id}/Items`:
//!   <https://dev.emby.media/reference/RestAPI/UserService/getUsers.html>,
//!   <https://dev.emby.media/reference/RestAPI/ItemsService/getUsersByUseridItems.html>;
//!   the key goes in `X-Emby-Token`
//!   (<https://dev.emby.media/doc/restapi/API-Key-Authentication.html>).
//!
//! Both bind query names case-insensitively; both answer
//! `{"Items": [...], "TotalRecordCount": n}`.

use super::{JellyfinError, JellyfinItem, JellyfinRead, JellyfinUser, ServerKind, UserItems};
use reqwest::RequestBuilder;
use serde::Deserialize;
use std::collections::HashSet;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(30);
const PAGE: usize = 500;
const USERS: &str = "users";
const ITEMS: &str = "items";

pub struct JellyfinClient {
    http: reqwest::Client,
    base: String,
    key: String,
    kind: ServerKind,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Page {
    #[serde(default)]
    items: Vec<JellyfinItem>,
    total_record_count: Option<usize>,
}

impl JellyfinClient {
    /// Redirects are not followed, so the key never reaches another host: a
    /// 3xx reads as an HTTP error.
    pub fn new(base: &str, key: &str, kind: ServerKind) -> Result<Self, JellyfinError> {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|source| JellyfinError::Transport { endpoint: "client setup", source })?;
        Ok(Self { http, base: base.trim().trim_end_matches('/').to_string(), key: key.to_string(), kind })
    }

    /// Which server this is: the two list a user's items at different paths.
    pub fn kind(&self) -> ServerKind {
        self.kind
    }

    fn get(&self, path: &str) -> RequestBuilder {
        self.request(reqwest::Method::GET, path)
    }

    /// Any request, with the key in the server's own header and never in the URL.
    pub(super) fn request(&self, method: reqwest::Method, path: &str) -> RequestBuilder {
        let request = self.http.request(method, format!("{}{path}", self.base)).header(reqwest::header::ACCEPT, "application/json");
        match self.kind {
            ServerKind::Jellyfin => request.header(reqwest::header::AUTHORIZATION, format!("MediaBrowser Token=\"{}\"", self.key)),
            ServerKind::Emby => request.header("X-Emby-Token", &self.key),
        }
    }

    /// A user's item listing: Jellyfin's `/Items?userId=`, Emby's `/Users/{id}/Items`.
    pub(super) fn user_listing(&self, user_id: &str) -> RequestBuilder {
        match self.kind {
            ServerKind::Jellyfin => self.get("/Items").query(&[("userId", user_id)]),
            ServerKind::Emby => self.get(&format!("/Users/{user_id}/Items")),
        }
    }

    pub(super) async fn json<T: serde::de::DeserializeOwned>(endpoint: &'static str, request: RequestBuilder) -> Result<T, JellyfinError> {
        let response = request.send().await.map_err(|source| JellyfinError::Transport { endpoint, source: source.without_url() })?;
        let status = response.status();
        if !status.is_success() {
            return Err(JellyfinError::Status { endpoint, status: status.as_u16() });
        }
        let body = crate::body::read(response).await.map_err(|source| JellyfinError::Body { endpoint, source })?;
        serde_json::from_slice(&body).map_err(|source| JellyfinError::Parse { endpoint, source })
    }

    pub async fn users(&self) -> Result<Vec<JellyfinUser>, JellyfinError> {
        Self::json(USERS, self.get("/Users")).await
    }

    /// One user's movies, series and episodes with their state, paged to the
    /// server's own total. A listing that ends short of that total is an
    /// error, so a truncated read can never pass as complete.
    pub async fn user_items(&self, user_id: &str) -> Result<Vec<JellyfinItem>, JellyfinError> {
        let mut items: Vec<JellyfinItem> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut start = 0usize;
        loop {
            let request = self.user_listing(user_id).query(&[
                ("Recursive", "true"),
                ("IncludeItemTypes", "Movie,Series,Episode"),
                ("Fields", "ProviderIds"),
                ("EnableUserData", "true"),
                ("EnableImages", "false"),
                ("EnableTotalRecordCount", "true"),
                // A stable order, so pages neither overlap nor skip.
                ("SortBy", "DateCreated,SortName"),
                ("SortOrder", "Ascending"),
                ("StartIndex", &start.to_string()),
                ("Limit", &PAGE.to_string()),
            ]);
            let page: Page = Self::json(ITEMS, request).await?;
            let total = page.total_record_count.ok_or(JellyfinError::Incomplete("no TotalRecordCount"))?;
            let got = page.items.len();
            for item in page.items {
                if seen.insert(item.id.clone()) {
                    items.push(item);
                }
            }
            start += got;
            if start >= total {
                if items.len() != total {
                    return Err(JellyfinError::Incomplete("pages disagree with TotalRecordCount"));
                }
                return Ok(items);
            }
            if got == 0 {
                return Err(JellyfinError::Incomplete("listing ended before TotalRecordCount"));
            }
        }
    }

    /// Every user and every user's items. A failed user marks the read
    /// incomplete and is skipped; a failed user listing fails the whole read.
    pub async fn read(&self) -> Result<JellyfinRead, JellyfinError> {
        let users = self.users().await?;
        let mut read = JellyfinRead { complete: !users.is_empty(), ..JellyfinRead::default() };
        if users.is_empty() {
            read.problems.push("the server listed no users".to_string());
        }
        for user in users {
            match self.user_items(&user.id).await {
                Ok(items) => read.users.push(UserItems { user_id: user.id, name: user.name, items }),
                Err(error) => {
                    read.complete = false;
                    read.problems.push(format!("user {}: {error}", user.name));
                }
            }
        }
        Ok(read)
    }
}
