//! The TRaSH-Guides data the sync reads, per app: custom formats, quality
//! profiles and size tables, from `docs/json/{radarr,sonarr}/{cf,
//! quality-profiles,quality-size}` at one pinned commit
//! (<https://github.com/TRaSH-Guides/Guides/tree/master/docs/json>).
//!
//! A commit never changes, so the guide is fetched once per pin and cached
//! in the state dir; the network is only asked again when the pin moves. Any
//! file that fails to arrive or parse fails the whole fetch: a partial guide
//! would preview a profile without some of its custom formats.

use crate::capacity::App;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where the guide comes from: a GitHub repository at a commit. Tests point
/// both bases at a fake server; the PCD source ([`super::pcd`]) reads two
/// other repositories through the same calls.
#[derive(Debug, Clone)]
pub struct GuideSource {
    /// GitHub's REST API, for listing a directory at a commit.
    pub api: String,
    /// Raw file host, for the files themselves.
    pub raw: String,
    /// `owner/name`.
    pub repo: String,
    pub commit: String,
}

impl GuideSource {
    pub fn github(commit: &str) -> Self {
        Self::repository(REPO, commit)
    }

    pub fn repository(repo: &str, commit: &str) -> Self {
        Self {
            api: "https://api.github.com".to_string(),
            raw: "https://raw.githubusercontent.com".to_string(),
            repo: repo.to_string(),
            commit: commit.to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Guide {
    /// The pin it was read at (for a PCD, database and schema commits).
    pub commit: String,
    /// The license the source declared, when it declares one (PCD manifest).
    #[serde(default)]
    pub license: Option<String>,
    pub radarr: AppGuide,
    pub sonarr: AppGuide,
}

impl Guide {
    pub fn app(&self, app: App) -> &AppGuide {
        match app {
            App::Radarr => &self.radarr,
            App::Sonarr => &self.sonarr,
        }
    }
}

/// One app's guide, keyed by trash id.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AppGuide {
    pub custom_formats: BTreeMap<String, GuideCustomFormat>,
    pub profiles: BTreeMap<String, GuideProfile>,
    pub sizes: Vec<GuideSize>,
    /// Formats the source has but this app cannot take, by id: why. A
    /// profile scoring one shows the reason instead of a bare "missing".
    #[serde(default)]
    pub skipped: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuideCustomFormat {
    pub trash_id: String,
    pub name: String,
    /// Score per score set; `default` unless a profile names another.
    #[serde(default)]
    pub trash_scores: BTreeMap<String, i32>,
    #[serde(default, rename = "includeCustomFormatWhenRenaming")]
    pub include_when_renaming: bool,
    #[serde(default)]
    pub specifications: Vec<GuideSpecification>,
}

/// The guide writes fields as an object (`{"value": …}`); the *arrs take a
/// list of `{name, value}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuideSpecification {
    pub name: String,
    pub implementation: String,
    #[serde(default)]
    pub negate: bool,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub fields: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuideProfile {
    #[serde(rename = "trash_id")]
    pub trash_id: String,
    pub name: String,
    #[serde(default, rename = "trash_score_set")]
    pub trash_score_set: Option<String>,
    pub upgrade_allowed: bool,
    pub cutoff: String,
    #[serde(default)]
    pub min_format_score: i32,
    #[serde(default)]
    pub cutoff_format_score: i32,
    #[serde(default)]
    pub min_upgrade_format_score: Option<i32>,
    /// Radarr only: the profile's language by name ("Original", "Any", …).
    #[serde(default)]
    pub language: Option<String>,
    /// Best first, as the guide lists them; the *arr API lists worst first.
    pub items: Vec<GuideQualityItem>,
    /// Custom format name → trash id.
    #[serde(default)]
    pub format_items: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuideQualityItem {
    pub name: String,
    pub allowed: bool,
    /// Set for a group: its member qualities.
    #[serde(default)]
    pub items: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuideSize {
    pub trash_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub qualities: Vec<GuideSizeQuality>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuideSizeQuality {
    pub quality: String,
    pub min: f64,
    #[serde(default)]
    pub preferred: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
}

/// Why the guide could not be read. No variant carries a URL with a query.
#[derive(Debug, thiserror::Error)]
pub enum GuideError {
    #[error("guide {what}: request failed: {source}")]
    Transport { what: String, source: reqwest::Error },
    #[error("guide {what}: HTTP {status}")]
    Http { what: String, status: u16 },
    #[error("guide {what}: {source}")]
    Body { what: String, source: crate::body::BodyError },
    #[error("guide {what} is not what the sync expects: {source}")]
    Parse { what: String, source: serde_json::Error },
    #[error("guide fetch task failed: {0}")]
    Task(String),
}

const REPO: &str = "TRaSH-Guides/Guides";
/// GitHub asks every API client to name itself and refuses a request without
/// a User-Agent (<https://docs.github.com/en/rest/using-the-rest-api/getting-started-with-the-rest-api#user-agent>).
const USER_AGENT: &str = "flinch-trash-sync";
/// Files in flight at once: a first fetch is about 550 small files.
const PARALLEL: usize = 8;

pub(super) async fn get(http: &reqwest::Client, url: &str, what: &str, accept: &str) -> Result<Vec<u8>, GuideError> {
    let response = http
        .get(url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .header(reqwest::header::ACCEPT, accept)
        .send()
        .await
        .map_err(|source| GuideError::Transport { what: what.to_string(), source: source.without_url() })?;
    let status = response.status();
    if !status.is_success() {
        return Err(GuideError::Http { what: what.to_string(), status: status.as_u16() });
    }
    crate::body::read(response).await.map_err(|source| GuideError::Body { what: what.to_string(), source })
}

pub(super) fn parse<T: serde::de::DeserializeOwned>(bytes: &[u8], what: &str) -> Result<T, GuideError> {
    serde_json::from_slice(bytes).map_err(|source| GuideError::Parse { what: what.to_string(), source })
}

/// Every file ending in `extension` in one directory of the source's repo at
/// its commit, through the contents API (<https://docs.github.com/en/rest/repos/contents#get-repository-content>;
/// up to 1,000 entries, and the largest directory holds about 360).
pub(super) async fn list(http: &reqwest::Client, source: &GuideSource, dir: &str, extension: &str) -> Result<Vec<String>, GuideError> {
    #[derive(Deserialize)]
    struct Entry {
        path: String,
        #[serde(rename = "type")]
        kind: String,
    }
    let url = format!("{}/repos/{}/contents/{dir}?ref={}", source.api.trim_end_matches('/'), source.repo, source.commit);
    let entries: Vec<Entry> = parse(&get(http, &url, dir, "application/vnd.github+json").await?, dir)?;
    Ok(entries.into_iter().filter(|entry| entry.kind == "file" && entry.path.ends_with(extension)).map(|entry| entry.path).collect())
}

/// Every file of `paths`, at most [`PARALLEL`] at a time, in input order.
pub(super) async fn fetch_all(
    http: &reqwest::Client,
    source: &GuideSource,
    paths: Vec<String>,
) -> Result<Vec<(String, Vec<u8>)>, GuideError> {
    let raw = source.raw.trim_end_matches('/').to_string();
    let mut pending = paths.into_iter().enumerate();
    let mut running = tokio::task::JoinSet::new();
    let mut done = Vec::new();
    loop {
        while running.len() < PARALLEL {
            let Some((index, path)) = pending.next() else { break };
            let (http, url) = (http.clone(), format!("{raw}/{}/{}/{path}", source.repo, source.commit));
            running.spawn(async move {
                let body = get(&http, &url, &path, "application/json").await;
                (index, path, body)
            });
        }
        let Some(joined) = running.join_next().await else { break };
        let (index, path, body) = joined.map_err(|error| GuideError::Task(error.to_string()))?;
        done.push((index, path, body?));
    }
    done.sort_by_key(|(index, ..)| *index);
    Ok(done.into_iter().map(|(_, path, body)| (path, body)).collect())
}

async fn fetch_app(http: &reqwest::Client, source: &GuideSource, app: App) -> Result<AppGuide, GuideError> {
    let base = format!("docs/json/{}", app.label());
    let mut guide = AppGuide::default();
    for (path, bytes) in fetch_all(http, source, list(http, source, &format!("{base}/cf"), ".json").await?).await? {
        let format: GuideCustomFormat = parse(&bytes, &path)?;
        guide.custom_formats.insert(format.trash_id.clone(), format);
    }
    for (path, bytes) in fetch_all(http, source, list(http, source, &format!("{base}/quality-profiles"), ".json").await?).await? {
        let profile: GuideProfile = parse(&bytes, &path)?;
        guide.profiles.insert(profile.trash_id.clone(), profile);
    }
    for (path, bytes) in fetch_all(http, source, list(http, source, &format!("{base}/quality-size"), ".json").await?).await? {
        guide.sizes.push(parse(&bytes, &path)?);
    }
    Ok(guide)
}

/// The whole guide at the source's commit, over the network.
pub async fn fetch(http: &reqwest::Client, source: &GuideSource) -> Result<Guide, GuideError> {
    let (radarr, sonarr) = (fetch_app(http, source, App::Radarr).await?, fetch_app(http, source, App::Sonarr).await?);
    Ok(Guide { commit: source.commit.clone(), license: None, radarr, sonarr })
}

/// TRaSH-Guides copies are `trash-guide-{commit}.json`.
const GUIDE_CACHE: &str = "trash-guide-";

fn cache_path(dir: &Path, prefix: &str, key: &str) -> PathBuf {
    dir.join(format!("{prefix}{key}.json"))
}

/// The cached guide for `commit`, if one was written and still parses.
pub fn read_cache(dir: &Path, commit: &str) -> Option<Guide> {
    read_cached(dir, GUIDE_CACHE, commit)
}

/// Cache the guide and drop the copies of other commits: only the pin is read.
pub fn write_cache(dir: &Path, guide: &Guide) -> std::io::Result<()> {
    write_cached(dir, GUIDE_CACHE, &guide.commit, guide)
}

/// A guide cached under `{prefix}{key}.json`, whose `commit` is `key`.
pub(super) fn read_cached(dir: &Path, prefix: &str, key: &str) -> Option<Guide> {
    let guide: Guide = serde_json::from_slice(&std::fs::read(cache_path(dir, prefix, key)).ok()?).ok()?;
    (guide.commit == key).then_some(guide)
}

/// Cache `guide` under its `commit` and drop every other key of `prefix`.
pub(super) fn write_cached(dir: &Path, prefix: &str, key: &str, guide: &Guide) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let keep = cache_path(dir, prefix, key);
    crate::persist::replace(&keep, &serde_json::to_vec(guide)?)?;
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name();
        let stale = name.to_str().is_some_and(|name| name.starts_with(prefix) && name.ends_with(".json"));
        if stale && entry.path() != keep {
            std::fs::remove_file(entry.path()).ok();
        }
    }
    Ok(())
}
