//! When each item was on disk, from *arr history: `/api/v3/history` read once
//! a day per instance, reduced to presence spans per card
//! ([`flinch_archive::presence`]), to the files removed lately
//! ([`flinch_archive::outside`]) and to the download behind each file on disk
//! ([`flinch_archive::torrents`]), and cached in `arr-history.json`. Every
//! other cycle reads the cache. A failed read keeps the cached spans:
//! presence is never worth failing a cycle over.

use super::fetch::fetch_json;
use super::Args;
use anyhow::{Context, Result};
use flinch_archive::arr::history::{HistoryPage, HistoryRecord};
use flinch_archive::arr::instances::Connection;
use flinch_archive::arr::{ArrMovie, ArrSeries};
use flinch_archive::capacity::{App, EvictionLedger};
use flinch_archive::outside::{self, OutsideDeletion, Removal};
use flinch_archive::presence::{self, FileEvent, Span};
use std::collections::{BTreeMap, HashMap, HashSet};

/// How long a read of the history serves.
const REFRESH_SECS: u64 = 86_400;
const PAGE_SIZE: usize = 1_000;
/// Pages read per event type before older history is left out.
const PAGE_CAP: usize = 20;

/// The cache: each instance's spans, refreshed independently, so one
/// instance's outage never discards another's read. The default instances
/// keep the slots they always had, so an older cache reads on.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Cache {
    #[serde(default)]
    radarr: Option<AppHistory>,
    #[serde(default)]
    sonarr: Option<AppHistory>,
    /// Every other instance's, by instance key (`radarr@4k`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    extra: BTreeMap<String, AppHistory>,
}

impl Cache {
    fn slot(&self, arr: &Connection) -> Option<&AppHistory> {
        match (arr.app, arr.is_default()) {
            (App::Radarr, true) => self.radarr.as_ref(),
            (App::Sonarr, true) => self.sonarr.as_ref(),
            (_, false) => self.extra.get(&arr.key()),
        }
    }

    fn store(&mut self, arr: &Connection, history: AppHistory) {
        match (arr.app, arr.is_default()) {
            (App::Radarr, true) => self.radarr = Some(history),
            (App::Sonarr, true) => self.sonarr = Some(history),
            (_, false) => {
                self.extra.insert(arr.key(), history);
            }
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct AppHistory {
    /// When the history was read, unix seconds.
    refreshed_at: u64,
    /// Import and file-deletion records read.
    records: usize,
    /// Card id → presence spans, for cards with files at the read.
    items: BTreeMap<String, Vec<Span>>,
    /// Files removed within [`outside::WINDOW_SECS`] of the read. `None` in a
    /// cache written before removals were kept: it is read again.
    #[serde(default)]
    removals: Option<Vec<Removal>>,
    /// Card id → download ids of its files on disk (newest import each).
    /// `None` in a cache written before downloads were kept: read again.
    #[serde(default)]
    downloads: Option<BTreeMap<String, Vec<String>>>,
}

/// Import and file-deletion event types (`MovieHistoryEventType` 3 and 6,
/// `EpisodeHistoryEventType` 3 and 5), one query each: a single `eventType`
/// filters on every API version; a repeated one is an array only on newer ones.
fn event_types(app: App) -> [u32; 2] {
    match app {
        App::Radarr => [3, 6],
        App::Sonarr => [3, 5],
    }
}

/// Sonarr records name an episode; its season needs the episode itself.
fn extra_query(app: App) -> &'static str {
    match app {
        App::Radarr => "",
        App::Sonarr => "&includeEpisode=true",
    }
}

/// What the history says beyond presence: files every instance removed
/// lately, and card id → the download ids (torrent hashes) behind its files.
pub(super) struct Attached {
    pub(super) removals: Vec<Removal>,
    pub(super) downloads: BTreeMap<String, Vec<String>>,
}

/// Fill `on_disk` on every movie and season, reading the history again when
/// the cached read is a day old, and return what else it says. Never fails
/// the cycle.
pub(super) async fn attach(client: &reqwest::Client, args: &Args, movies: &mut [ArrMovie], series: &mut [ArrSeries]) -> Attached {
    let path = super::state_dir().join("arr-history.json");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let mut cache: Cache = super::read_state(&path);
    let mut refreshed = Vec::new();
    for arr in &args.arrs {
        let fresh = cache.slot(arr).is_some_and(|history| {
            history.refreshed_at <= now
                && now - history.refreshed_at < REFRESH_SECS
                && history.removals.is_some()
                && history.downloads.is_some()
        });
        if fresh {
            continue;
        }
        match read_history(client, arr).await {
            Ok(records) => {
                let (history, summary) = spans_for(arr, &records, movies, series, now);
                cache.store(arr, history);
                refreshed.push(summary);
            }
            Err(error) => eprintln!("[flinch-arrd] {} history unreadable, keeping the cached spans: {error:#}", arr.key()),
        }
    }
    // An instance no longer configured says nothing about today's library.
    cache.extra.retain(|key, _| args.arrs.iter().any(|arr| arr.key() == *key));
    if !refreshed.is_empty() {
        println!("[flinch-arrd] arr history refreshed: {}", refreshed.join("; "));
        super::write_state(&path, &cache);
    }
    // Card ids carry their instance, so every instance's spans share one map.
    let histories: Vec<&AppHistory> = cache.radarr.iter().chain(cache.sonarr.iter()).chain(cache.extra.values()).collect();
    let spans = |id: String| histories.iter().find_map(|history| history.items.get(&id)).cloned().unwrap_or_default();
    for movie in movies.iter_mut() {
        movie.on_disk = spans(movie.card_id());
    }
    for show in series.iter_mut() {
        let ids: Vec<String> = show.seasons.iter().map(|season| show.season_card_id(season.season_number)).collect();
        for (season, id) in show.seasons.iter_mut().zip(ids) {
            season.on_disk = spans(id);
        }
    }
    let (mut removals, mut downloads) = (Vec::new(), BTreeMap::new());
    for history in [cache.radarr, cache.sonarr].into_iter().flatten().chain(cache.extra.into_values()) {
        removals.extend(history.removals.unwrap_or_default());
        downloads.extend(history.downloads.unwrap_or_default());
    }
    Attached { removals, downloads }
}

/// The items whose files something other than FLINCH removed lately, logged
/// when any is still monitored with nothing on disk: it will download again.
pub(super) fn outside(
    removals: &[Removal],
    ledger: &EvictionLedger,
    (movies, series): (&[ArrMovie], &[ArrSeries]),
    now: u64,
) -> Vec<OutsideDeletion> {
    let listed = outside::outside_deletions(removals, &ledger.handoffs, movies, series, now);
    let returning = listed.iter().filter(|item| item.monitored == Some(true) && !item.on_disk).count();
    if !listed.is_empty() {
        println!(
            "[flinch-arrd] deleted outside FLINCH in the last {} days: {} item(s), {returning} still monitored with nothing on disk",
            outside::WINDOW_SECS / 86_400,
            listed.len()
        );
    }
    listed
}

/// Every import and file-deletion record of one instance, newest first,
/// paged. Records a page boundary shifts onto the next page arrive twice and
/// count once.
async fn read_history(client: &reqwest::Client, arr: &Connection) -> Result<Vec<HistoryRecord>> {
    let mut seen = HashSet::new();
    let mut records = Vec::new();
    for event_type in event_types(arr.app) {
        for page in 1..=PAGE_CAP {
            let url = format!(
                "{}/api/v3/history?page={page}&pageSize={PAGE_SIZE}&sortKey=date&sortDirection=descending&eventType={event_type}{}",
                arr.base,
                extra_query(arr.app)
            );
            let body: HistoryPage = serde_json::from_value(fetch_json(client, &url, &arr.key).await?).context("history page shape")?;
            let last = body.records.len() < PAGE_SIZE || (page * PAGE_SIZE) as u64 >= body.total_records;
            // A malformed row is one record lost, not the whole read.
            for record in body.records.into_iter().filter_map(|row| serde_json::from_value::<HistoryRecord>(row).ok()) {
                if seen.insert(record.id) {
                    records.push(record);
                }
            }
            if last {
                break;
            }
            if page == PAGE_CAP {
                eprintln!(
                    "[flinch-arrd] {} history: page cap reached for event type {event_type} ({PAGE_CAP} × {PAGE_SIZE}); older records are left out",
                    arr.key()
                );
            }
        }
    }
    Ok(records)
}

/// Presence spans for the instance's cards with files today, and the refresh
/// summary for the log.
fn spans_for(arr: &Connection, records: &[HistoryRecord], movies: &[ArrMovie], series: &[ArrSeries], now: u64) -> (AppHistory, String) {
    let instance = arr.name.as_str();
    let mut events: HashMap<String, Vec<FileEvent>> = HashMap::new();
    for (card, event) in records.iter().filter_map(|record| record.file_event(instance)) {
        events.entry(card).or_default().push(event);
    }
    // Each of the instance's cards on disk today, with today's file date.
    let cards: Vec<(String, Option<u64>)> = match arr.app {
        App::Radarr => movies
            .iter()
            .filter(|movie| movie.instance == instance)
            .filter_map(|movie| {
                let file_date = movie.movie_file.as_ref().and_then(|file| file.date_added.as_deref());
                Some((movie.to_card()?.id, file_date.and_then(presence::parse_utc)))
            })
            .collect(),
        App::Sonarr => series
            .iter()
            .filter(|show| show.instance == instance)
            .flat_map(|show| {
                show.to_cards().into_iter().map(|card| {
                    let season = show.seasons.iter().find(|season| Some(season.season_number) == card.season_index);
                    (card.id, season.and_then(|season| season.files_added.as_deref()).and_then(presence::parse_utc))
                })
            })
            .collect(),
    };
    let (mut fallback, mut undated) = (0, 0);
    let mut on_disk = HashSet::new();
    let mut items = BTreeMap::new();
    for (id, file_date) in cards {
        on_disk.insert(id.clone());
        let found = presence::derive(events.remove(&id).unwrap_or_default(), file_date);
        fallback += usize::from(found.fallback);
        undated += found.undated;
        if !found.spans.is_empty() {
            items.insert(id, found.spans);
        }
    }
    let summary = format!(
        "{} {} record(s), {} item(s) with spans, {fallback} opened by the fallback, {undated} undatable",
        arr.key(),
        records.len(),
        items.len()
    );
    let removals = records
        .iter()
        .filter_map(|record| record.removal(instance))
        .filter(|removal| now.saturating_sub(removal.at) <= outside::WINDOW_SECS)
        .collect();
    let downloads =
        flinch_archive::torrents::map::downloads(records, instance).into_iter().filter(|(card, _)| on_disk.contains(card)).collect();
    let history = AppHistory { refreshed_at: now, records: records.len(), items, removals: Some(removals), downloads: Some(downloads) };
    (history, summary)
}
