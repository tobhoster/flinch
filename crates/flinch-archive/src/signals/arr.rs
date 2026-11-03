//! What Radarr and Sonarr imported lately and what they are still downloading.
//!
//! Wire shape, from the Radarr and Sonarr sources (`develop`): `GET
//! /api/v3/history/since?date=&eventType=3` answers a bare `HistoryResource`
//! array, not a page; event type 3 is the import (`downloadFolderImported`)
//! and 1 the grab in both apps. A record's `data` is a
//! `Dictionary<string, string>`, so `data.size` is the file's byte count as a
//! string. Sonarr writes one import record per episode, each with that
//! episode's own file size, so a season pack is the sum of its records; an
//! import and the grab of its download share the `downloadId`. `GET
//! /api/v3/queue` answers a `PagingResource<QueueResource>` whose `size` and
//! `sizeleft` are decimals (JSON numbers, possibly fractional), and Sonarr
//! lists one record per episode of a download, each with the whole
//! download's. A download complete but not imported has a `sizeleft` of 0.

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

/// The start of the window ending at `now`, as `history/since` takes it.
fn window_start(now: u64) -> String {
    presence::format_utc(now.saturating_sub(IMPORT_WINDOW_SECS))
}

/// The `/api/v3/history/since` path and query for the imports of the window
/// ending at `now`. Sonarr names the season only on the embedded episode.
pub fn imports_path(app: App, now: u64) -> String {
    let episode = match app {
        App::Radarr => "",
        App::Sonarr => "&includeEpisode=true",
    };
    format!("/api/v3/history/since?date={}&eventType=3{episode}", window_start(now))
}

/// The `/api/v3/history/since` path and query for the grabs of the window
/// ending at `now`: when each import's download was grabbed.
pub fn grabs_path(now: u64) -> String {
    format!("/api/v3/history/since?date={}&eventType=1", window_start(now))
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
    #[serde(default)]
    download_id: Option<String>,
    data: ImportData,
}

/// One grabbed `HistoryResource`: when, and which download.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GrabRecord {
    date: String,
    download_id: String,
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
    size: Option<f64>,
    #[serde(default)]
    sizeleft: Option<f64>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    tracked_download_state: Option<String>,
    #[serde(default)]
    download_id: Option<String>,
}

impl QueueRecord {
    /// What the download will still land on its volume. Complete but not
    /// imported (waiting for space, blocked, importing, or finished in the
    /// client and not yet picked up), nothing is left to download and the
    /// whole `size` is still to land: seen live, ten episodes held for days
    /// on full TV disks counted as nothing. Otherwise, what is left.
    fn landing_bytes(&self) -> u64 {
        let left = self.sizeleft.unwrap_or(0.0);
        let awaiting_import = match self.tracked_download_state.as_deref() {
            Some("importPending" | "importBlocked" | "importing") => true,
            Some("downloading") | None => self.status.as_deref() == Some("completed") && left <= 0.0,
            // Imported, failed or ignored: the app lands nothing more of it.
            Some(_) => false,
        };
        // A float-to-int `as` saturates: negative or NaN reads as 0.
        if awaiting_import {
            self.size.unwrap_or(0.0).floor() as u64
        } else {
            left.floor() as u64
        }
    }
}

/// The imports in an instance's `history/since` rows, one per file, each with
/// the grab of its download from the instance's grab rows. A record that
/// names no item of the app, no date or no size is skipped.
pub fn parse_imports(app: App, instance: &str, imports: Vec<serde_json::Value>, grabs: Vec<serde_json::Value>) -> Vec<Import> {
    let grabs = grab_times(grabs);
    rows::<ImportRecord>(imports)
        .filter_map(|record| {
            let item = item_ref(app, record.movie_id, record.series_id, record.episode.map(|episode| episode.season_number))?;
            let epoch = presence::parse_utc(&record.date)?;
            let bytes = record.data.size.trim().parse().ok()?;
            let grabbed_at =
                record.download_id.and_then(|download| grabs.get(&download)?.iter().copied().filter(|grabbed| *grabbed <= epoch).max());
            Some(Import { app, instance: instance.to_string(), item, epoch, bytes, grabbed_at })
        })
        .collect()
}

/// Download id → when it was grabbed. Sonarr writes one grab record per
/// episode of a download, and a torrent grabbed again keeps its hash, so a
/// download can carry several times; an import joins the last before it.
fn grab_times(records: Vec<serde_json::Value>) -> HashMap<String, Vec<u64>> {
    let mut times: HashMap<String, Vec<u64>> = HashMap::new();
    for grab in rows::<GrabRecord>(records).filter(|grab| !grab.download_id.is_empty()) {
        if let Some(at) = presence::parse_utc(&grab.date) {
            times.entry(grab.download_id).or_default().push(at);
        }
    }
    times
}

/// The downloads in an instance's queue rows, one per download, with the
/// bytes each will still land.
pub fn parse_queue(app: App, instance: &str, records: Vec<serde_json::Value>) -> Vec<Queued> {
    let queued = rows::<QueueRecord>(records).filter_map(|record| {
        let bytes = record.landing_bytes();
        let season = record.season_number.or_else(|| record.episode.map(|episode| episode.season_number));
        let item = item_ref(app, record.movie_id, record.series_id, season)?;
        Some((download_key(record.download_id, record.id), Queued { app, instance: instance.to_string(), item, bytes }))
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

/// The import cache (`arr-imports.json`): each instance's last read of the
/// window, kept independently so one instance's outage never discards
/// another's read. The default instances keep the slots they always had, so
/// an older cache reads on.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ImportCache {
    #[serde(default)]
    pub radarr: Option<ImportRead>,
    #[serde(default)]
    pub sonarr: Option<ImportRead>,
    /// Every other instance's read, by [`crate::ids::instance_key`].
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub extra: std::collections::BTreeMap<String, ImportRead>,
}

/// One read of an app's imports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportRead {
    /// When the imports were read, unix seconds.
    pub read_at: u64,
    /// Whether the read took in the grabs too, so each import carries
    /// [`Import::grabbed_at`]. A read from before FLINCH read grabs did not.
    #[serde(default)]
    pub with_grabs: bool,
    pub imports: Vec<Import>,
}

impl ImportCache {
    /// The instance's cached read.
    pub fn read(&self, app: App, instance: &str) -> Option<&ImportRead> {
        match (app, instance.is_empty()) {
            (App::Radarr, true) => self.radarr.as_ref(),
            (App::Sonarr, true) => self.sonarr.as_ref(),
            (_, false) => self.extra.get(&crate::ids::instance_key(app, instance)),
        }
    }

    /// Replace the instance's cached read.
    pub fn store(&mut self, app: App, instance: &str, read: ImportRead) {
        match (app, instance.is_empty()) {
            (App::Radarr, true) => self.radarr = Some(read),
            (App::Sonarr, true) => self.sonarr = Some(read),
            (_, false) => {
                self.extra.insert(crate::ids::instance_key(app, instance), read);
            }
        }
    }

    /// Whether the instance's cached read still serves at `now`. A read dated
    /// in the future (a clock step back) does not, nor one without the grabs:
    /// read again at once, its backlog stops counting as arrival.
    pub fn is_fresh(&self, app: App, instance: &str, now: u64) -> bool {
        self.read(app, instance).is_some_and(|read| read.with_grabs && read.read_at <= now && now - read.read_at < IMPORT_REFRESH_SECS)
    }

    /// Every instance's cached imports inside the window ending at `now`.
    pub fn imports(&self, now: u64) -> Vec<Import> {
        let since = now.saturating_sub(IMPORT_WINDOW_SECS);
        [self.radarr.as_ref(), self.sonarr.as_ref()]
            .into_iter()
            .flatten()
            .chain(self.extra.values())
            .flat_map(|read| &read.imports)
            .filter(|import| import.epoch >= since)
            .cloned()
            .collect()
    }
}
