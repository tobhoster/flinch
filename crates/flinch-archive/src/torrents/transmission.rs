//! Transmission's RPC: a JSON POST guarded by the `X-Transmission-Session-Id`
//! handshake, with optional HTTP basic authentication.
//!
//! Shapes from the official spec,
//! https://github.com/transmission/transmission/blob/main/docs/rpc-spec.md:
//! - 2.2.1: the first request (or one with a stale id) answers 409 carrying
//!   the id in `X-Transmission-Session-Id`; resend with it.
//! - 5: from RPC 6.0.0 (Transmission 4.1) the 409 also carries
//!   `X-Transmission-Rpc-Version`, and the server speaks JSON-RPC 2.0 with
//!   snake_case methods and keys (`torrent_get`, `hash_string`, `params` →
//!   `result`, failures in `error`). Without the header the server is older
//!   and speaks the bespoke protocol (`torrent-get`, `hashString`,
//!   `arguments`, `"result": "success"`). The header picks the dialect.
//! - 3.3 `torrent_get` fields: `hash_string`, `name`, `upload_ratio` (-1 n/a,
//!   -2 infinite), `seconds_seeding`, `is_finished` ("has reached its seed
//!   ratio or idle limit"), `percent_done`, `download_dir`, and `files`
//!   (`name` relative to `download_dir`). `ids` takes hash strings.
//! - 3.5 `torrent_remove`: `ids`, `delete_local_data`.

use super::qbittorrent::join;
use super::{receive, ClientKind, Credentials, Torrent, TorrentError, UNBOUNDED_RATIO};
use serde_json::{json, Value};
use std::sync::Mutex;

const KIND: ClientKind = ClientKind::Transmission;
const SESSION: &str = "X-Transmission-Session-Id";
const VERSION: &str = "X-Transmission-Rpc-Version";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dialect {
    /// Before RPC 6: dashed methods, camelCase keys, `arguments`.
    Bespoke,
    /// RPC 6 and later: JSON-RPC 2.0, snake_case.
    JsonRpc,
}

pub(super) struct Transmission {
    http: reqwest::Client,
    url: String,
    credentials: Credentials,
    session: Mutex<Option<(String, Dialect)>>,
}

/// One key in both dialects.
struct Key {
    bespoke: &'static str,
    snake: &'static str,
}

const HASH: Key = Key { bespoke: "hashString", snake: "hash_string" };
const NAME: Key = Key { bespoke: "name", snake: "name" };
const RATIO: Key = Key { bespoke: "uploadRatio", snake: "upload_ratio" };
const SEEDING: Key = Key { bespoke: "secondsSeeding", snake: "seconds_seeding" };
const FINISHED: Key = Key { bespoke: "isFinished", snake: "is_finished" };
const DONE: Key = Key { bespoke: "percentDone", snake: "percent_done" };
const DIR: Key = Key { bespoke: "downloadDir", snake: "download_dir" };
const FILES: Key = Key { bespoke: "files", snake: "files" };

impl Key {
    fn name(&self, dialect: Dialect) -> &'static str {
        match dialect {
            Dialect::Bespoke => self.bespoke,
            Dialect::JsonRpc => self.snake,
        }
    }
}

impl Transmission {
    /// `base` is the RPC URL itself, or a server root that gets the default
    /// `/transmission/rpc`.
    pub(super) fn new(http: reqwest::Client, base: &str, credentials: Credentials) -> Self {
        let has_path = reqwest::Url::parse(base).is_ok_and(|url| !url.path().trim_matches('/').is_empty());
        let url = if has_path { base.to_string() } else { format!("{base}/transmission/rpc") };
        Self { http, url, credentials, session: Mutex::new(None) }
    }

    fn session(&self) -> Option<(String, Dialect)> {
        self.session.lock().map(|session| session.clone()).unwrap_or(None)
    }

    /// One method call in the server's dialect; `arguments` is built for it.
    async fn call(&self, method: Key, arguments: impl Fn(Dialect) -> Value) -> Result<(Value, Dialect), TorrentError> {
        let password = match &self.credentials.password {
            Ok(password) => password.clone(),
            Err(var) => return Err(TorrentError::MissingSecret { client: KIND, var: var.clone() }),
        };
        for _ in 0..2 {
            let session = self.session();
            let dialect = session.as_ref().map_or(Dialect::Bespoke, |(_, dialect)| *dialect);
            let body = match dialect {
                Dialect::Bespoke => json!({ "method": method.bespoke, "arguments": arguments(dialect) }),
                Dialect::JsonRpc => json!({ "jsonrpc": "2.0", "method": method.snake, "params": arguments(dialect), "id": 1 }),
            };
            let mut request = self.http.post(&self.url).json(&body);
            if let Some((id, _)) = &session {
                request = request.header(SESSION, id);
            }
            if !self.credentials.username.is_empty() || password.is_some() {
                request = request.basic_auth(&self.credentials.username, password.as_deref());
            }
            let response = request.send().await.map_err(|source| TorrentError::Transport { client: KIND, source: source.without_url() })?;
            if response.status() == reqwest::StatusCode::CONFLICT {
                let id = header(&response, SESSION).ok_or(TorrentError::Http { client: KIND, status: 409 })?;
                let modern = header(&response, VERSION)
                    .and_then(|version| version.split('.').next().and_then(|major| major.parse::<u32>().ok()))
                    .is_some_and(|major| major >= 6);
                let dialect = if modern { Dialect::JsonRpc } else { Dialect::Bespoke };
                if let Ok(mut session) = self.session.lock() {
                    *session = Some((id, dialect));
                }
                continue;
            }
            if response.status() == reqwest::StatusCode::UNAUTHORIZED {
                return Err(TorrentError::Login { client: KIND });
            }
            let (_, text) = receive(KIND, response).await?;
            let reply: Value = serde_json::from_str(&text).map_err(|error| parse(error.to_string()))?;
            return unwrap(reply, dialect).map(|value| (value, dialect));
        }
        Err(TorrentError::Http { client: KIND, status: 409 })
    }

    async fn get(&self, hash: Option<&str>, fields: &[&Key]) -> Result<(Vec<Value>, Dialect), TorrentError> {
        let hash = hash.map(str::to_ascii_lowercase);
        let (arguments, dialect) = self
            .call(Key { bespoke: "torrent-get", snake: "torrent_get" }, |dialect| {
                let names: Vec<&str> = fields.iter().map(|key| key.name(dialect)).collect();
                match &hash {
                    Some(hash) => json!({ "fields": names, "ids": [hash] }),
                    None => json!({ "fields": names }),
                }
            })
            .await?;
        let torrents = arguments.get("torrents").and_then(Value::as_array).cloned().ok_or_else(|| parse("no torrents array".into()))?;
        Ok((torrents, dialect))
    }

    pub(super) async fn list(&self, hash: Option<&str>) -> Result<Vec<Torrent>, TorrentError> {
        let (rows, dialect) = self.get(hash, &[&HASH, &NAME, &RATIO, &SEEDING, &FINISHED, &DONE, &DIR]).await?;
        rows.iter().map(|row| torrent(row, dialect)).collect()
    }

    pub(super) async fn files(&self, hash: &str) -> Result<Vec<String>, TorrentError> {
        let (rows, dialect) = self.get(Some(hash), &[&HASH, &DIR, &FILES]).await?;
        let row = rows
            .iter()
            .find(|row| text(row, &HASH, dialect).is_some_and(|found| found.eq_ignore_ascii_case(hash)))
            .ok_or_else(|| parse(format!("torrent {hash} is not listed")))?;
        let dir = text(row, &DIR, dialect).unwrap_or_default();
        let files = row.get(FILES.name(dialect)).and_then(Value::as_array).ok_or_else(|| parse("no files array".into()))?;
        files
            .iter()
            .map(|file| {
                file.get("name").and_then(Value::as_str).map(|name| join(dir, name)).ok_or_else(|| parse("a file has no name".into()))
            })
            .collect()
    }

    pub(super) async fn remove(&self, hash: &str, delete_data: bool) -> Result<(), TorrentError> {
        let hash = hash.to_ascii_lowercase();
        self.call(Key { bespoke: "torrent-remove", snake: "torrent_remove" }, |dialect| match dialect {
            Dialect::Bespoke => json!({ "ids": [hash], "delete-local-data": delete_data }),
            Dialect::JsonRpc => json!({ "ids": [hash], "delete_local_data": delete_data }),
        })
        .await
        .map(drop)
    }
}

fn header(response: &reqwest::Response, name: &str) -> Option<String> {
    response.headers().get(name).and_then(|value| value.to_str().ok()).map(str::to_string)
}

fn parse(detail: String) -> TorrentError {
    TorrentError::Parse { client: KIND, detail }
}

/// The reply's payload: `arguments` after `"result":"success"`, or a
/// JSON-RPC `result`; anything else is the server's own failure text.
fn unwrap(reply: Value, dialect: Dialect) -> Result<Value, TorrentError> {
    match dialect {
        Dialect::Bespoke => match reply.get("result").and_then(Value::as_str) {
            Some("success") => Ok(reply.get("arguments").cloned().unwrap_or(Value::Null)),
            other => Err(parse(format!("result {:?}", other.unwrap_or("missing")))),
        },
        Dialect::JsonRpc => match (reply.get("result"), reply.get("error")) {
            (Some(result), None) => Ok(result.clone()),
            (_, Some(error)) => {
                Err(parse(format!("error {}", error.get("message").and_then(Value::as_str).unwrap_or("without a message"))))
            }
            (None, None) => Err(parse("no result".into())),
        },
    }
}

fn text<'a>(row: &'a Value, key: &Key, dialect: Dialect) -> Option<&'a str> {
    row.get(key.name(dialect)).and_then(Value::as_str)
}

fn torrent(row: &Value, dialect: Dialect) -> Result<Torrent, TorrentError> {
    let number = |key: &Key| row.get(key.name(dialect)).and_then(Value::as_f64);
    let hash = text(row, &HASH, dialect).ok_or_else(|| parse("a torrent has no hash".into()))?.to_ascii_lowercase();
    let name = text(row, &NAME, dialect).unwrap_or_default().to_string();
    let ratio = match number(&RATIO).unwrap_or(0.0) {
        -2.0 => UNBOUNDED_RATIO,
        ratio if ratio < 0.0 => 0.0,
        ratio => ratio,
    };
    Ok(Torrent {
        content_path: join(text(row, &DIR, dialect).unwrap_or_default(), &name),
        hash,
        name,
        ratio,
        seeding_secs: number(&SEEDING).map_or(0, |secs| secs.max(0.0) as u64),
        complete: number(&DONE).is_some_and(|done| done >= 1.0),
        limit_reached: row.get(FINISHED.name(dialect)).and_then(Value::as_bool).unwrap_or(false),
    })
}
