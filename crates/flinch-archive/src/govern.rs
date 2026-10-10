//! One cycle of capacity governance, from the *arrs' disks to each volume's
//! byte target.
//!
//! The daemon fetches; this module decides. It lives in the library so every
//! branch — unmeasured, healthy, short of space, an item on no governed disk —
//! is tested rather than trusted.

use crate::arr::{ArrMovie, ArrSeries};
use crate::capacity::{
    daily_series, App, CapacityConfig, CapacityStatus, CycleCapacity, LibraryVolumes, OnDisk, SlidingWindowCapacityForecaster,
    VolumeForecast, VolumeLoad, INGEST_HISTORY_DAYS,
};
use crate::plan::EvictionPlan;
use crate::signals::{ItemRef, Signals};
use std::collections::{BTreeMap, HashMap};

/// Card id → the library volume its files live on, attributed through the app
/// that owns the item (mount paths are container-local). Items whose path no
/// governed mount holds are absent: they can never be evicted.
pub fn volume_map(library: &LibraryVolumes, movies: &[ArrMovie], series: &[ArrSeries]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for movie in movies {
        if let Some(volume) = movie_volume(library, movie) {
            map.insert(movie.card_id(), volume.to_string());
        }
    }
    for show in series {
        if let Some(volume) = show_volume(library, show) {
            for season in &show.seasons {
                map.insert(show.season_card_id(season.season_number), volume.to_string());
            }
        }
    }
    map
}

pub fn movie_volume<'a>(library: &'a LibraryVolumes, movie: &ArrMovie) -> Option<&'a str> {
    movie.path.as_deref().and_then(|path| library.volume_of(App::Radarr, &movie.instance, path))
}

pub fn show_volume<'a>(library: &'a LibraryVolumes, show: &ArrSeries) -> Option<&'a str> {
    show.path.as_deref().and_then(|path| library.volume_of(App::Sonarr, &show.instance, path))
}

/// What is arriving on each volume: bytes imported per day (oldest first, see
/// [`crate::capacity::daily_series`]) and bytes still downloading.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ingest {
    pub daily: BTreeMap<String, Vec<u64>>,
    pub queue: BTreeMap<String, u64>,
}

impl Ingest {
    /// Attribute imports (the last [`INGEST_HISTORY_DAYS`]) and queued downloads
    /// to volumes through the *arr item each names. An item on no governed
    /// disk counts nowhere.
    pub fn attribute(library: &LibraryVolumes, movies: &[ArrMovie], series: &[ArrSeries], signals: &Signals, now: u64) -> Self {
        // *arr ids repeat across instances: an event joins by instance and id.
        let movies: HashMap<(&str, u32), &ArrMovie> = movies.iter().map(|movie| ((movie.instance.as_str(), movie.id), movie)).collect();
        let series: HashMap<(&str, u32), &ArrSeries> = series.iter().map(|show| ((show.instance.as_str(), show.id), show)).collect();
        let volume_of = |instance: &str, item: &ItemRef| -> Option<&str> {
            match item {
                ItemRef::Movie(id) => {
                    u32::try_from(*id).ok().and_then(|id| movies.get(&(instance, id))).and_then(|movie| movie_volume(library, movie))
                }
                ItemRef::Series { series_id, .. } => {
                    u32::try_from(*series_id).ok().and_then(|id| series.get(&(instance, id))).and_then(|show| show_volume(library, show))
                }
            }
        };
        let mut imports: BTreeMap<&str, Vec<(u64, u64)>> = BTreeMap::new();
        for import in &signals.imports {
            if let Some(volume) = volume_of(&import.instance, &import.item) {
                imports.entry(volume).or_default().push((import.epoch, import.bytes));
            }
        }
        let mut queue: BTreeMap<String, u64> = BTreeMap::new();
        for queued in &signals.queue {
            if let Some(volume) = volume_of(&queued.instance, &queued.item) {
                let bytes = queue.entry(volume.to_string()).or_insert(0);
                *bytes = bytes.saturating_add(queued.bytes_left);
            }
        }
        let daily =
            imports.into_iter().map(|(volume, events)| (volume.to_string(), daily_series(events, now, INGEST_HISTORY_DAYS))).collect();
        Self { daily, queue }
    }
}

/// Everything one cycle decided about capacity.
#[derive(Debug, Clone)]
pub struct Governance {
    pub library: LibraryVolumes,
    pub config: CapacityConfig,
    /// One per governed volume. Empty when unmeasured: no target, no eviction.
    pub forecasts: Vec<VolumeForecast>,
    /// Every item on a governed disk: for candidates, display and the ledger.
    pub located: HashMap<String, String>,
    /// Library bytes and still-credited evictions per volume (see
    /// [`crate::capacity::EvictionLedger`]): credits come off each projection.
    pub on_disk: OnDisk,
}

/// Forecast every governed volume. An invalid config (which settings
/// validation already refuses) forecasts nothing: unmeasured, never guessed.
pub fn govern(
    library: LibraryVolumes,
    located: HashMap<String, String>,
    config: &CapacityConfig,
    ingest: &Ingest,
    on_disk: OnDisk,
) -> Governance {
    let credits = on_disk.credit_totals();
    let forecasts = match SlidingWindowCapacityForecaster::new(config.clone()) {
        Ok(forecaster) => library
            .volumes
            .iter()
            .filter_map(|volume| {
                let load = VolumeLoad {
                    total_bytes: volume.total_bytes,
                    used_bytes: volume.used_bytes(),
                    daily_ingest: ingest.daily.get(&volume.path).map_or(&[], Vec::as_slice),
                    queue_bytes: ingest.queue.get(&volume.path).copied().unwrap_or(0),
                    in_flight_bytes: credits.get(&volume.path).copied().unwrap_or(0),
                };
                forecaster.forecast(&load).map(|forecast| VolumeForecast { volume: volume.path.clone(), forecast })
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    Governance { library, config: config.clone(), forecasts, located, on_disk }
}

impl Governance {
    /// The volume key an item's files live on, if a governed mount holds it.
    pub fn volume_for(&self, card_id: &str) -> Option<String> {
        self.located.get(card_id).cloned()
    }

    /// status.json's capacity block; `None` when unmeasured. `handed` lists
    /// every verified FLINCH collection member still on disk, with its bytes.
    pub fn status<'a>(&self, plan: &EvictionPlan, handed: impl IntoIterator<Item = (&'a str, u64)>) -> Option<CapacityStatus> {
        if self.forecasts.is_empty() {
            return None;
        }
        let mut per_volume: BTreeMap<String, u64> = BTreeMap::new();
        for (card_id, bytes) in handed {
            if let Some(volume) = self.located.get(card_id) {
                let total = per_volume.entry(volume.clone()).or_insert(0);
                *total = total.saturating_add(bytes);
            }
        }
        Some(CapacityStatus::new(CycleCapacity {
            config: &self.config,
            volumes: &self.library.volumes,
            forecasts: &self.forecasts,
            plan,
            unmatched_roots: &self.library.unmatched_roots,
            on_disk: &self.on_disk,
            handed: &per_volume,
        }))
    }
}

#[cfg(test)]
mod tests;
