//! qBittorrent's Web API v2: a cookie session from `/api/v2/auth/login`.
//!
//! Shapes from the official reference and source:
//! - https://github.com/qbittorrent/qBittorrent/wiki/WebUI-API-(qBittorrent-5.0):
//!   login takes form `username`/`password` and answers with a session cookie,
//!   with `Referer` set to the Web UI's own host; `GET /api/v2/torrents/info`
//!   (optional `hashes`), `GET /api/v2/torrents/files?hash=` (`name` relative
//!   to `save_path`), `POST /api/v2/torrents/delete` (`hashes`, `deleteFiles`).
//! - src/webui/api/serialize/serialize_torrent.cpp (release-5.0.0 and master):
//!   `max_ratio`/`max_seeding_time` are the *effective* limits (the global ones
//!   when the torrent uses them; negative when none), `max_seeding_time` in
//!   minutes, `seeding_time` in seconds, `ratio` -1 when at or above 9999.
//! - src/webui/webapplication.cpp: the cookie is `SID` up to 5.1 and
//!   `QBT_SID_<port>` from 5.2, so whatever cookie login sets is sent back.
//!
//! Older servers answer a bad login 200 `Fails.`, newer ones 401; an expired
//! session answers 403 and is logged into again once.

use super::{send, ClientKind, Credentials, Torrent, TorrentError, UNBOUNDED_RATIO};
use serde::Deserialize;
use std::sync::Mutex;

const KIND: ClientKind = ClientKind::Qbittorrent;

pub(super) struct Qbittorrent {
    http: reqwest::Client,
    base: String,
    credentials: Credentials,
    /// `name=value` pairs login set, sent back as one `Cookie` header.
    cookie: Mutex<Option<String>>,
}

#[derive(Deserialize)]
struct Info {
    hash: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    ratio: f64,
    #[serde(default)]
    seeding_time: i64,
    #[serde(default = "none")]
    max_ratio: f64,
    #[serde(default = "none_i")]
    max_seeding_time: i64,
    #[serde(default)]
    progress: f64,
    #[serde(default)]
    save_path: String,
    #[serde(default)]
    content_path: String,
}

fn none() -> f64 {
    -1.0
}

fn none_i() -> i64 {
    -1
}

#[derive(Deserialize)]
struct File {
    name: String,
}

impl Info {
    fn torrent(self) -> Torrent {
        let ratio = if self.ratio < 0.0 { UNBOUNDED_RATIO } else { self.ratio };
        let seeding_secs = u64::try_from(self.seeding_time).unwrap_or(0);
        let limit_reached = (self.max_ratio >= 0.0 && ratio >= self.max_ratio)
            || u64::try_from(self.max_seeding_time).is_ok_and(|minutes| seeding_secs >= minutes.saturating_mul(60));
        let content_path = if self.content_path.is_empty() { join(&self.save_path, &self.name) } else { self.content_path };
        Torrent {
            hash: self.hash.to_ascii_lowercase(),
            name: self.name,
            ratio,
            seeding_secs,
            complete: self.progress >= 1.0,
            limit_reached,
            content_path,
        }
    }
}

pub(super) fn join(dir: &str, name: &str) -> String {
    format!("{}/{}", dir.trim_end_matches(['/', '\\']), name.trim_start_matches('/'))
}

impl Qbittorrent {
    pub(super) fn new(http: reqwest::Client, base: String, credentials: Credentials) -> Self {
        Self { http, base, credentials, cookie: Mutex::new(None) }
    }

    fn cookie(&self) -> Option<String> {
        self.cookie.lock().map(|cookie| cookie.clone()).unwrap_or(None)
    }

    fn set_cookie(&self, value: Option<String>) {
        if let Ok(mut cookie) = self.cookie.lock() {
            *cookie = value;
        }
    }

    async fn login(&self) -> Result<(), TorrentError> {
        let password = match &self.credentials.password {
            Ok(password) => password.clone().unwrap_or_default(),
            Err(var) => return Err(TorrentError::MissingSecret { client: KIND, var: var.clone() }),
        };
        let request = self
            .http
            .post(format!("{}/api/v2/auth/login", self.base))
            .header(reqwest::header::REFERER, &self.base)
            .form(&[("username", self.credentials.username.as_str()), ("password", password.as_str())]);
        let (headers, body) = match send(KIND, request).await {
            Err(TorrentError::Http { status: 401 | 403, .. }) => return Err(TorrentError::Login { client: KIND }),
            other => other?,
        };
        if body.trim() == "Fails." {
            return Err(TorrentError::Login { client: KIND });
        }
        let pairs: Vec<&str> = headers
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .filter_map(|value| value.split(';').next())
            .map(str::trim)
            .filter(|pair| pair.contains('='))
            .collect();
        // No cookie: the server bypasses authentication for this address.
        self.set_cookie((!pairs.is_empty()).then(|| pairs.join("; ")));
        Ok(())
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let request = self.http.request(method, format!("{}{path}", self.base)).header(reqwest::header::REFERER, &self.base);
        match self.cookie() {
            Some(cookie) => request.header(reqwest::header::COOKIE, cookie),
            None => request,
        }
    }

    /// One call, logging in first when there is no session, and once more
    /// when the server says the session lapsed.
    async fn call(&self, method: reqwest::Method, path: &str, form: &[(&str, &str)]) -> Result<String, TorrentError> {
        let authenticated = !self.credentials.username.is_empty() || !matches!(self.credentials.password, Ok(None));
        if self.cookie().is_none() && authenticated {
            self.login().await?;
        }
        for retry in [false, true] {
            let mut request = self.request(method.clone(), path);
            if !form.is_empty() {
                request = request.form(form);
            }
            match send(KIND, request).await {
                Err(TorrentError::Http { status: 403, .. }) if !retry => self.login().await?,
                other => return other.map(|(_, body)| body),
            }
        }
        Err(TorrentError::Login { client: KIND })
    }

    fn parse<T: serde::de::DeserializeOwned>(body: &str) -> Result<T, TorrentError> {
        serde_json::from_str(body).map_err(|error| TorrentError::Parse { client: KIND, detail: error.to_string() })
    }

    pub(super) async fn list(&self, hash: Option<&str>) -> Result<Vec<Torrent>, TorrentError> {
        let path = match hash {
            Some(hash) => format!("/api/v2/torrents/info?hashes={}", hash.to_ascii_lowercase()),
            None => "/api/v2/torrents/info".to_string(),
        };
        let body = self.call(reqwest::Method::GET, &path, &[]).await?;
        let rows: Vec<Info> = Self::parse(&body)?;
        Ok(rows.into_iter().map(Info::torrent).collect())
    }

    pub(super) async fn files(&self, hash: &str) -> Result<Vec<String>, TorrentError> {
        let listed = self.call(reqwest::Method::GET, &format!("/api/v2/torrents/info?hashes={}", hash.to_ascii_lowercase()), &[]).await?;
        let infos: Vec<Info> = Self::parse(&listed)?;
        let save_path = infos
            .into_iter()
            .find(|info| info.hash.eq_ignore_ascii_case(hash))
            .map(|info| info.save_path)
            .ok_or_else(|| TorrentError::Parse { client: KIND, detail: format!("torrent {hash} is not listed") })?;
        let body = self.call(reqwest::Method::GET, &format!("/api/v2/torrents/files?hash={}", hash.to_ascii_lowercase()), &[]).await?;
        let files: Vec<File> = Self::parse(&body)?;
        Ok(files.into_iter().map(|file| join(&save_path, &file.name)).collect())
    }

    pub(super) async fn remove(&self, hash: &str, delete_data: bool) -> Result<(), TorrentError> {
        let hash = hash.to_ascii_lowercase();
        let delete = if delete_data { "true" } else { "false" };
        self.call(reqwest::Method::POST, "/api/v2/torrents/delete", &[("hashes", hash.as_str()), ("deleteFiles", delete)]).await.map(drop)
    }
}
