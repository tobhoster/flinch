//! Capacity governance — keep the media store under a ceiling by evicting only
//! when space is actually needed.
//!
//! The *arrs own the files and know the filesystems they write to. Each cycle
//! the daemon asks Radarr and Sonarr for `/api/v3/diskspace` (every mount in
//! the container) and `/api/v3/rootfolder` (where the library lives); this
//! module keeps only the filesystems that host a root folder, measures each
//! against the watermarks, and decides per volume: evict now or stay idle.
//!
//! Watermark governance (the Kubernetes image-GC high/low pattern):
//!
//! - **Below the ceiling, nothing is deleted.** A library under budget is doing
//!   its job; deleting a safe-looking item there buys nothing and risks the one
//!   mistake this reflex exists to prevent.
//! - **Crossing the ceiling latches eviction for that volume.** The plan frees
//!   the least expected regret per byte first until the volume is back at the
//!   *release* mark; the latch persists across runs until it gets there.
//!   Release below ceiling is the hysteresis — equal thresholds would chatter
//!   one item per finished download.
//! - **Per volume, never pooled.** Freeing the TV disk does not relieve a full
//!   movies disk, and a full `/config` mount is not the library's problem.
//! - **More aggressive only on measured evidence.** Unmeasured capacity evicts
//!   nothing and leaves the latch as it was: losing telemetry never deletes more.
//! - **Pressure widens the permitted set; it never lowers a floor.** While
//!   evicting, the operator may let the calibrated never-played rule contribute,
//!   still gated by its own P(safe) floor and dwell.
//! - **Space that never frees is reported, not chased.** Evicted bytes the
//!   disk has not released after the recycle window stay credited as *held*
//!   (see [`EvictionLedger`]), so a torrent seeding the same file cannot make
//!   FLINCH evict a second batch for one gap.

mod inflight;
mod status;
mod volumes;

pub use inflight::{
    Credit, Eviction, EvictionLedger, HandedOver, HeldEviction, Occupancy, HELD_CREDIT_SECS, SETTLE_GRACE_SECS, STALE_ON_DISK_SECS,
};
pub use status::{CapacityStatus, VolumeStatus};
pub use volumes::{App, AppDisks, LibraryVolumes, RecycleBin, RootFolder, Volume};

use crate::policy::ArchivePolicy;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The two thresholds, validated once: 0 < release ≤ ceiling ≤ 1.
///
/// Stored widened to f64 through the shortest decimal form: `f64::from(0.8f32)`
/// is 0.800000011920929…, which would put "exactly at the ceiling" a few KB
/// over it per TB and make byte accounting drift from what the operator typed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Watermarks {
    ceiling: f64,
    release: f64,
}

/// The f64 nearest to the decimal an operator wrote, not to its f32 bits.
fn widen(fraction: f32) -> f64 {
    fraction.to_string().parse().unwrap_or(f64::from(fraction))
}

impl Watermarks {
    /// `None` outside 0 < release ≤ ceiling ≤ 1 (NaN included): a malformed
    /// threshold means ungoverned, never a guessed number.
    pub fn new(ceiling: f32, release: f32) -> Option<Self> {
        let valid = ceiling.is_finite() && release.is_finite() && release > 0.0 && release <= ceiling && ceiling <= 1.0;
        valid.then(|| Self { ceiling: widen(ceiling), release: widen(release) })
    }

    pub fn ceiling(&self) -> f64 {
        self.ceiling
    }

    pub fn release(&self) -> f64 {
        self.release
    }
}

/// One volume measured against the watermarks.
#[derive(Debug, Clone, PartialEq)]
pub struct VolumeMeasure {
    pub path: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub ceiling_bytes: u64,
    pub release_bytes: u64,
    /// Bytes over the ceiling; 0 when under.
    pub deficit_bytes: u64,
    /// Bytes over the release mark: what a latched run frees.
    pub release_gap_bytes: u64,
    pub utilization: f32,
    pub over_ceiling: bool,
}

/// One measured moment of the library volumes.
#[derive(Debug, Clone, PartialEq)]
pub struct CapacitySnapshot {
    pub watermarks: Watermarks,
    pub volumes: Vec<VolumeMeasure>,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub ceiling_bytes: u64,
    pub release_bytes: u64,
    pub deficit_bytes: u64,
    pub release_gap_bytes: u64,
    pub utilization: f32,
    /// Any volume over its ceiling.
    pub over_ceiling: bool,
}

fn sum(values: impl Iterator<Item = u64>) -> u64 {
    values.fold(0u64, u64::saturating_add)
}

fn ratio(used: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        (used as f64 / total as f64) as f32
    }
}

impl CapacitySnapshot {
    /// `None` when there is nothing to measure: no library volume was found.
    pub fn of(volumes: &[Volume], watermarks: Watermarks) -> Option<Self> {
        if volumes.is_empty() {
            return None;
        }
        let measures: Vec<VolumeMeasure> = volumes
            .iter()
            .map(|v| {
                let deficit_bytes = v.excess_over(watermarks.ceiling);
                VolumeMeasure {
                    path: v.path.clone(),
                    total_bytes: v.total_bytes,
                    used_bytes: v.used_bytes(),
                    ceiling_bytes: v.budget(watermarks.ceiling),
                    release_bytes: v.budget(watermarks.release),
                    deficit_bytes,
                    release_gap_bytes: v.excess_over(watermarks.release),
                    utilization: ratio(v.used_bytes(), v.total_bytes),
                    over_ceiling: deficit_bytes > 0,
                }
            })
            .collect();
        let total_bytes = sum(measures.iter().map(|m| m.total_bytes));
        let used_bytes = sum(measures.iter().map(|m| m.used_bytes));
        Some(Self {
            watermarks,
            total_bytes,
            used_bytes,
            ceiling_bytes: sum(measures.iter().map(|m| m.ceiling_bytes)),
            release_bytes: sum(measures.iter().map(|m| m.release_bytes)),
            deficit_bytes: sum(measures.iter().map(|m| m.deficit_bytes)),
            release_gap_bytes: sum(measures.iter().map(|m| m.release_gap_bytes)),
            utilization: ratio(used_bytes, total_bytes),
            over_ceiling: measures.iter().any(|m| m.over_ceiling),
            volumes: measures,
        })
    }
}

/// What FLINCH can name on each volume beyond the measurement: its library
/// items, and the evictions a disk may still hold.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OnDisk {
    /// Bytes of the library items on each volume.
    pub library: BTreeMap<String, u64>,
    /// Evicted bytes each volume still credits against its goal.
    pub credit: BTreeMap<String, Credit>,
    /// Held evictions per volume (see [`EvictionLedger::held`]).
    pub held: BTreeMap<String, Vec<HeldEviction>>,
}

impl OnDisk {
    /// Every volume's credit, pending and held together: what goals subtract.
    pub fn credit_totals(&self) -> BTreeMap<String, u64> {
        self.credit.iter().map(|(volume, credit)| (volume.clone(), credit.total())).collect()
    }

    /// Bytes on `volume` that are neither library media nor an eviction
    /// FLINCH still credits: downloads, recycle bins of other deletions, and
    /// files no app tracks.
    pub fn untracked(&self, volume: &str, used: u64) -> u64 {
        let library = self.library.get(volume).copied().unwrap_or(0);
        let credit = self.credit.get(volume).map_or(0, Credit::total);
        used.saturating_sub(library.saturating_add(credit))
    }
}

/// The volumes whose eviction is latched, persisted across runs
/// (`state/capacity.json`). A missing or corrupt file reads as unlatched: the
/// failure mode deletes less, never more.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Latch {
    #[serde(default)]
    pub latched: BTreeSet<String>,
}

pub fn read_latch(path: &std::path::Path) -> Latch {
    std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

pub fn write_latch(path: &std::path::Path, latch: &Latch) -> std::io::Result<()> {
    crate::persist::replace(path, &serde_json::to_vec(latch)?)
}

/// What one cycle does about capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapacityAction {
    /// No library volume measured: nothing is evicted; the latch is kept.
    Unmeasured,
    /// No volume latched: nothing is evicted.
    Idle,
    /// At least one volume latched: free `goal_bytes` in total, per volume.
    Evict { goal_bytes: u64, armed_never_played: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapacityDecision {
    pub action: CapacityAction,
    /// The latch to persist for the next run.
    pub latch: Latch,
    /// Bytes each latched volume must free this run.
    pub goals: BTreeMap<String, u64>,
}

/// Decide one cycle: which volumes evict, and how much.
///
/// A volume latches when it crosses its ceiling and stays latched while it is
/// above its release mark — both judged on the measurement alone. Its goal is
/// the gap to the release mark minus `pending` (bytes already evicted that the
/// recycle bin still holds), so a latched volume waiting on its recycle bin
/// has a goal of 0 instead of evicting a second batch for the same gap. A
/// latched volume vanishing from the measurement (unmounted, renamed) drops
/// out of the latch: there is nothing to free on a disk that is not there.
/// While anything evicts and `arm_never_played` is set, the calibrated
/// never-played rule joins the permitted set.
pub fn decide_capacity(
    policy: &mut ArchivePolicy,
    snapshot: Option<&CapacitySnapshot>,
    before: &Latch,
    arm_never_played: bool,
    pending: &BTreeMap<String, u64>,
) -> CapacityDecision {
    let Some(snapshot) = snapshot else {
        return CapacityDecision { action: CapacityAction::Unmeasured, latch: before.clone(), goals: BTreeMap::new() };
    };
    // Every latched volume is a key, even at a goal of 0 (waiting on its
    // recycle bin): "latched" and "has a goal key" mean the same thing.
    let goals: BTreeMap<String, u64> = snapshot
        .volumes
        .iter()
        .filter(|m| m.over_ceiling || (before.latched.contains(&m.path) && m.release_gap_bytes > 0))
        .map(|m| {
            let credit = pending.get(&m.path).copied().unwrap_or(0);
            (m.path.clone(), m.release_gap_bytes.saturating_sub(credit))
        })
        .collect();
    let latch = Latch { latched: goals.keys().cloned().collect() };
    if goals.is_empty() {
        return CapacityDecision { action: CapacityAction::Idle, latch, goals };
    }
    if arm_never_played {
        policy.unwatched_reclaim.enabled = true;
    }
    CapacityDecision {
        action: CapacityAction::Evict { goal_bytes: sum(goals.values().copied()), armed_never_played: policy.unwatched_reclaim.enabled },
        latch,
        goals,
    }
}

#[cfg(test)]
mod tests;
