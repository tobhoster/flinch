//! What Radarr and Sonarr grabbed lately and what they are still downloading.
//!
//! Wire shape, from the Radarr and Sonarr sources (`develop`): `GET
//! /api/v3/history/since?date=&eventType=1` answers a bare `HistoryResource`
//! array, not a page; event type 1 is `grabbed` in both apps. A record's
//! `data` is a `Dictionary<string, string>`, so `data.size` is the release's
//! byte count as a string. Sonarr writes one grabbed record per episode of a
//! release, each with the whole release's size and the same `downloadId`.
//! `GET /api/v3/queue` answers a `PagingResource<QueueResource>` whose
//! `sizeleft` is a decimal (a JSON number, possibly fractional), and Sonarr
//! again lists one record per episode of a download.

use super::{rows, Grab, ItemRef, Queued};
use crate::arr::history::HistoryEpisode;
use crate::capacity::App;
use crate::presence;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::Entry;
use std::collections::HashMap;

/// How far back grabs count.
pub const GRAB_WINDOW_SECS: u64 = 30 * 86_400;
/// How long one read of an app's grabs serves.
pub const GRAB_REFRESH_SECS: u64 = 6 * 3_600;

/// The `/api/v3/history/since` path and query for the grabs of the window
/// ending at `now`. Sonarr names the season only on the embedded episode.
pub fn grabs_path(app: App, now: u64) -> String {
    let since = crate::presence::format_utc(now.saturating_sub(GRAB_WINDOW_SECS));
    let episode = match app {
        App::Radarr => "",
        App::Sonarr => "&includeEpisode=true",
    };
    format!("/api/v3/history/since?date={since}&eventType=1{episode}")
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

/// One grabbed `HistoryResource`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GrabRecord {
    id: u64,
    date: String,
    #[serde(default)]
    movie_id: Option<u64>,
    #[serde(default)]
    series_id: Option<u64>,
    #[serde(default)]
    episode: Option<HistoryEpisode>,
    #[serde(default)]
    download_id: Option<String>,
    data: GrabData,
}

#[derive(Deserialize)]
struct GrabData {
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

/// The grabs in an app's `history/since` rows, one per download. A record
/// that names no item of the app, no date or no size is skipped.
pub fn parse_grabs(app: App, records: Vec<serde_json::Value>) -> Vec<Grab> {
    let grabs = rows::<GrabRecord>(records).filter_map(|record| {
        let item = item_ref(app, record.movie_id, record.series_id, record.episode.map(|episode| episode.season_number))?;
        let epoch = presence::parse_utc(&record.date)?;
        let bytes = record.data.size.trim().parse().ok()?;
        Some((download_key(record.download_id, record.id), Grab { app, item, epoch, bytes }))
    });
    by_download(grabs, |grab| &mut grab.item)
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
    by_download(queued, |queued| &mut queued.item)
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

/// Keep the first entry of each download. A download spanning several
/// seasons of one show (a multi-season pack) names no single season.
fn by_download<T>(entries: impl Iterator<Item = (String, T)>, item: fn(&mut T) -> &mut ItemRef) -> Vec<T> {
    let mut index = HashMap::new();
    let mut kept: Vec<T> = Vec::new();
    for (key, mut entry) in entries {
        match index.entry(key) {
            Entry::Vacant(slot) => {
                slot.insert(kept.len());
                kept.push(entry);
            }
            Entry::Occupied(slot) => {
                if let Some(first) = kept.get_mut(*slot.get()) {
                    if let (ItemRef::Series { season, .. }, ItemRef::Series { season: other, .. }) = (item(first), *item(&mut entry)) {
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

/// The grab cache (`arr-grabs.json`): each app's last read of the window,
/// kept independently so one app's outage never discards the other's read.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct GrabCache {
    #[serde(default)]
    pub radarr: Option<GrabRead>,
    #[serde(default)]
    pub sonarr: Option<GrabRead>,
}

/// One read of an app's grabs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrabRead {
    /// When the grabs were read, unix seconds.
    pub read_at: u64,
    pub grabs: Vec<Grab>,
}

impl GrabCache {
    pub fn slot(&mut self, app: App) -> &mut Option<GrabRead> {
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
        read.as_ref().is_some_and(|read| read.read_at <= now && now - read.read_at < GRAB_REFRESH_SECS)
    }

    /// Both apps' cached grabs inside the window ending at `now`.
    pub fn grabs(&self, now: u64) -> Vec<Grab> {
        let since = now.saturating_sub(GRAB_WINDOW_SECS);
        [&self.radarr, &self.sonarr]
            .into_iter()
            .flatten()
            .flat_map(|read| &read.grabs)
            .filter(|grab| grab.epoch >= since)
            .cloned()
            .collect()
    }
}
