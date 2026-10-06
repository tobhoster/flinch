//! Capacity for one cycle: measure the library volumes, advance the eviction
//! ledger against them, and forecast each one.

use super::state_dir;
use flinch_archive::arr::{ArrMovie, ArrSeries};
use flinch_archive::capacity::{App, AppDisks, CapacityConfig, EvictionLedger, LibraryVolumes, Occupancy, OnDisk, RecycleBin};
use flinch_archive::govern::{self, Governance, Ingest};
use flinch_archive::signals::Signals;
use flinch_archive::ArchiveCard;
use std::collections::{BTreeMap, HashSet};

/// Where the eviction ledger lives; the caller writes it after the hand-off.
pub(super) fn ledger_path() -> std::path::PathBuf {
    state_dir().join("evictions.json")
}

/// Forecast this cycle's volumes, crediting what recycle bins still hold and
/// what the disk never released. Returns the governance and the ledger,
/// advanced to `now`.
pub(super) fn govern(
    disks: &[AppDisks],
    (movies, series): (&[ArrMovie], &[ArrSeries]),
    cards: &[ArchiveCard],
    signals: &Signals,
    config: &CapacityConfig,
    now: u64,
) -> (Governance, EvictionLedger) {
    let library = LibraryVolumes::build(disks);
    for (app, root) in &library.unmatched_roots {
        eprintln!("[flinch-arrd] capacity: {}:{root} is on no reported mount; its items are never evicted", app.label());
    }
    let located = govern::volume_map(&library, movies, series);
    // Library bytes per governed volume: what the disk holds that FLINCH can name.
    let mut library_bytes: BTreeMap<String, u64> = BTreeMap::new();
    for card in cards {
        if let Some(volume) = located.get(&card.id) {
            let bytes = library_bytes.entry(volume.clone()).or_insert(0);
            *bytes = bytes.saturating_add(card.size_bytes);
        }
    }
    let measured: BTreeMap<String, Occupancy> = library
        .volumes
        .iter()
        .map(|volume| {
            let named = library_bytes.get(&volume.path).copied().unwrap_or(0);
            (volume.path.clone(), Occupancy { used: volume.used_bytes(), library: named })
        })
        .collect();
    let mut ledger = EvictionLedger::read(&ledger_path());
    let present: HashSet<&str> = cards.iter().map(|card| card.id.as_str()).collect();
    ledger.observe(|id| present.contains(id), |app| library.recycle_secs(app), &measured, now);
    for app in [App::Radarr, App::Sonarr] {
        if library.recycle_bin(app) == RecycleBin::NeverEmptied {
            eprintln!(
                "[flinch-arrd] capacity: {} never empties its recycle bin; its deletes free no space until someone does",
                app.label()
            );
        }
    }
    let ingest = Ingest::attribute(&library, movies, series, signals, now);
    let on_disk = OnDisk { library: library_bytes, credit: ledger.credits(), held: ledger.held() };
    let governance = govern::govern(library, located, config, &ingest, on_disk);
    log(&governance);
    (governance, ledger)
}

/// One line per governed volume, shaped for grep.
fn log(governance: &Governance) {
    if governance.forecasts.is_empty() {
        println!("[flinch-arrd] capacity: unmeasured (no library volume reported); nothing is evicted");
        return;
    }
    let gib = |bytes: u64| bytes as f64 / 1_073_741_824.0;
    let window = governance.config.sliding_window_days;
    for row in &governance.forecasts {
        let forecast = &row.forecast;
        let state = match forecast.target_reclaim_bytes {
            0 => "healthy".to_string(),
            bytes => format!("free {:.1} GiB{}", gib(bytes), if forecast.is_emergency { " (emergency)" } else { "" }),
        };
        println!(
            "[flinch-arrd] capacity {}: {:.1}% of {:.1} GiB, +{:.1} GiB/day, {:.1} GiB queued, {:.1} GiB in {window} d: {state}",
            row.volume,
            forecast.current_utilization * 100.0,
            gib(forecast.max_capacity_bytes),
            gib(forecast.daily_ingest_rate_bytes),
            gib(forecast.queue_bytes),
            gib(forecast.projected_used_bytes),
        );
    }
}
