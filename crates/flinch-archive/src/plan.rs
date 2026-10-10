//! The plan: which items leave so every volume's forecast fits, at the least
//! expected regret.
//!
//! - **Eligibility is narrow and explicit.** An item is a candidate unless it
//!   is pinned, in its grace period, unknown to Plex, on no governed disk,
//!   without watch evidence, or never played while never-played reclaim is off
//!   or held. Everything else competes on regret alone.
//! - **How much is the forecast's call.** Each volume's `B_target` (see
//!   [`crate::capacity`]) is a covering constraint; a healthy forecast plans
//!   nothing and the solver never runs.
//! - **Which items is the solver's.** [`knapsack::select`] solves the 0-1
//!   program with HiGHS, or greedily in an emergency, and never orphans part
//!   of a show. With an archive tier ([`crate::archive`]) it may move a movie or a
//!   whole series to an archive root instead of evicting it.
//! - `flinch-archive` never deletes anything. The plan is handed to
//!   Maintainerr only when `dry_run` is off, and Maintainerr deletes on its own
//!   schedule.

mod archive;
pub mod candidates;
mod config;
pub mod knapsack;
mod manifest;

pub use archive::{arr_item, ArchiveDestination, PlanMove};
pub use config::{InvalidPlannerConfig, PlannerConfig};
pub use manifest::Manifest;

use crate::capacity::VolumeForecast;
use crate::daemon::NeverPlayedHold;
use crate::regret::Regret;
use knapsack::{Force, Method, Sequence, Unit};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Why an item can never be selected this run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Exclusion {
    /// A favorite, or on a keep list (keep collection, keep tag, or the
    /// operator's own Maintainerr exclusion).
    Pinned(Pin),
    /// On disk for fewer than the grace period's days.
    Grace {
        days: u32,
    },
    /// Plex has no id for it, so Maintainerr cannot act on it.
    NotInPlex,
    /// No governed mount holds its files.
    NoGovernedDisk,
    /// No watch source knows it: "never played" would be a guess.
    NoWatchEvidence,
    NeverPlayedOff,
    NeverPlayedHeld(NeverPlayedHold),
    /// An operator rule keeps it ([`crate::rules`]): the rule's name.
    Rule(String),
    /// Its torrents keep it ([`crate::torrents`]): below their seed goal, or
    /// holding its bytes through a hardlink that stays.
    Seeding(crate::torrents::SeedHold),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pin {
    Favorite,
    KeepList,
    /// A household member asked to keep it ([`crate::requests`]), until
    /// `until` (unix seconds).
    Requested {
        until: u64,
    },
}

impl std::fmt::Display for Exclusion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pinned(Pin::Favorite) => f.write_str("Pinned: favorite"),
            Self::Pinned(Pin::KeepList) => f.write_str("Pinned: on a keep list"),
            Self::Pinned(Pin::Requested { until }) => {
                write!(f, "Pinned: kept on request until {}", crate::notify::utc_date(until / 86_400))
            }
            Self::Grace { days } => write!(f, "In its {days}-day grace period"),
            Self::NotInPlex => f.write_str("Not matched in the media server"),
            Self::NoGovernedDisk => f.write_str("On no governed disk"),
            Self::NoWatchEvidence => f.write_str("No watch evidence this run"),
            Self::NeverPlayedOff => f.write_str("Never played; never-played reclaim is off"),
            Self::NeverPlayedHeld(hold) => write!(f, "Never played; held {}", hold.until()),
            Self::Rule(name) => write!(f, "Kept by rule \u{201c}{name}\u{201d}"),
            Self::Seeding(hold) => write!(f, "{hold}"),
        }
    }
}

/// One library item as the planner sees it. Serialized into
/// `plan-inputs.json` so a rules preview can re-plan without the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaCandidate {
    pub id: String,
    pub title: String,
    pub size_bytes: u64,
    /// The governed volume its files live on.
    pub volume: Option<String>,
    pub regret: Regret,
    /// Why losing it is cheap, for the manifest.
    pub reason: String,
    pub age_days: f32,
    /// Set by the caller for every rule but the grace period, which the
    /// planner applies from [`PlannerConfig::grace_period_days`].
    pub exclusion: Option<Exclusion>,
    /// Its place in its show; `None` for a movie.
    pub sequence: Option<Sequence>,
    /// Already handed to Maintainerr: preferred, so its window keeps running.
    pub handed: bool,
    /// Nobody finished it: it goes to Leaving Soon, not straight to deletion.
    pub announce: bool,
    /// Maintainerr must never take it, whatever its own rules say: pinned, or
    /// someone is partway through.
    pub protect: bool,
    /// What to do with its quality, from the same regret (see [`crate::quality`]).
    pub quality: crate::quality::QualityAdvice,
    /// How safe evicting it is, 0..1, never above `1 − P(watch)`.
    pub eviction_safety: f64,
    /// An operator rule asks for it to go when its volume needs space
    /// ([`crate::rules`]). Never set on a protected item, and ignored there.
    #[serde(default)]
    pub force: Option<Force>,
}

/// One selected item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanItem {
    pub id: String,
    pub title: String,
    pub size_bytes: u64,
    pub regret: f64,
    pub reason: String,
    pub volume: String,
    #[serde(skip)]
    pub announce: bool,
    /// The selected item that must leave first (an unplayed later season, a
    /// played earlier one). Precedes this one in [`EvictionPlan::items`].
    #[serde(skip)]
    pub after: Option<String>,
}

/// One volume's target against what the plan takes there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VolumeOutcome {
    pub volume: String,
    pub target_bytes: u64,
    pub planned_bytes: u64,
    /// Everything selectable on this volume: what a larger target could draw on.
    pub eligible_bytes: u64,
}

/// Why a candidate stays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kept {
    Excluded(Exclusion),
    /// Its volume's forecast fits: nothing there needs to go.
    Healthy,
    /// Selectable, but the target was covered more cheaply.
    NotNeeded,
    /// It moves to the archive root instead of leaving ([`PlanMove`]).
    Archived,
}

impl std::fmt::Display for Kept {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Excluded(exclusion) => exclusion.fmt(f),
            Self::Healthy => f.write_str("Not needed: storage is healthy"),
            Self::NotNeeded => f.write_str("Not needed this run"),
            Self::Archived => f.write_str("Moves to the archive: still playable"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EvictionPlan {
    /// How the items were chosen; `None` when every forecast was healthy and
    /// the solver never ran.
    pub method: Option<Method>,
    pub solver_error: Option<String>,
    /// Selected items in the order they should leave: prerequisites first,
    /// then most bytes per regret.
    pub items: Vec<PlanItem>,
    /// Movies and whole series that move to an archive root instead; their
    /// bytes count toward each volume's `planned_bytes`.
    pub moves: Vec<PlanMove>,
    pub volumes: Vec<VolumeOutcome>,
    pub target_bytes: u64,
    pub total_reclaimed_bytes: u64,
    pub total_regret: f64,
    /// Candidates the solver could select.
    pub candidates_count: usize,
    pub eligible_bytes: u64,
    /// Why every candidate not selected stays.
    pub kept: HashMap<String, Kept>,
}

impl EvictionPlan {
    /// Every volume's target is covered.
    pub fn covered(&self) -> bool {
        self.volumes.iter().all(|volume| volume.planned_bytes >= volume.target_bytes)
    }

    /// What may be handed over this run: items past their grace streak
    /// (`eligible`), in plan order, each only once the item it must follow is
    /// handed over too (earlier, or earlier in this list).
    pub fn releasable(&self, eligible: &[String], handed: &HashSet<String>) -> Vec<String> {
        let eligible: HashSet<&str> = eligible.iter().map(String::as_str).collect();
        let mut released: HashSet<&str> = HashSet::new();
        let mut out = Vec::new();
        for item in self.items.iter().filter(|item| eligible.contains(item.id.as_str())) {
            let ready = item.after.as_deref().is_none_or(|after| released.contains(after) || handed.contains(after));
            if ready {
                released.insert(item.id.as_str());
                out.push(item.id.clone());
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PlanError {
    #[error(transparent)]
    Config(#[from] InvalidPlannerConfig),
    #[error("candidate {id} has a regret that is not a finite number of 0 or more")]
    InvalidRegret { id: String },
}

/// Plan one run with no archive tier: evictions only.
pub fn generate_eviction_plan(
    candidates: &[MediaCandidate],
    forecasts: &[VolumeForecast],
    config: &PlannerConfig,
) -> Result<EvictionPlan, PlanError> {
    generate_plan(candidates, forecasts, config, &[])
}

/// Plan one run.
///
/// Each forecast's `target_reclaim_bytes` is a covering constraint on its
/// volume. All zero: a healthy plan with no method and no items. Any forecast
/// in an emergency: the greedy pass instead of HiGHS. Each `archive`
/// destination lets its app's items move there instead of leaving.
pub fn generate_plan(
    candidates: &[MediaCandidate],
    forecasts: &[VolumeForecast],
    config: &PlannerConfig,
    archive: &[ArchiveDestination],
) -> Result<EvictionPlan, PlanError> {
    config.validate()?;
    if let Some(bad) = candidates.iter().find(|c| !(c.regret.value.is_finite() && c.regret.value >= 0.0)) {
        return Err(PlanError::InvalidRegret { id: bad.id.clone() });
    }
    let exclusion = |candidate: &MediaCandidate| {
        candidate.exclusion.clone().or_else(|| candidate.volume.is_none().then_some(Exclusion::NoGovernedDisk)).or_else(|| {
            (candidate.age_days < config.grace_period_days as f32).then_some(Exclusion::Grace { days: config.grace_period_days })
        })
    };
    let units: Vec<Unit> = candidates
        .iter()
        .map(|candidate| Unit {
            size_bytes: candidate.size_bytes,
            regret: candidate.regret.value,
            volume: candidate.volume.clone().unwrap_or_default(),
            sequence: candidate.sequence.clone(),
            selectable: exclusion(candidate).is_none(),
            handed: candidate.handed,
            // A rule never forces a pinned or partway item, whoever set it.
            force: candidate.force.filter(|_| !candidate.protect),
        })
        .collect();
    let targets: BTreeMap<String, u64> = forecasts.iter().map(|f| (f.volume.clone(), f.forecast.target_reclaim_bytes)).collect();
    let healthy = targets.values().all(|bytes| *bytes == 0);
    let tier = archive::tier(candidates, archive, |c| exclusion(c).is_none() && !c.protect && !c.handed && c.force.is_none());
    let (method, solver_error, chosen, moved) = if healthy {
        (None, None, Vec::new(), Vec::new())
    } else {
        let emergency = forecasts.iter().any(|f| f.forecast.is_emergency);
        let selection = knapsack::select_with_moves(&units, &targets, config.quantum_bytes(), emergency, &tier.moves);
        (Some(selection.method), selection.solver_error, selection.chosen, selection.moved)
    };
    let moves = tier.plan_moves(&moved, candidates);

    let items: Vec<PlanItem> = knapsack::release_order(&units, &chosen)
        .into_iter()
        .map(|(index, after)| {
            let candidate = &candidates[index];
            PlanItem {
                id: candidate.id.clone(),
                title: candidate.title.clone(),
                size_bytes: candidate.size_bytes,
                regret: candidate.regret.value,
                reason: candidate.reason.clone(),
                volume: units[index].volume.clone(),
                announce: candidate.announce,
                after: after.map(|prerequisite| candidates[prerequisite].id.clone()),
            }
        })
        .collect();

    let mut volumes: BTreeMap<&str, VolumeOutcome> = targets
        .iter()
        .map(|(volume, target)| {
            (volume.as_str(), VolumeOutcome { volume: volume.clone(), target_bytes: *target, planned_bytes: 0, eligible_bytes: 0 })
        })
        .collect();
    for unit in units.iter().filter(|unit| unit.selectable) {
        if let Some(outcome) = volumes.get_mut(unit.volume.as_str()) {
            outcome.eligible_bytes = outcome.eligible_bytes.saturating_add(unit.size_bytes);
        }
    }
    let freed = items.iter().map(|item| (&item.volume, item.size_bytes)).chain(moves.iter().map(|m| (&m.volume, m.size_bytes)));
    for (volume, bytes) in freed {
        if let Some(outcome) = volumes.get_mut(volume.as_str()) {
            outcome.planned_bytes = outcome.planned_bytes.saturating_add(bytes);
        }
    }
    let volumes: Vec<VolumeOutcome> = volumes.into_values().collect();

    let selected: std::collections::HashSet<&str> = items.iter().map(|item| item.id.as_str()).collect();
    let archived: HashSet<&str> = moves.iter().flat_map(|m| m.cards.iter().map(String::as_str)).collect();
    let kept = candidates
        .iter()
        .filter(|candidate| !selected.contains(candidate.id.as_str()))
        .map(|candidate| {
            let target = candidate.volume.as_deref().and_then(|volume| targets.get(volume)).copied().unwrap_or(0);
            let why = match exclusion(candidate) {
                Some(exclusion) => Kept::Excluded(exclusion),
                None if archived.contains(candidate.id.as_str()) => Kept::Archived,
                None if target == 0 => Kept::Healthy,
                None => Kept::NotNeeded,
            };
            (candidate.id.clone(), why)
        })
        .collect();

    let sum = |values: &mut dyn Iterator<Item = u64>| values.fold(0u64, u64::saturating_add);
    Ok(EvictionPlan {
        method,
        solver_error,
        target_bytes: sum(&mut volumes.iter().map(|v| v.target_bytes)),
        total_reclaimed_bytes: sum(&mut items.iter().map(|item| item.size_bytes)),
        total_regret: items.iter().fold(0.0, |total, item| total + item.regret),
        candidates_count: units.iter().filter(|unit| unit.selectable).count(),
        eligible_bytes: sum(&mut volumes.iter().map(|v| v.eligible_bytes)),
        items,
        moves,
        volumes,
        kept,
    })
}

/// Items and bytes that never-played reclaim would add if it were on: the
/// candidates excluded by that switch alone.
pub fn never_played_preview(candidates: &[MediaCandidate]) -> (usize, u64) {
    candidates
        .iter()
        .filter(|candidate| candidate.exclusion == Some(Exclusion::NeverPlayedOff))
        .fold((0, 0), |(count, bytes), candidate| (count + 1, bytes + candidate.size_bytes))
}

#[cfg(test)]
mod tests;
