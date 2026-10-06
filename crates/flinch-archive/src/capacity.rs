//! Capacity governance — forecast each library volume a window ahead and ask
//! the planner to free only what that forecast says will not fit.
//!
//! The *arrs own the files and know the filesystems they write to. Each cycle
//! the daemon asks Radarr and Sonarr for `/api/v3/diskspace` (every mount in
//! the container) and `/api/v3/rootfolder` (where the library lives); this
//! module keeps only the filesystems that host a root folder and forecasts
//! each one (see [`SlidingWindowCapacityForecaster`]):
//!
//! ```text
//! v̂        = EWMA_α(bytes imported per day, last 30 days)
//! U_proj   = U + v̂·W + queued bytes − evictions not yet freed
//! B_target = max(0, U_proj − θ_target·C_max + headroom)
//! ```
//!
//! - **Stateless.** Every cycle forecasts from the measurement and the logs;
//!   nothing latches, so no stored state can disagree with the disk.
//! - **Per volume, never pooled.** Freeing the TV disk does not relieve a full
//!   movies disk, and a full `/config` mount is not the library's problem.
//! - **Unmeasured evicts nothing.** No library volume, no forecast, no target:
//!   losing telemetry never deletes more.
//! - **Space that never frees is reported, not chased.** Evicted bytes the
//!   disk has not released after the recycle window stay credited (see
//!   [`EvictionLedger`]) and come off the projection, so a torrent seeding the
//!   same file cannot make FLINCH evict a second batch for one gap.

mod inflight;
mod status;
mod volumes;

pub use inflight::{
    Credit, Eviction, EvictionLedger, HandedOver, HeldEviction, Occupancy, HELD_CREDIT_SECS, SETTLE_GRACE_SECS, STALE_ON_DISK_SECS,
};
pub use status::{CapacityStatus, CycleCapacity, VolumeStatus};
pub use volumes::{App, AppDisks, LibraryVolumes, RecycleBin, RootFolder, Volume};

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const GIB: u64 = 1 << 30;
const DAY_SECS: u64 = 86_400;

/// How many days of ingest the velocity is smoothed over.
pub const INGEST_HISTORY_DAYS: usize = 30;

/// The forecaster's knobs, as the operator sets them (`settings.json`
/// `capacity`). Every field defaults individually.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CapacityConfig {
    /// Caps each volume's capacity (`min` with its measured size), for a share
    /// whose quota is smaller than the disk. `None`: the measured size.
    pub max_capacity_bytes: Option<u64>,
    /// θ_target: the fraction of capacity the projection must stay under.
    pub target_utilization: f64,
    /// θ_emerg: at or above this *current* fraction the planner skips the
    /// solver for the greedy pass.
    pub emergency_utilization: f64,
    /// W: how many days ahead the projection looks.
    pub sliding_window_days: u32,
    /// α: weight of the newest day in the ingest average.
    pub ewma_alpha: f64,
    /// Extra bytes kept free below the target.
    pub headroom_buffer_bytes: u64,
}

impl Default for CapacityConfig {
    fn default() -> Self {
        Self {
            max_capacity_bytes: None,
            target_utilization: 0.80,
            emergency_utilization: 0.95,
            sliding_window_days: 14,
            ewma_alpha: 0.2,
            headroom_buffer_bytes: 50 * GIB,
        }
    }
}

/// A [`CapacityConfig`] outside its bounds; the message names the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidCapacityConfig(pub &'static str);

impl CapacityConfig {
    pub fn validate(&self) -> Result<(), InvalidCapacityConfig> {
        let fraction = |value: f64| value.is_finite() && value > 0.0 && value <= 1.0;
        if !(fraction(self.target_utilization) && fraction(self.emergency_utilization)) {
            return Err(InvalidCapacityConfig("the target and emergency utilization must be between 0 and 1"));
        }
        if self.target_utilization >= self.emergency_utilization {
            return Err(InvalidCapacityConfig("the target utilization must be below the emergency utilization"));
        }
        if !(1..=365).contains(&self.sliding_window_days) {
            return Err(InvalidCapacityConfig("the forecast window must be 1 to 365 days"));
        }
        if !fraction(self.ewma_alpha) {
            return Err(InvalidCapacityConfig("the smoothing factor (ewma_alpha) must be above 0 and at most 1"));
        }
        if self.max_capacity_bytes == Some(0) {
            return Err(InvalidCapacityConfig("the maximum capacity must be above 0 when set"));
        }
        Ok(())
    }
}

/// One volume's inputs for a forecast.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VolumeLoad<'a> {
    pub total_bytes: u64,
    pub used_bytes: u64,
    /// Bytes imported per day, oldest first, one entry per day.
    pub daily_ingest: &'a [u64],
    /// Bytes still to download for items on this volume.
    pub queue_bytes: u64,
    /// Bytes evicted and credited but not yet freed (see [`EvictionLedger`]).
    pub in_flight_bytes: u64,
}

/// One volume, forecast.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapacityForecast {
    pub current_used_bytes: u64,
    pub max_capacity_bytes: u64,
    pub current_utilization: f64,
    pub daily_ingest_rate_bytes: u64,
    pub queue_bytes: u64,
    pub in_flight_bytes: u64,
    pub projected_used_bytes: u64,
    /// B_target: what this volume must free.
    pub target_reclaim_bytes: u64,
    pub is_emergency: bool,
}

/// A forecast with the volume it belongs to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VolumeForecast {
    pub volume: String,
    #[serde(flatten)]
    pub forecast: CapacityForecast,
}

/// The stateless forecaster: the same inputs always give the same target.
#[derive(Debug, Clone, PartialEq)]
pub struct SlidingWindowCapacityForecaster {
    config: CapacityConfig,
}

impl SlidingWindowCapacityForecaster {
    pub fn new(config: CapacityConfig) -> Result<Self, InvalidCapacityConfig> {
        config.validate()?;
        Ok(Self { config })
    }

    pub fn config(&self) -> &CapacityConfig {
        &self.config
    }

    /// `None` for a volume with no capacity: nothing to measure against.
    pub fn forecast(&self, load: &VolumeLoad) -> Option<CapacityForecast> {
        let config = &self.config;
        let capacity = config.max_capacity_bytes.map_or(load.total_bytes, |cap| cap.min(load.total_bytes));
        if capacity == 0 {
            return None;
        }
        let velocity = ewma(config.ewma_alpha, load.daily_ingest);
        let projected = load.used_bytes as f64 + velocity * f64::from(config.sliding_window_days) + load.queue_bytes as f64
            - load.in_flight_bytes as f64;
        let projected = projected.max(0.0);
        let safe = config.target_utilization * capacity as f64;
        let target = (projected - safe + config.headroom_buffer_bytes as f64).max(0.0);
        let utilization = load.used_bytes as f64 / capacity as f64;
        Some(CapacityForecast {
            current_used_bytes: load.used_bytes,
            max_capacity_bytes: capacity,
            current_utilization: utilization,
            daily_ingest_rate_bytes: velocity.round() as u64,
            queue_bytes: load.queue_bytes,
            in_flight_bytes: load.in_flight_bytes,
            projected_used_bytes: projected.round() as u64,
            target_reclaim_bytes: target.ceil() as u64,
            is_emergency: utilization >= config.emergency_utilization,
        })
    }
}

/// Exponentially weighted moving average, oldest first, seeded with the first
/// day: `v = α·x + (1 − α)·v`. 0 for an empty series.
pub fn ewma(alpha: f64, series: &[u64]) -> f64 {
    let mut values = series.iter().map(|bytes| *bytes as f64);
    let Some(first) = values.next() else { return 0.0 };
    values.fold(first, |average, today| alpha * today + (1.0 - alpha) * average)
}

/// Bytes per day over the `days` 24-hour windows ending at `now`, oldest
/// first. Events in the future or older than the window are ignored.
pub fn daily_series(events: impl IntoIterator<Item = (u64, u64)>, now: u64, days: usize) -> Vec<u64> {
    let mut series = vec![0u64; days];
    for (epoch, bytes) in events {
        let Some(age) = now.checked_sub(epoch) else { continue };
        let back = (age / DAY_SECS) as usize;
        if let Some(slot) = back.checked_add(1).and_then(|back| days.checked_sub(back)) {
            series[slot] = series[slot].saturating_add(bytes);
        }
    }
    series
}

fn sum(values: impl Iterator<Item = u64>) -> u64 {
    values.fold(0u64, u64::saturating_add)
}

fn ratio(used: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        used as f64 / total as f64
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

#[cfg(test)]
mod tests;
