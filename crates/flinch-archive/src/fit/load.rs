//! Reading the daemon's state directory into fit items.
//!
//! `items.json` is the library (required); `playback.json` (media-server
//! history) and `tautulli.json` (Tautulli streams) are the outcome record, and
//! either may be absent — a household runs on one, the other, or both.

use super::plays::PlayLog;
use super::FitItem;
use crate::card::LibraryKind;
use crate::plex::{public_normalise, PlayJoin, PlayKeys, PlexMetadata};
use crate::tautulli::TautulliRow;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    /// `embeddings.json` exists but cannot be used. Absent is fine: no taste.
    #[error(transparent)]
    Vectors(#[from] crate::embedding::StoreError),
}

/// The household as the fitter sees it.
#[derive(Debug)]
pub struct Household {
    /// Library items with files on disk, their plays attached.
    pub items: Vec<FitItem>,
    /// `items.json` rows that were not a readable library item.
    pub unreadable_rows: usize,
    pub plex_rows: usize,
    pub tautulli_rows: usize,
    /// Every title's EmbeddingGemma vector the daemon has cached.
    pub vectors: crate::embedding::VectorStore,
}

/// What the fitter reads out of an `items.json` row; only the fields it needs.
#[derive(Debug, Deserialize)]
struct SnapshotRow {
    id: String,
    title: String,
    kind: String,
    #[serde(default)]
    size_bytes: u64,
    /// Absent for items with nothing on disk: nothing to judge.
    #[serde(default)]
    age_days: Option<f32>,
    #[serde(default)]
    episodes: Option<u32>,
    /// Episode numbers with a file, when the daemon read them.
    #[serde(default)]
    episodes_on_disk: Option<Vec<u32>>,
    #[serde(default)]
    season_label: Option<String>,
    /// The movie's (or show's) year: the only thing the title fallback may lean on.
    #[serde(default)]
    year: Option<u32>,
    /// How the daemon joined this item's plays, when it resolved in Plex: its
    /// ratingKeys and the `plex://` GUIDs that reach plays from before a
    /// library re-add, so the panel sees exactly the plays the daemon did.
    #[serde(default)]
    play_keys: Option<PlayKeys>,
    /// Presence spans from *arr history; absent in rows written before them.
    #[serde(default)]
    on_disk: Vec<crate::presence::Span>,
}

pub fn load_household(state_dir: &Path) -> Result<Household, LoadError> {
    let rows: Vec<serde_json::Value> = read_json(&state_dir.join("items.json"))?;
    let plex: Vec<PlexMetadata> = read_optional_json(&state_dir.join("playback.json"))?;
    let streams: Vec<TautulliRow> = read_optional_json(&state_dir.join("tautulli.json"))?;
    let log = PlayLog::new(&plex, &streams);

    let mut unreadable_rows = 0;
    let mut snapshots = Vec::new();
    for row in rows {
        match serde_json::from_value::<SnapshotRow>(row) {
            Ok(snapshot) => snapshots.push(snapshot),
            Err(_) => unreadable_rows += 1,
        }
    }
    // A movie title+year two library items share identifies neither; the
    // daemon's resolution refuses the fallback for it, and so does the fitter.
    let mut movie_titles: HashMap<(String, u32), usize> = HashMap::new();
    for snapshot in snapshots.iter().filter(|snapshot| snapshot.kind == "movie") {
        if let Some(year) = snapshot.year {
            *movie_titles.entry((public_normalise(&snapshot.title), year)).or_default() += 1;
        }
    }
    let items = snapshots
        .into_iter()
        .filter_map(|snapshot| {
            let unique = snapshot.year.is_some_and(|year| movie_titles.get(&(public_normalise(&snapshot.title), year)) == Some(&1));
            fit_item(snapshot, unique, &log)
        })
        .collect();
    let vectors = crate::embedding::VectorStore::read(state_dir)?;
    Ok(Household { items, unreadable_rows, plex_rows: plex.len(), tautulli_rows: streams.len(), vectors })
}

/// A library item with its plays, or `None` when nothing is on disk.
fn fit_item(row: SnapshotRow, unique_title: bool, log: &PlayLog) -> Option<FitItem> {
    let age_days = row.age_days?;
    let kind = if row.kind == "movie" { LibraryKind::Movie } else { LibraryKind::Season };
    let season_index = row.season_label.as_deref().and_then(|label| label.trim_start_matches('S').parse::<u32>().ok());
    let show_title = match kind {
        LibraryKind::Season => row
            .season_label
            .as_deref()
            .and_then(|label| row.title.strip_suffix(&format!(" {label}")))
            .map(str::to_string)
            .or_else(|| Some(row.title.clone())),
        LibraryKind::Movie => None,
    };
    // The same join the daemon's resolution applies at inference.
    let join = match row.play_keys {
        Some(keys) => PlayJoin::Keys(keys),
        None if unique_title => PlayJoin::fallback(kind, &row.title, row.year),
        None => PlayJoin::Unresolved,
    };
    Some(FitItem {
        on_disk: row.on_disk,
        plays: log.item_plays(&join).into_iter().cloned().collect(),
        audience_plays: log.audience_plays(&join).into_iter().cloned().collect(),
        id: row.id,
        title: row.title,
        kind,
        size_bytes: row.size_bytes,
        age_days,
        episodes_total: row.episodes,
        episodes_on_disk: row.episodes_on_disk,
        season_index,
        show_title,
    })
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, LoadError> {
    let text = std::fs::read_to_string(path).map_err(|source| LoadError::Read { path: path.to_path_buf(), source })?;
    parse(path, &text)
}

/// A missing file is an empty record; an unreadable or malformed one is an error.
fn read_optional_json<T: DeserializeOwned + Default>(path: &Path) -> Result<T, LoadError> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(path, &text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(source) => Err(LoadError::Read { path: path.to_path_buf(), source }),
    }
}

fn parse<T: DeserializeOwned>(path: &Path, text: &str) -> Result<T, LoadError> {
    serde_json::from_str(text).map_err(|source| LoadError::Parse { path: path.to_path_buf(), source })
}

#[cfg(test)]
mod tests;
