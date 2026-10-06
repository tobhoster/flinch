//! What Radarr and Sonarr imported lately and what they are still downloading.
//!
//! Wire shape, from the Radarr and Sonarr sources (`develop`): `GET
//! /api/v3/history/since?date=&eventType=3` answers a bare `HistoryResource`
//! array, not a page; event type 3 is the import (`downloadFolderImported`)
//! in both apps. A record's `data` is a `Dictionary<string, string>`, so
//! `data.size` is the file's byte count as a string. Sonarr writes one import
//! record per episode, each with that episode's own file size, so a season
//! pack is the sum of its records. `GET /api/v3/queue` answers a
//! `PagingResource<QueueResource>` whose `sizeleft` is a decimal (a JSON
//! number, possibly fractional), and Sonarr lists one record per episode of a
//! download, each with the whole download's `sizeleft`.

use super::{rows, Import, ItemRef, Queued};
use crate::arr::history::HistoryEpisode;
use crate::capacity::App;
use crate::presence;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::Entry;
use std::collections::HashMap;

/// How far back imports count.
pub const IMPORT_WINDOW_SECS: u64 = 30 * 86_400;
/// How long one read of an app's imports serves.
pub const IMPORT_REFRESH_SECS: u64 = 6 * 3_600;

/// The `/api/v3/history/since` path and query for the imports of the window
/// ending at `now`. Sonarr names the season only on the embedded episode.
pub fn imports_path(app: App, now: u64) -> String {
    let since = crate::presence::format_utc(now.saturating_sub(IMPORT_WINDOW_SECS));
    let episode = match app {
        App::Radarr => "",
        App::Sonarr => "&includeEpisode=true",
    };
    format!("/api/v3/history/since?date={since}&eventType=3{episode}")
}

/// The `/api/v3/queue` path and query for one page; items the app cannot
/// place in its library are left out.
pub fn queue_path(app: App, page: usize, page_size: usize) -> String {
    let unknown = match app {
        App::Radarr => "includeUnknownMovieItems",
        App::Sonarr => "includeUnknownSeriesItems",
    };
    format!("/api/v3/queue?page={page}&pageSize={page_size}&{unknown}=false")
}

/// One imported `HistoryResource`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImportRecord {
    date: String,
    #[serde(default)]
    movie_id: Option<u64>,
    #[serde(default)]
    series_id: Option<u64>,
    #[serde(default)]
    episode: Option<HistoryEpisode>,
    data: ImportData,
}

#[derive(Deserialize)]
struct ImportData {
    size: String,
}

/// One `QueueResource`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueueRecord {
    id: u64,
    #[serde(default)]
    movie_id: Option<u64>,
    #[serde(default)]
    series_id: Option<u64>,
    #[serde(default)]
    season_number: Option<u32>,
    #[serde(default)]
    episode: Option<HistoryEpisode>,
    #[serde(default)]
    sizeleft: Option<f64>,
    #[serde(default)]
    download_id: Option<String>,
}

/// The imports in an app's `history/since` rows, one per file. A record that
/// names no item of the app, no date or no size is skipped.
pub fn parse_imports(app: App, records: Vec<serde_json::Value>) -> Vec<Import> {
    rows::<ImportRecord>(records)
        .filter_map(|record| {
            let item = item_ref(app, record.movie_id, record.series_id, record.episode.map(|episode| episode.season_number))?;
            let epoch = presence::parse_utc(&record.date)?;
            let bytes = record.data.size.trim().parse().ok()?;
            Some(Import { app, item, epoch, bytes })
        })
        .collect()
}

/// The downloads in an app's queue rows, one per download, with the bytes
/// still to come.
pub fn parse_queue(app: App, records: Vec<serde_json::Value>) -> Vec<Queued> {
    let queued = rows::<QueueRecord>(records).filter_map(|record| {
        let season = record.season_number.or_else(|| record.episode.map(|episode| episode.season_number));
        let item = item_ref(app, record.movie_id, record.series_id, season)?;
        // A float-to-int `as` saturates: negative reads as 0.
        let bytes_left = record.sizeleft.unwrap_or(0.0).floor() as u64;
        Some((download_key(record.download_id, record.id), Queued { app, item, bytes_left }))
    });
    by_download(queued)
}

/// The app's own item; 0 is the apps' "none".
fn item_ref(app: App, movie_id: Option<u64>, series_id: Option<u64>, season: Option<u32>) -> Option<ItemRef> {
    match app {
        App::Radarr => movie_id.filter(|id| *id > 0).map(ItemRef::Movie),
        App::Sonarr => series_id.filter(|id| *id > 0).map(|series_id| ItemRef::Series { series_id, season }),
    }
}

/// The download a record belongs to; a record without one stands alone.
fn download_key(download_id: Option<String>, record_id: u64) -> String {
    download_id.filter(|id| !id.is_empty()).unwrap_or_else(|| format!("record:{record_id}"))
}

/// Keep the first record of each download. A download spanning several
/// seasons of one show (a multi-season pack) names no single season.
fn by_download(entries: impl Iterator<Item = (String, Queued)>) -> Vec<Queued> {
    let mut index = HashMap::new();
    let mut kept: Vec<Queued> = Vec::new();
    for (key, entry) in entries {
        match index.entry(key) {
            Entry::Vacant(slot) => {
                slot.insert(kept.len());
                kept.push(entry);
            }
            Entry::Occupied(slot) => {
                if let Some(first) = kept.get_mut(*slot.get()) {
                    if let (ItemRef::Series { season, .. }, ItemRef::Series { season: other, .. }) = (&mut first.item, entry.item) {
                        if *season != other {
                            *season = None;
                        }
                    }
                }
            }
        }
    }
    kept
}

/// The import cache (`arr-imports.json`): each app's last read of the window,
/// kept independently so one app's outage never discards the other's read.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ImportCache {
    #[serde(default)]
    pub radarr: Option<ImportRead>,
    #[serde(default)]
    pub sonarr: Option<ImportRead>,
}

/// One read of an app's imports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportRead {
    /// When the imports were read, unix seconds.
    pub read_at: u64,
    pub imports: Vec<Import>,
}

impl ImportCache {
    pub fn slot(&mut self, app: App) -> &mut Option<ImportRead> {
        match app {
            App::Radarr => &mut self.radarr,
            App::Sonarr => &mut self.sonarr,
        }
    }

    /// Whether the app's cached read still serves at `now`. A read dated in
    /// the future (a clock step back) does not.
    pub fn is_fresh(&self, app: App, now: u64) -> bool {
        let read = match app {
            App::Radarr => &self.radarr,
            App::Sonarr => &self.sonarr,
        };
        read.as_ref().is_some_and(|read| read.read_at <= now && now - read.read_at < IMPORT_REFRESH_SECS)
    }

    /// Both apps' cached imports inside the window ending at `now`.
    pub fn imports(&self, now: u64) -> Vec<Import> {
        let since = now.saturating_sub(IMPORT_WINDOW_SECS);
        [&self.radarr, &self.sonarr]
            .into_iter()
            .flatten()
            .flat_map(|read| &read.imports)
            .filter(|import| import.epoch >= since)
            .cloned()
            .collect()
    }
}
