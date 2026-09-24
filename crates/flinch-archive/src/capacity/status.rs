//! The capacity block of status.json: what one measured cycle decided, per
//! volume and in aggregate, in the terms the operator reads. Two questions are
//! kept apart on purpose: whether the plan *covers* a goal (the eligible set is
//! big enough) and whether the goal is *met* (FLINCH has handed that much over).

use super::{sum, App, CapacityAction, CapacityDecision, CapacitySnapshot};
use crate::plan::VolumeOutcome;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One volume as status.json reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VolumeStatus {
    pub path: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub utilization: f32,
    pub deficit_bytes: u64,
    pub release_gap_bytes: u64,
    pub latched: bool,
    pub goal_bytes: u64,
    /// What the plan takes on this volume this run.
    pub reclaimed_bytes: u64,
    /// Everything eligible on this volume: the reserve eviction can draw on.
    pub eligible_bytes: u64,
    /// Evicted bytes the recycle bin may still hold on this volume.
    #[serde(default)]
    pub pending_bytes: u64,
    /// Bytes verified in a FLINCH deletion collection and still on disk:
    /// handed to Maintainerr, waiting for its schedule.
    #[serde(default)]
    pub handed_bytes: u64,
    /// Whether the plan covers the goal; `false` means the eligible set cannot
    /// get this volume back to its release mark. `None` unless evicting.
    #[serde(default)]
    pub covered: Option<bool>,
    /// Whether what is handed over covers the goal: FLINCH's part is done and
    /// Maintainerr's schedule does the rest. A covered goal stays unmet for a
    /// few runs while grace runs and per-run caps pace the hand-over. `None`
    /// unless evicting.
    #[serde(default)]
    pub goal_met: Option<bool>,
}

/// The operator-facing view of one measured cycle, published in status.json.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapacityStatus {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub ceiling: f32,
    pub release: f32,
    pub ceiling_bytes: u64,
    pub release_bytes: u64,
    pub deficit_bytes: u64,
    pub release_gap_bytes: u64,
    pub utilization: f32,
    pub over_ceiling: bool,
    /// Any volume is evicting this run.
    pub latched: bool,
    /// Bytes this run is freeing across volumes; 0 when idle.
    pub goal_bytes: u64,
    /// Whether every evicting volume's plan covers its goal. `None` when idle.
    #[serde(default)]
    pub covered: Option<bool>,
    /// Whether every evicting volume's hand-over covers its goal. `None` when
    /// idle: an idle run has no goal to miss.
    #[serde(default)]
    pub goal_met: Option<bool>,
    /// The calibrated never-played rule is part of the permitted set.
    pub armed_never_played: bool,
    #[serde(default)]
    pub volumes: Vec<VolumeStatus>,
    /// "app:path" root folders no mount holds — items there can never be
    /// evicted. Empty means every library root is governed.
    #[serde(default)]
    pub unmatched_roots: Vec<String>,
    /// Evicted bytes the recycle bins may still hold, credited against goals.
    #[serde(default)]
    pub pending_bytes: u64,
    /// Bytes handed to Maintainerr and still on disk, across volumes.
    #[serde(default)]
    pub handed_bytes: u64,
}

impl CapacityStatus {
    pub fn new(
        snapshot: &CapacitySnapshot,
        decision: &CapacityDecision,
        outcomes: &[VolumeOutcome],
        unmatched_roots: &[(App, String)],
        pending: &BTreeMap<String, u64>,
        handed: &BTreeMap<String, u64>,
    ) -> Self {
        let (goal_bytes, armed_never_played) = match decision.action {
            CapacityAction::Evict { goal_bytes, armed_never_played } => (goal_bytes, armed_never_played),
            CapacityAction::Idle | CapacityAction::Unmeasured => (0, false),
        };
        let volumes: Vec<VolumeStatus> = snapshot
            .volumes
            .iter()
            .map(|m| {
                let outcome = outcomes.iter().find(|o| o.volume == m.path);
                let latched = decision.goals.contains_key(&m.path);
                let reclaimed_bytes = outcome.map_or(0, |o| o.reclaimed_bytes);
                let handed_bytes = handed.get(&m.path).copied().unwrap_or(0);
                let goal = decision.goals.get(&m.path).copied().unwrap_or(0);
                VolumeStatus {
                    path: m.path.clone(),
                    total_bytes: m.total_bytes,
                    used_bytes: m.used_bytes,
                    utilization: m.utilization,
                    deficit_bytes: m.deficit_bytes,
                    release_gap_bytes: m.release_gap_bytes,
                    latched,
                    goal_bytes: goal,
                    reclaimed_bytes,
                    eligible_bytes: outcome.map_or(0, |o| o.eligible_bytes),
                    pending_bytes: pending.get(&m.path).copied().unwrap_or(0),
                    handed_bytes,
                    covered: latched.then_some(reclaimed_bytes >= goal),
                    goal_met: latched.then_some(handed_bytes >= goal),
                }
            })
            .collect();
        let evicting: Vec<&VolumeStatus> = volumes.iter().filter(|v| v.latched).collect();
        let every = |flag: fn(&VolumeStatus) -> Option<bool>| {
            (!evicting.is_empty()).then(|| evicting.iter().all(|v| flag(v) == Some(true)))
        };
        let (latched, covered, goal_met) = (!evicting.is_empty(), every(|v| v.covered), every(|v| v.goal_met));
        let handed_bytes = sum(volumes.iter().map(|v| v.handed_bytes));
        Self {
            total_bytes: snapshot.total_bytes,
            used_bytes: snapshot.used_bytes,
            ceiling: snapshot.watermarks.ceiling() as f32,
            release: snapshot.watermarks.release() as f32,
            ceiling_bytes: snapshot.ceiling_bytes,
            release_bytes: snapshot.release_bytes,
            deficit_bytes: snapshot.deficit_bytes,
            release_gap_bytes: snapshot.release_gap_bytes,
            utilization: snapshot.utilization,
            over_ceiling: snapshot.over_ceiling,
            latched,
            goal_bytes,
            covered,
            goal_met,
            armed_never_played,
            volumes,
            pending_bytes: sum(pending.values().copied()),
            handed_bytes,
            unmatched_roots: unmatched_roots
                .iter()
                .map(|(app, root)| format!("{}:{root}", app.label()))
                .collect(),
        }
    }
}
