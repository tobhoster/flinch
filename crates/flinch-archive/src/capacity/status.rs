//! The capacity block of status.json: each volume's forecast against what the
//! plan takes there. Two questions stay apart: whether the plan *covers* a
//! target (the eligible set is big enough) and whether the target is *met*
//! (FLINCH has handed that much over).

use super::{ratio, sum, App, CapacityConfig, HeldEviction, OnDisk, Volume, VolumeForecast};
use crate::plan::knapsack::Method;
use crate::plan::EvictionPlan;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One volume as status.json reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VolumeStatus {
    pub path: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub utilization: f64,
    /// C_max the forecast used: the measured size, or the operator's cap.
    pub capacity_bytes: u64,
    pub daily_ingest_bytes: u64,
    /// Bytes still to download for items on this volume.
    pub queue_bytes: u64,
    pub projected_used_bytes: u64,
    /// B_target: what this volume must free.
    pub target_reclaim_bytes: u64,
    pub emergency: bool,
    pub planned_bytes: u64,
    /// Everything selectable on this volume.
    pub eligible_bytes: u64,
    /// Evicted bytes the recycle bin may still hold.
    pub pending_bytes: u64,
    /// Evicted bytes past the bin's window the disk never released.
    pub held_bytes: u64,
    pub held: Vec<HeldEviction>,
    pub library_bytes: u64,
    /// Used bytes that are neither library media nor a credited eviction.
    pub untracked_bytes: u64,
    /// Bytes handed to Maintainerr and still on disk.
    pub handed_bytes: u64,
    /// The plan covers the target. `None` while the volume is healthy.
    pub covered: Option<bool>,
    /// What is handed over covers the target. `None` while healthy.
    pub goal_met: Option<bool>,
}

/// The operator-facing view of one measured cycle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapacityStatus {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub utilization: f64,
    pub target_utilization: f64,
    pub emergency_utilization: f64,
    pub window_days: u32,
    pub headroom_bytes: u64,
    pub target_reclaim_bytes: u64,
    /// Every volume's forecast fits: nothing needs to go.
    pub healthy: bool,
    pub emergency: bool,
    pub planned_bytes: u64,
    pub eligible_bytes: u64,
    pub covered: Option<bool>,
    pub goal_met: Option<bool>,
    /// How the plan was chosen; `None` when healthy.
    pub method: Option<Method>,
    pub solver_error: Option<String>,
    pub total_regret: f64,
    pub pending_bytes: u64,
    pub held_bytes: u64,
    pub untracked_bytes: u64,
    pub handed_bytes: u64,
    /// "app:path" root folders no mount holds: items there are never evicted.
    pub unmatched_roots: Vec<String>,
    pub volumes: Vec<VolumeStatus>,
}

/// What one cycle decided, as the capacity block reports it.
#[derive(Debug, Clone, Copy)]
pub struct CycleCapacity<'a> {
    pub config: &'a CapacityConfig,
    pub volumes: &'a [Volume],
    pub forecasts: &'a [VolumeForecast],
    pub plan: &'a EvictionPlan,
    pub unmatched_roots: &'a [(App, String)],
    pub on_disk: &'a OnDisk,
    /// Handed-over bytes still on disk, per volume.
    pub handed: &'a BTreeMap<String, u64>,
}

impl CapacityStatus {
    pub fn new(cycle: CycleCapacity<'_>) -> Self {
        let CycleCapacity { config, volumes, forecasts, plan, unmatched_roots, on_disk, handed } = cycle;
        let rows: Vec<VolumeStatus> = volumes
            .iter()
            .filter_map(|volume| {
                let forecast = &forecasts.iter().find(|f| f.volume == volume.path)?.forecast;
                let outcome = plan.volumes.iter().find(|o| o.volume == volume.path);
                let used = volume.used_bytes();
                let credit = on_disk.credit.get(&volume.path).copied().unwrap_or_default();
                let target = forecast.target_reclaim_bytes;
                let planned_bytes = outcome.map_or(0, |o| o.planned_bytes);
                let handed_bytes = handed.get(&volume.path).copied().unwrap_or(0);
                let evicting = target > 0;
                Some(VolumeStatus {
                    path: volume.path.clone(),
                    total_bytes: volume.total_bytes,
                    used_bytes: used,
                    utilization: ratio(used, volume.total_bytes),
                    capacity_bytes: forecast.max_capacity_bytes,
                    daily_ingest_bytes: forecast.daily_ingest_rate_bytes,
                    queue_bytes: forecast.queue_bytes,
                    projected_used_bytes: forecast.projected_used_bytes,
                    target_reclaim_bytes: target,
                    emergency: forecast.is_emergency,
                    planned_bytes,
                    eligible_bytes: outcome.map_or(0, |o| o.eligible_bytes),
                    pending_bytes: credit.pending,
                    held_bytes: credit.held,
                    held: on_disk.held.get(&volume.path).cloned().unwrap_or_default(),
                    library_bytes: on_disk.library.get(&volume.path).copied().unwrap_or(0),
                    untracked_bytes: on_disk.untracked(&volume.path, used),
                    handed_bytes,
                    covered: evicting.then_some(planned_bytes >= target),
                    goal_met: evicting.then_some(handed_bytes >= target),
                })
            })
            .collect();
        let evicting: Vec<&VolumeStatus> = rows.iter().filter(|v| v.target_reclaim_bytes > 0).collect();
        let every =
            |flag: fn(&VolumeStatus) -> Option<bool>| (!evicting.is_empty()).then(|| evicting.iter().all(|v| flag(v) == Some(true)));
        let total = |bytes: fn(&VolumeStatus) -> u64| sum(rows.iter().map(bytes));
        let (total_bytes, used_bytes) = (total(|v| v.total_bytes), total(|v| v.used_bytes));
        Self {
            total_bytes,
            used_bytes,
            utilization: ratio(used_bytes, total_bytes),
            target_utilization: config.target_utilization,
            emergency_utilization: config.emergency_utilization,
            window_days: config.sliding_window_days,
            headroom_bytes: config.headroom_buffer_bytes,
            target_reclaim_bytes: total(|v| v.target_reclaim_bytes),
            healthy: evicting.is_empty(),
            emergency: rows.iter().any(|v| v.emergency),
            planned_bytes: total(|v| v.planned_bytes),
            eligible_bytes: total(|v| v.eligible_bytes),
            covered: every(|v| v.covered),
            goal_met: every(|v| v.goal_met),
            method: plan.method,
            solver_error: plan.solver_error.clone(),
            total_regret: plan.total_regret,
            pending_bytes: total(|v| v.pending_bytes),
            held_bytes: total(|v| v.held_bytes),
            untracked_bytes: total(|v| v.untracked_bytes),
            handed_bytes: total(|v| v.handed_bytes),
            unmatched_roots: unmatched_roots.iter().map(|(app, root)| format!("{}:{root}", app.label())).collect(),
            volumes: rows,
        }
    }
}
