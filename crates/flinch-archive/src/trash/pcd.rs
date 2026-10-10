//! The second source: a Profilarr Compliant Database (PCD), the open format
//! Profilarr reads (<https://github.com/Dictionarry-Hub/schema>). A PCD is a
//! `pcd.json` manifest and numbered SQL ops (`ops/*.sql`) that build on the
//! schema repository's own ops; replayed in order into SQLite they hold the
//! formats, profiles and size tables, which [`read`] lays into the same
//! guide model the TRaSH-Guides source fills. Desired, diff and apply do not
//! know which source an instance reads.
//!
//! License first: the database's `pcd.json` is read before any op, and a
//! database that declares no `license` is not fetched (unlicensed means all
//! rights reserved). The default, the Dictionarry database at
//! [`super::config::PCD_COMMIT`], declares MIT; its schema is MIT. The
//! license read is kept with the cached copy and shown on the page.
//!
//! PCD rows carry names, not trash ids: each format, profile and size table
//! gets [`id`] of its name, so a config names a PCD profile as it names a
//! TRaSH one. Both repositories are pinned commits, fetched once per pin
//! pair and cached like the guide.

mod read;
mod values;

pub use read::LANGUAGE_REJECT;

use super::config::PcdConfig;
use super::guide::{self, Guide, GuideError, GuideSource};
use crate::capacity::App;
use serde::Deserialize;
use std::path::Path;

/// Cached copies are `trash-pcd-{commit}-{schema commit}.json`.
const CACHE: &str = "trash-pcd-";

/// The stable 32-hex id of a PCD row: SHA-256 of `{kind}:{name}`, cut to
/// the length of a trash id. `kind` is `cf`, `profile` or `size`.
pub fn id(kind: &str, name: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, format!("{kind}:{name}").as_bytes());
    digest.as_ref().iter().take(16).map(|byte| format!("{byte:02x}")).collect()
}

/// `pcd.json`, the fields the sync reads (<https://github.com/Dictionarry-Hub/schema/blob/main/docs/manifest.md>).
#[derive(Debug, Deserialize)]
struct Manifest {
    name: String,
    #[serde(default)]
    license: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum PcdError {
    #[error(transparent)]
    Fetch(#[from] GuideError),
    #[error("the PCD {repository} ({name}) declares no license in pcd.json; FLINCH does not copy an unlicensed database")]
    Unlicensed { repository: String, name: String },
    #[error("PCD op {op}: {source}")]
    Sql { op: String, source: rusqlite::Error },
    #[error("PCD op {0} is not UTF-8 text")]
    Text(String),
    #[error("PCD replay task failed: {0}")]
    Task(String),
}

/// The cache key of a pin pair; also the read guide's `commit`.
pub fn key(config: &PcdConfig) -> String {
    format!("{}-{}", config.commit, config.schema_commit)
}

pub fn read_cache(dir: &Path, config: &PcdConfig) -> Option<Guide> {
    guide::read_cached(dir, CACHE, &key(config))
}

pub fn write_cache(dir: &Path, read: &Guide) -> std::io::Result<()> {
    guide::write_cached(dir, CACHE, &read.commit, read)
}

/// The leading number of an op file: ops run by it, not by name.
fn order(path: &str) -> (u64, String) {
    let file = path.rsplit('/').next().unwrap_or(path);
    let digits: String = file.chars().take_while(char::is_ascii_digit).collect();
    (digits.parse().unwrap_or(u64::MAX), file.to_string())
}

async fn ops(http: &reqwest::Client, source: &GuideSource) -> Result<Vec<(String, String)>, PcdError> {
    let mut paths = guide::list(http, source, "ops", ".sql").await?;
    paths.sort_by_key(|path| order(path));
    guide::fetch_all(http, source, paths)
        .await?
        .into_iter()
        .map(|(path, bytes)| String::from_utf8(bytes).map(|sql| (path.clone(), sql)).map_err(|_| PcdError::Text(path)))
        .collect()
}

/// Check the license, then fetch both repositories' ops, schema first, and
/// replay them. `key` becomes the read guide's `commit`.
pub async fn fetch(http: &reqwest::Client, database: &GuideSource, schema: &GuideSource, key: String) -> Result<Guide, PcdError> {
    let url = format!("{}/{}/{}/pcd.json", database.raw.trim_end_matches('/'), database.repo, database.commit);
    let manifest: Manifest = guide::parse(&guide::get(http, &url, "pcd.json", "application/json").await?, "pcd.json")?;
    let Some(license) = manifest.license.filter(|license| !license.trim().is_empty()) else {
        return Err(PcdError::Unlicensed { repository: database.repo.clone(), name: manifest.name });
    };
    let mut all = ops(http, schema).await?;
    all.extend(ops(http, database).await?);
    let (radarr, sonarr) = tokio::task::spawn_blocking(move || replay(&all)).await.map_err(|error| PcdError::Task(error.to_string()))??;
    Ok(Guide { commit: key, license: Some(license), radarr, sonarr })
}

/// Both apps' guides from ops already in order.
pub fn replay(ops: &[(String, String)]) -> Result<(guide::AppGuide, guide::AppGuide), PcdError> {
    let db = read::replay(ops)?;
    Ok((read::app_guide(&db, App::Radarr)?, read::app_guide(&db, App::Sonarr)?))
}

/// Where the configured PCD comes from, on GitHub.
pub fn sources(config: &PcdConfig) -> (GuideSource, GuideSource) {
    (GuideSource::repository(&config.repository, &config.commit), GuideSource::repository(&config.schema_repository, &config.schema_commit))
}
