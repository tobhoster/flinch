//! When each item was on disk, from *arr history: `/api/v3/history` read once
//! a day, reduced to presence spans per card ([`flinch_archive::presence`])
//! and to the files removed lately ([`flinch_archive::outside`]), and cached
//! in `arr-history.json`. Every other cycle reads the cache. A failed read
//! keeps the cached spans: presence is never worth failing a cycle over.

use super::fetch::fetch_json;
use super::Args;
use anyhow::{Context, Result};
use flinch_archive::arr::history::{HistoryPage, HistoryRecord};
use flinch_archive::arr::{ArrMovie, ArrSeries};
use flinch_archive::capacity::EvictionLedger;
use flinch_archive::outside::{self, OutsideDeletion, Removal};
use flinch_archive::presence::{self, FileEvent, Span};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

/// How long a read of the history serves.
const REFRESH_SECS: u64 = 86_400;
const PAGE_SIZE: usize = 1_000;
/// Pages read per event type before older history is left out.
const PAGE_CAP: usize = 20;

/// The cache: each app's spans, refreshed independently, so one app's outage
/// never discards the other's read.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Cache {
    #[serde(default)]
    radarr: Option<AppHistory>,
    #[serde(default)]
    sonarr: Option<AppHistory>,
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
}

#[derive(Clone, Copy)]
enum App {
    Radarr,
    Sonarr,
}

impl App {
    fn name(self) -> &'static str {
        match self {
            App::Radarr => "radarr",
            App::Sonarr => "sonarr",
        }
    }

    /// Import and file-deletion event types (`MovieHistoryEventType` 3 and 6,
    /// `EpisodeHistoryEventType` 3 and 5), one query each: a single `eventType`
    /// filters on every API version; a repeated one is an array only on newer ones.
    fn event_types(self) -> [u32; 2] {
        match self {
            App::Radarr => [3, 6],
            App::Sonarr => [3, 5],
        }
    }

    /// Sonarr records name an episode; its season needs the episode itself.
    fn extra_query(self) -> &'static str {
        match self {
            App::Radarr => "",
            App::Sonarr => "&includeEpisode=true",
        }
    }

    fn endpoint(self, args: &Args) -> (&str, &str) {
        match self {
            App::Radarr => (args.radarr_url.trim_end_matches('/'), args.radarr_key.as_str()),
            App::Sonarr => (args.sonarr_url.trim_end_matches('/'), args.sonarr_key.as_str()),
        }
    }

    fn slot(self, cache: &mut Cache) -> &mut Option<AppHistory> {
        match self {
            App::Radarr => &mut cache.radarr,
            App::Sonarr => &mut cache.sonarr,
        }
    }
}

/// Fill `on_disk` on every movie and season, reading the history again when
/// the cached read is a day old, and return the files both apps removed
/// lately. Never fails the cycle.
pub(super) async fn attach(
    client: &reqwest::Client,
    args: &Args,
    movies: &mut [ArrMovie],
    series: &mut [ArrSeries],
) -> Vec<Removal> {
    let path = super::state_dir().join("arr-history.json");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let mut cache = read_cache(&path);
    let mut refreshed = Vec::new();
    for app in [App::Radarr, App::Sonarr] {
        let fresh = app.slot(&mut cache).as_ref().is_some_and(|history| {
            history.refreshed_at <= now && now - history.refreshed_at < REFRESH_SECS && history.removals.is_some()
        });
        if fresh {
            continue;
        }
        match read_history(client, args, app).await {
            Ok(records) => {
                let (history, summary) = spans_for(app, &records, movies, series, now);
                *app.slot(&mut cache) = Some(history);
                refreshed.push(summary);
            }
            Err(error) => eprintln!("[flinch-arrd] {} history unreadable, keeping the cached spans: {error:#}", app.name()),
        }
    }
    if !refreshed.is_empty() {
        println!("[flinch-arrd] arr history refreshed: {}", refreshed.join("; "));
        if let Err(error) = write_cache(&path, &cache) {
            eprintln!("[flinch-arrd] arr-history.json write failed, the history is read again next cycle: {error}");
        }
    }
    let spans = |history: &Option<AppHistory>, id: String| history.as_ref().and_then(|history| history.items.get(&id)).cloned().unwrap_or_default();
    for movie in movies.iter_mut() {
        movie.on_disk = spans(&cache.radarr, format!("radarr-{}", movie.id));
    }
    for show in series.iter_mut() {
        for season in &mut show.seasons {
            season.on_disk = spans(&cache.sonarr, format!("sonarr-{}-s{}", show.id, season.season_number));
        }
    }
    [cache.radarr, cache.sonarr].into_iter().flatten().flat_map(|history| history.removals.unwrap_or_default()).collect()
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

/// Every import and file-deletion record, newest first, paged. Records a page
/// boundary shifts onto the next page arrive twice and count once.
async fn read_history(client: &reqwest::Client, args: &Args, app: App) -> Result<Vec<HistoryRecord>> {
    let (base, key) = app.endpoint(args);
    let mut seen = HashSet::new();
    let mut records = Vec::new();
    for event_type in app.event_types() {
        for page in 1..=PAGE_CAP {
            let url = format!(
                "{base}/api/v3/history?page={page}&pageSize={PAGE_SIZE}&sortKey=date&sortDirection=descending&eventType={event_type}{}",
                app.extra_query()
            );
            let body: HistoryPage = serde_json::from_value(fetch_json(client, &url, key).await?).context("history page shape")?;
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
                    app.name()
                );
            }
        }
    }
    Ok(records)
}

/// Presence spans for the app's cards with files today, and the refresh
/// summary for the log.
fn spans_for(app: App, records: &[HistoryRecord], movies: &[ArrMovie], series: &[ArrSeries], now: u64) -> (AppHistory, String) {
    let mut events: HashMap<String, Vec<FileEvent>> = HashMap::new();
    for (card, event) in records.iter().filter_map(HistoryRecord::file_event) {
        events.entry(card).or_default().push(event);
    }
    // Each card on disk today, with today's file date.
    let cards: Vec<(String, Option<u64>)> = match app {
        App::Radarr => movies
            .iter()
            .filter_map(|movie| {
                let file_date = movie.movie_file.as_ref().and_then(|file| file.date_added.as_deref());
                Some((movie.to_card()?.id, file_date.and_then(presence::parse_utc)))
            })
            .collect(),
        App::Sonarr => series
            .iter()
            .flat_map(|show| {
                show.to_cards().into_iter().map(|card| {
                    let season = show.seasons.iter().find(|season| Some(season.season_number) == card.season_index);
                    (card.id, season.and_then(|season| season.files_added.as_deref()).and_then(presence::parse_utc))
                })
            })
            .collect(),
    };
    let (mut fallback, mut undated) = (0, 0);
    let mut items = BTreeMap::new();
    for (id, file_date) in cards {
        let found = presence::derive(events.remove(&id).unwrap_or_default(), file_date);
        fallback += usize::from(found.fallback);
        undated += found.undated;
        if !found.spans.is_empty() {
            items.insert(id, found.spans);
        }
    }
    let summary = format!(
        "{} {} record(s), {} item(s) with spans, {fallback} opened by the fallback, {undated} undatable",
        app.name(),
        records.len(),
        items.len()
    );
    let removals = records.iter().filter_map(HistoryRecord::removal).filter(|removal| now.saturating_sub(removal.at) <= outside::WINDOW_SECS).collect();
    (AppHistory { refreshed_at: now, records: records.len(), items, removals: Some(removals) }, summary)
}

/// A missing cache is a first run; an unreadable one is read again from the *arrs.
fn read_cache(path: &Path) -> Cache {
    let parsed = match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Cache::default(),
        Err(error) => Err(error.to_string()),
    };
    parsed.unwrap_or_else(|error| {
        eprintln!("[flinch-arrd] {} unreadable, reading the history again: {error}", path.display());
        Cache::default()
    })
}

fn write_cache(path: &Path, cache: &Cache) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    flinch_archive::persist::replace(path, &serde_json::to_vec(cache)?)
}
