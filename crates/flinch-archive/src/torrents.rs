//! Torrents that still hold the library's bytes.
//!
//! A download client keeps seeding after Radarr or Sonarr import, usually
//! through a hardlink to the library file. Two things follow, and both are
//! about deleting the right thing at the right time:
//! - **Seed goals.** Removing a torrent before its tracker's ratio or time is
//!   met costs the household its standing there. An item whose torrent has
//!   not met its goal (the client's own share limit, or FLINCH's floor) is
//!   kept: [`SeedHold::BelowGoal`].
//! - **Bytes held.** Deleting a hardlinked library file frees nothing while
//!   the torrent's link remains. Its size counts only when the torrent goes
//!   too, so otherwise the item is kept: [`SeedHold::HeldByTorrent`].
//! - **Desired ratio.** Past its goal, a torrent may still be short of the
//!   ratio the operator would like (`prefer_after_ratio`). Its item is not
//!   kept, only spared: drawn on once nothing else on its disk fills the
//!   target ([`map::spared`]).
//!
//! Which torrent holds which card comes from *arr history (`downloadId` of the
//! newest import of each file), falling back to a torrent saved inside the
//! card's folder. Missing evidence keeps, as everywhere: a client that cannot
//! be read, or links that cannot be checked, hold the items they might touch.
//! Every request is a read except [`Torrents::remove`], which a dry run prints
//! instead of sending, and which is read back to confirm.

pub mod map;
mod qbittorrent;
mod transmission;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// `settings.json` `torrents`. Off (no clients) by default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TorrentsConfig {
    pub clients: Vec<ClientConfig>,
    /// Keep an item while any of its torrents is below its seed goal.
    pub respect_seed_goals: bool,
    /// After the native executor deletes an item, remove its torrents (and
    /// their data) once they met their goal. Ignored with Maintainerr.
    pub remove_after_delete: bool,
    /// FLINCH's seed goal: a torrent at this ratio has met it…
    pub min_ratio: f64,
    /// …as has one seeded this many days. 0 and 0 set no goal of FLINCH's own.
    pub min_seed_days: u32,
    /// A desired ratio, softer than the goal: an item whose torrent is below
    /// it is a [`crate::plan::knapsack::Force::Spare`], taken only once
    /// nothing else on its disk fills the target. 0 (the default) is off.
    pub prefer_after_ratio: f64,
    /// How a client's path reads inside the daemon, for the hardlink check:
    /// the longest matching `from` prefix is replaced by `to`. Applied to
    /// *arr library paths too.
    pub path_map: Vec<PathMap>,
}

impl Default for TorrentsConfig {
    fn default() -> Self {
        Self {
            clients: Vec::new(),
            respect_seed_goals: true,
            remove_after_delete: true,
            min_ratio: 1.0,
            min_seed_days: 14,
            prefer_after_ratio: 0.0,
            path_map: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientConfig {
    pub kind: ClientKind,
    /// qBittorrent's Web UI root, or Transmission's root or full RPC URL.
    pub url: String,
    #[serde(default)]
    pub username: String,
    /// The environment variable holding the password; the password itself
    /// never enters `settings.json`.
    #[serde(default)]
    pub password_env: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientKind {
    Qbittorrent,
    Transmission,
}

impl std::fmt::Display for ClientKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Qbittorrent => "qbittorrent",
            Self::Transmission => "transmission",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathMap {
    pub from: String,
    pub to: String,
}

/// A [`TorrentsConfig`] outside its bounds; the message names the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidTorrentsConfig(pub &'static str);

impl TorrentsConfig {
    pub fn validate(&self) -> Result<(), InvalidTorrentsConfig> {
        if !(self.min_ratio.is_finite() && (0.0..=100.0).contains(&self.min_ratio)) {
            return Err(InvalidTorrentsConfig("the minimum seed ratio (torrents.min_ratio) must be 0 to 100"));
        }
        if self.min_seed_days > 3_650 {
            return Err(InvalidTorrentsConfig("the minimum seed days (torrents.min_seed_days) must be at most 3650"));
        }
        if !(self.prefer_after_ratio.is_finite() && (0.0..=100.0).contains(&self.prefer_after_ratio)) {
            return Err(InvalidTorrentsConfig("the desired seed ratio (torrents.prefer_after_ratio) must be 0 to 100"));
        }
        for client in &self.clients {
            let url = reqwest::Url::parse(&client.url).map_err(|_| InvalidTorrentsConfig("a torrent client URL is not a URL"))?;
            if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                return Err(InvalidTorrentsConfig("a torrent client URL must be http(s)://host…"));
            }
            if !url.username().is_empty() || url.password().is_some() {
                return Err(InvalidTorrentsConfig("put torrent client credentials in username and password_env, not the URL"));
            }
            let env_name = |name: &str| !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
            if client.password_env.as_deref().is_some_and(|name| !env_name(name)) {
                return Err(InvalidTorrentsConfig("a torrent client password_env must name an environment variable (A-Z, 0-9, _)"));
            }
        }
        if self.path_map.iter().any(|map| !map.from.starts_with('/') || !map.to.starts_with('/')) {
            return Err(InvalidTorrentsConfig("torrent path_map entries must map absolute paths"));
        }
        Ok(())
    }

    /// Whether a torrent at `ratio` after `seeding_secs` has met its goal:
    /// complete, and the client's own share limit reached or FLINCH's floor
    /// met (either the ratio or the days, as trackers' hit-and-run rules read).
    pub fn meets_goal(&self, torrent: &Torrent) -> bool {
        if !torrent.complete {
            return false;
        }
        let no_floor = self.min_ratio <= 0.0 && self.min_seed_days == 0;
        torrent.limit_reached
            || no_floor
            || (self.min_ratio > 0.0 && torrent.ratio >= self.min_ratio)
            || (self.min_seed_days > 0 && torrent.seeding_secs >= u64::from(self.min_seed_days) * 86_400)
    }

    /// Whether the native executor removes `holding` after deleting every
    /// card it holds (the caller checks the cards and the executor).
    pub fn may_remove(&self, holding: &Holding) -> bool {
        self.removes(holding.meets_goal)
    }

    /// [`Self::may_remove`] for a torrent that does or does not meet its goal.
    pub fn removes(&self, meets_goal: bool) -> bool {
        self.remove_after_delete && (meets_goal || !self.respect_seed_goals)
    }

    /// `path` as the daemon sees it, through the longest matching
    /// [`PathMap::from`] that ends at a path component.
    pub fn local(&self, path: &str) -> PathBuf {
        let best = self
            .path_map
            .iter()
            .filter(|map| {
                let from = map.from.trim_end_matches('/');
                path.strip_prefix(from).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
            })
            .max_by_key(|map| map.from.trim_end_matches('/').len());
        match best {
            Some(map) => {
                let rest = &path[map.from.trim_end_matches('/').len()..];
                PathBuf::from(format!("{}{rest}", map.to.trim_end_matches('/')))
            }
            None => PathBuf::from(path),
        }
    }
}

/// One torrent as a client lists it, reduced to what the seed goal and the
/// card match need. Paths are the client's.
#[derive(Debug, Clone, PartialEq)]
pub struct Torrent {
    /// Lower-case info hash, as the *arr `downloadId` reads once lowered.
    pub hash: String,
    pub name: String,
    pub ratio: f64,
    pub seeding_secs: u64,
    pub complete: bool,
    /// The client's own share limit (ratio or seeding time) is reached.
    pub limit_reached: bool,
    /// The content's root: the file of a single-file torrent, else its folder.
    pub content_path: String,
}

/// A ratio the clients report as unbounded (qBittorrent caps at 9999 and
/// sends -1 above; Transmission sends -2 for "infinite").
pub const UNBOUNDED_RATIO: f64 = 9_999.0;

/// One torrent holding one or more cards.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Holding {
    pub hash: String,
    pub client: ClientKind,
    /// Position of its client in [`TorrentsConfig::clients`].
    pub client_index: usize,
    pub ratio: f64,
    pub seeding_secs: u64,
    pub meets_goal: bool,
    /// The torrent's files (client paths) that are hardlinks of a file in the
    /// card's library folder: while the torrent stays, deleting frees nothing.
    pub hardlinked_paths: Vec<String>,
    /// Every file and the library folder could be read. `false` reads as held.
    pub links_verified: bool,
    /// Every card this torrent holds (a pack spans seasons).
    pub cards: Vec<String>,
}

/// Why an item stays for its torrents ([`crate::plan::Exclusion::Seeding`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "why")]
pub enum SeedHold {
    /// A torrent below its seed goal; the lowest such ratio, in hundredths.
    BelowGoal { ratio_centi: u32, seeding_days: u32 },
    /// Hardlinked to a torrent that stays: deleting frees nothing yet.
    HeldByTorrent,
    /// A torrent holds it and whether it is hardlinked could not be read.
    LinksUnverified,
    /// A client that may hold it could not be read this cycle.
    ClientUnreadable,
}

impl std::fmt::Display for SeedHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BelowGoal { ratio_centi, seeding_days } => {
                write!(f, "Seeding: ratio {}.{:02} after {seeding_days} day(s), below its seed goal", ratio_centi / 100, ratio_centi % 100)
            }
            Self::HeldByTorrent => f.write_str("Hardlinked to a torrent that stays: deleting would free nothing yet"),
            Self::LinksUnverified => f.write_str("Held by a torrent whose hardlinks FLINCH cannot read"),
            Self::ClientUnreadable => f.write_str("Its torrent client could not be read this run"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TorrentError {
    /// The URL is stripped: it names the client's address.
    #[error("{client}: request failed: {source}")]
    Transport { client: ClientKind, source: reqwest::Error },
    #[error("{client}: {source}")]
    Body { client: ClientKind, source: crate::body::BodyError },
    /// Any non-2xx, redirects included: credentials would travel with one.
    #[error("{client}: HTTP {status}")]
    Http { client: ClientKind, status: u16 },
    #[error("{client}: login refused")]
    Login { client: ClientKind },
    #[error("{client}: password variable {var} is not set")]
    MissingSecret { client: ClientKind, var: String },
    #[error("{client}: unexpected response: {detail}")]
    Parse { client: ClientKind, detail: String },
    #[error("{client}: torrent {hash} is still listed after its removal")]
    NotApplied { client: ClientKind, hash: String },
    #[error("no torrent client number {0} is configured")]
    UnknownClient(usize),
}

enum Client {
    Qbittorrent(qbittorrent::Qbittorrent),
    Transmission(transmission::Transmission),
}

/// Every configured client, through the daemon's no-redirect HTTP client.
pub struct Torrents {
    clients: Vec<Client>,
    dry_run: bool,
}

/// The username, and the password read from its variable (`Err` names a
/// variable that is unset).
struct Credentials {
    username: String,
    password: Result<Option<String>, String>,
}

impl Torrents {
    pub fn new(http: &reqwest::Client, config: &TorrentsConfig, dry_run: bool) -> Self {
        let clients = config
            .clients
            .iter()
            .map(|client| {
                let password = match client.password_env.as_deref().filter(|name| !name.is_empty()) {
                    Some(var) => std::env::var(var).map(Some).map_err(|_| var.to_string()),
                    None => Ok(None),
                };
                let credentials = Credentials { username: client.username.clone(), password };
                let base = client.url.trim_end_matches('/').to_string();
                match client.kind {
                    ClientKind::Qbittorrent => Client::Qbittorrent(qbittorrent::Qbittorrent::new(http.clone(), base, credentials)),
                    ClientKind::Transmission => Client::Transmission(transmission::Transmission::new(http.clone(), &base, credentials)),
                }
            })
            .collect();
        Self { clients, dry_run }
    }

    /// Every torrent of every client, index-aligned with the configuration.
    pub async fn list(&self) -> Vec<Result<Vec<Torrent>, TorrentError>> {
        let mut listings = Vec::with_capacity(self.clients.len());
        for client in &self.clients {
            listings.push(match client {
                Client::Qbittorrent(client) => client.list(None).await,
                Client::Transmission(client) => client.list(None).await,
            });
        }
        listings
    }

    /// The torrent's files, as absolute paths in the client's view.
    pub async fn files(&self, client: usize, hash: &str) -> Result<Vec<String>, TorrentError> {
        match self.clients.get(client).ok_or(TorrentError::UnknownClient(client))? {
            Client::Qbittorrent(client) => client.files(hash).await,
            Client::Transmission(client) => client.files(hash).await,
        }
    }

    /// Remove `holding`'s torrent, its data too when `delete_data`, and read
    /// the client back to confirm it is gone. A dry run prints and sends nothing.
    pub async fn remove(&self, holding: &Holding, delete_data: bool) -> Result<(), TorrentError> {
        let client = self.clients.get(holding.client_index).ok_or(TorrentError::UnknownClient(holding.client_index))?;
        if self.dry_run {
            println!("[dry-run] would remove torrent {} from {} (delete data: {delete_data})", holding.hash, holding.client);
            return Ok(());
        }
        match client {
            Client::Qbittorrent(client) => client.remove(&holding.hash, delete_data).await?,
            Client::Transmission(client) => client.remove(&holding.hash, delete_data).await?,
        }
        // Both clients drop a removed torrent from their listing at once; a
        // short wait covers a busy session finishing the removal.
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            let listed = match client {
                Client::Qbittorrent(client) => client.list(Some(&holding.hash)).await?,
                Client::Transmission(client) => client.list(Some(&holding.hash)).await?,
            };
            if !listed.iter().any(|torrent| torrent.hash == holding.hash) {
                return Ok(());
            }
        }
        Err(TorrentError::NotApplied { client: holding.client, hash: holding.hash.clone() })
    }
}

/// Send `request`; a non-2xx (redirects included) is an error, the body read
/// under [`crate::body`]'s cap. Errors carry no URL.
async fn send(client: ClientKind, request: reqwest::RequestBuilder) -> Result<(reqwest::header::HeaderMap, String), TorrentError> {
    let response = request.send().await.map_err(|source| TorrentError::Transport { client, source: source.without_url() })?;
    receive(client, response).await
}

/// [`send`]'s second half, for a response already in hand.
async fn receive(client: ClientKind, response: reqwest::Response) -> Result<(reqwest::header::HeaderMap, String), TorrentError> {
    let status = response.status();
    if !status.is_success() {
        return Err(TorrentError::Http { client, status: status.as_u16() });
    }
    let headers = response.headers().clone();
    let body = crate::body::read_text(response).await.map_err(|source| TorrentError::Body { client, source })?;
    Ok((headers, body))
}

#[cfg(test)]
mod tests;
