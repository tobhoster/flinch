//! Reading the daemon's state directory into fit items.
//!
//! `items.json` is the library (required); `playback.json` (media-server
//! history) and `tautulli.json` (Tautulli streams) are the outcome record, and
//! either may be absent — a household runs on one, the other, or both.

use super::plays::PlayLog;
use super::FitItem;
use crate::card::LibraryKind;
use crate::ids::PlexIds;
use crate::plex::{public_normalise, PlayJoin, PlayKeys, PlexMetadata};
use crate::tautulli::{self, TautulliRow};
use crate::watch::WatchSource;
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
    /// Oldest stream Tautulli holds: its silence only counts after this.
    pub tautulli_coverage_start: Option<u64>,
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
    #[serde(default)]
    season_label: Option<String>,
    #[serde(default)]
    hard_guard: Option<String>,
    #[serde(default)]
    watch_source: Option<String>,
    #[serde(default)]
    series_status: Option<String>,
    #[serde(default)]
    last_aired_epoch: Option<u64>,
    /// The movie's (or show's) year: the only thing the title fallback may lean on.
    #[serde(default)]
    year: Option<u32>,
    /// Placement of a GUID-resolved item; present only for those.
    #[serde(default)]
    plex: Option<PlexIds>,
    /// How the daemon joined this item's plays, when it resolved in Plex: its
    /// ratingKeys and the `plex://` GUIDs that reach plays from before a
    /// library re-add, so the panel sees exactly the plays the daemon did.
    #[serde(default)]
    play_keys: Option<PlayKeys>,
    #[serde(default)]
    genres: Vec<String>,
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
    Ok(Household {
        items,
        unreadable_rows,
        plex_rows: plex.len(),
        tautulli_rows: streams.len(),
        tautulli_coverage_start: tautulli::coverage(&streams).map(|coverage| coverage.start),
    })
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
        genres: row.genres,
        on_disk: row.on_disk,
        plays: log.item_plays(&join).into_iter().cloned().collect(),
        audience_plays: log.audience_plays(&join).into_iter().cloned().collect(),
        id: row.id,
        title: row.title,
        kind,
        size_bytes: row.size_bytes,
        age_days,
        episodes_total: row.episodes,
        season_index,
        show_title,
        watch_source: row.watch_source.as_deref().and_then(WatchSource::from_label),
        is_newest_season: row.hard_guard.as_deref() == Some("newest-season"),
        series_status: row.series_status,
        last_aired_epoch: row.last_aired_epoch,
        guid_resolved: row.plex.is_some(),
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
