//! Capacity for one cycle: measure the library volumes, advance the eviction
//! ledger against them, decide per volume, and persist the latch.

use super::state_dir;
use flinch_archive::arr::{ArrMovie, ArrSeries};
use flinch_archive::capacity::{App, AppDisks, CapacityAction, EvictionLedger, LibraryVolumes, Occupancy, OnDisk, RecycleBin};
use flinch_archive::daemon::RuntimeSettings;
use flinch_archive::govern::{self, Governance};
use flinch_archive::ids::PlexIds;
use flinch_archive::{ArchiveCard, ArchivePolicy};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Where the eviction ledger lives; the caller writes it after the hand-off.
pub(super) fn ledger_path() -> std::path::PathBuf {
    state_dir().join("evictions.json")
}

/// Govern this cycle's capacity. Below the ceiling nothing is evicted; a
/// latched volume frees down to its release mark, crediting what recycle bins
/// still hold and what the disk never released. May arm never-played reclaim
/// on `policy` while evicting. Only items with Plex ids (`plex_ids`) count
/// toward a goal: nothing else can be handed over. Returns the governance and
/// the ledger, advanced to `now`.
pub(super) fn govern(
    disks: &[AppDisks],
    (movies, series): (&[ArrMovie], &[ArrSeries]),
    cards: &[ArchiveCard],
    plex_ids: &HashMap<String, PlexIds>,
    settings: &RuntimeSettings,
    policy: &mut ArchivePolicy,
    now: u64,
) -> (Governance, EvictionLedger) {
    let library = LibraryVolumes::build(disks);
    for (app, root) in &library.unmatched_roots {
        eprintln!("[flinch-arrd] capacity: {}:{root} is on no reported mount — its items are never evicted", app.label());
    }
    let volume_of = govern::volume_map(&library, movies, series);
    let latch_path = state_dir().join("capacity.json");
    let latch = flinch_archive::capacity::read_latch(&latch_path);
    // Library bytes per governed volume: what the disk holds that FLINCH can name.
    let mut library_bytes: BTreeMap<String, u64> = BTreeMap::new();
    for card in cards {
        if let Some(volume) = volume_of.get(&card.id) {
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
    // Evictions already handed over whose bytes a recycle bin (or anything
    // else) still holds are credited against the goals, so one gap is never
    // evicted twice.
    let mut ledger = EvictionLedger::read(&ledger_path());
    let present: HashSet<&str> = cards.iter().map(|card| card.id.as_str()).collect();
    ledger.observe(|id| present.contains(id), |app| library.recycle_secs(app), &measured, now);
    for app in [App::Radarr, App::Sonarr] {
        if library.recycle_bin(app) == RecycleBin::NeverEmptied {
            eprintln!(
                "[flinch-arrd] capacity: {} never empties its recycle bin — its deletes free no space until someone does",
                app.label()
            );
        }
    }
    let on_disk = OnDisk { library: library_bytes, credit: ledger.credits(), held: ledger.held() };
    let governance = govern::govern(library, volume_of, |id| plex_ids.contains_key(id), settings, &latch, on_disk, policy);
    if let Err(error) = flinch_archive::capacity::write_latch(&latch_path, &governance.decision.latch) {
        eprintln!("[flinch-arrd] capacity.json write failed: {error}");
    }
    log(&governance);
    (governance, ledger)
}

/// One line per governed volume, shaped for grep.
fn log(governance: &Governance) {
    if governance.invalid_watermarks {
        eprintln!("[flinch-arrd] capacity: watermarks invalid (need 0 < release <= ceiling <= 100%) — nothing is evicted");
        return;
    }
    let Some(snapshot) = &governance.snapshot else {
        println!("[flinch-arrd] capacity: unmeasured (no library volume reported) — nothing is evicted");
        return;
    };
    let gib = |bytes: u64| bytes as f64 / 1_073_741_824.0;
    for volume in &snapshot.volumes {
        let state = match governance.decision.goals.get(&volume.path) {
            Some(goal) => format!("evicting {:.1} GiB to reach {:.0}%", gib(*goal), snapshot.watermarks.release() * 100.0),
            None => format!("idle under the {:.0}% ceiling", snapshot.watermarks.ceiling() * 100.0),
        };
        let untracked = governance.on_disk.untracked(&volume.path, volume.used_bytes);
        let held = governance.on_disk.credit.get(&volume.path).map_or(0, |credit| credit.held);
        let held = if held > 0 { format!(" · {:.1} GiB evicted but never freed (held)", gib(held)) } else { String::new() };
        println!(
            "[flinch-arrd] capacity {}: {:.1}% of {:.1} GiB — {state} · {:.1} GiB here isn't library media{held}",
            volume.path,
            volume.utilization * 100.0,
            gib(volume.total_bytes),
            gib(untracked)
        );
    }
    if let CapacityAction::Evict { armed_never_played: true, .. } = governance.decision.action {
        println!("[flinch-arrd] capacity: never-played rule armed while evicting");
    }
}
