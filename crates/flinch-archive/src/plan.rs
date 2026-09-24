//! The plan: turn cards + policy into an auditable delete list, sharpness-gated.
//!
//! The sharpness contract is explicit here, not aspirational:
//!
//! - An item is proposed for deletion only when the DETERMINISTIC policy says
//!   safe AND the model's probability is at least `delete_floor` (default 0.95).
//! - Anything below that floor is kept. Reconstruction from trash is cheap;
//!   reconstructing a season that was actually still wanted is not.
//! - `flinch-archive` never deletes anything. It writes a candidate list; a
//!   separate, explicit command (`apply`) moves candidates to a trash location
//!   with a TTL. A delete path owned by a model is the one failure this crate
//!   exists to prevent: a library emptied by a confident mistake.
//!
//! What the policy permits and how much the plan takes are separate questions.
//! The policy (plus both floors) decides *eligibility*; the [`ReclaimGoal`]
//! decides *how much* of the eligible set is taken, in eviction order — the
//! least expected regret per byte freed first.

use crate::calibration::{calibration_report, Observation, Probability};
use crate::card::ArchiveCard;
use crate::policy::{decide, reclaims_bytes, ArchivePolicy, Reason, ScoreVerdict};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Every delete in the plan. One row per item, with the reason the policy
/// produced. If the reason reads wrong to a human, the rule — not the model —
/// is what to change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanEntry {
    pub id: String,
    pub title: String,
    pub size_bytes: u64,
    pub reason: Reason,
    pub delete_probability: f32,
}

/// How much of the eligible set a plan takes. A plan-time input, not a policy
/// rule: the policy says what MAY go; the goal says how much SHOULD.
#[derive(Debug, Clone, PartialEq)]
pub enum ReclaimGoal {
    /// Every eligible item — the CLI's "delete everything safe".
    AllSafe,
    /// Eligible items in eviction order until this many bytes are covered.
    /// `Bytes(0)` plans nothing: there is no "zero means everything" sentinel.
    Bytes(u64),
    /// Per-volume goals: an item is taken only while *its* volume still needs
    /// space. Freeing a different disk relieves nothing, so an item on a volume
    /// with no goal — or on no known volume — is kept.
    PerVolume(VolumeGoals),
}

/// Byte goals per library volume, and which volume each card lives on.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VolumeGoals {
    /// Volume path → bytes it must free. Volumes absent here need nothing.
    pub goals: BTreeMap<String, u64>,
    /// Card id → the library volume its files live on.
    pub volume_of: HashMap<String, String>,
    /// Cards already handed to Maintainerr. While the policy still permits
    /// them they are taken first: re-planning a goal onto other items would
    /// take them back and restart every Leaving Soon window, delaying the
    /// space by a whole window each time.
    pub handed: HashSet<String>,
}

impl VolumeGoals {
    fn goal(&self, volume: &str) -> u64 {
        self.goals.get(volume).copied().unwrap_or(0)
    }
}

impl ReclaimGoal {
    fn volume_of(&self, id: &str) -> Option<&str> {
        match self {
            ReclaimGoal::PerVolume(goals) => goals.volume_of.get(id).map(String::as_str),
            ReclaimGoal::AllSafe | ReclaimGoal::Bytes(_) => None,
        }
    }

    fn handed(&self, id: &str) -> bool {
        match self {
            ReclaimGoal::PerVolume(goals) => goals.handed.contains(id),
            ReclaimGoal::AllSafe | ReclaimGoal::Bytes(_) => false,
        }
    }
}

/// Accounting for one volume under [`ReclaimGoal::PerVolume`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VolumeOutcome {
    pub volume: String,
    pub goal_bytes: u64,
    pub reclaimed_bytes: u64,
    /// Everything eligible on this volume: the reserve eviction can draw on.
    pub eligible_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub entries: Vec<PlanEntry>,
    pub reclaimed_bytes: u64,
    /// Total bytes the goal asked for; `None` when the goal was "everything safe".
    #[serde(default)]
    pub goal_bytes: Option<u64>,
    /// Every goal covered. Vacuously true for "everything safe".
    pub goal_met: bool,
    /// Everything the goal could ever take — policy- and floor-eligible, and on
    /// a known volume when goals are per volume — before the goal truncates it.
    #[serde(default)]
    pub eligible_bytes: u64,
    /// Per-volume accounting; empty unless the goal is per volume.
    #[serde(default)]
    pub volumes: Vec<VolumeOutcome>,
    /// How sharp the *decision set* is, measured on any provided labels:
    /// Brier/ECE/sharpness of the delete probabilities vs the deterministic
    /// labels. Reported even with no labels (then `None`).
    pub quality: Option<QualityReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualityReport {
    pub items: usize,
    pub brier: f32,
    pub ece: f32,
    pub sharpness: f32,
}

/// The interface the sharp head slots into. `Baseline` is the shipped,
/// complete implementation; `CalibratedHead` is where a trained model lands
/// once watch-history labels exist. It is a real boundary, not a stub: the
/// plan honours it even when every bias is a baseline rule.
pub trait ArchiveModel {
    /// `reason` is the policy's decision for this card *with* its score verdict.
    /// A model must judge that decision, not re-derive one without the verdict:
    /// the first version did, and armed never-played reclaim could never plan
    /// a deletion while its preview promised several.
    fn delete_probability(&self, card: &ArchiveCard, reason: &Reason) -> Probability;
}

/// Deterministic baseline as a "model": 1.0 when the policy says delete, 0.0
/// otherwise. Perfectly sharp and perfectly unimpressive — its Brier is what
/// any real head must beat (the archive analog of extrapolation baseline 4).
pub struct Baseline {
    pub policy: ArchivePolicy,
}

impl Baseline {
    pub fn new(policy: ArchivePolicy) -> Self {
        Self { policy }
    }

    pub fn with_defaults() -> Self {
        Self::new(ArchivePolicy::default())
    }
}

impl ArchiveModel for Baseline {
    fn delete_probability(&self, _card: &ArchiveCard, reason: &Reason) -> Probability {
        Probability::new(if reclaims_bytes(reason) > 0 { 1.0 } else { 0.0 }).unwrap_or(Probability::ZERO)
    }
}

/// One eligible item, with what the eviction order needs to rank it.
struct Candidate<'a> {
    card: &'a ArchiveCard,
    reason: &'a Reason,
    probability: f32,
    bytes: u64,
    volume: Option<&'a str>,
    /// Already handed to Maintainerr (see [`VolumeGoals::handed`]).
    handed: bool,
    regret: f64,
}

/// Expected regret of evicting an item, per byte it frees: the chance someone
/// still wanted it, spread over what deleting it buys. Minimising the sum of
/// this over a byte goal is what "least harm per GiB" means; ranking by P(safe)
/// alone would evict fifty tiny near-certain items before one large one that
/// is nearly as safe.
fn regret_per_byte(p_safe: f32, bytes: u64) -> f64 {
    f64::from(1.0 - p_safe.clamp(0.0, 1.0)) / bytes.max(1) as f64
}

/// Items already handed over first; then least regret per byte; at equal
/// regret the larger item (fewer delete operations); then the id, so the order
/// is total and reproducible. A non-finite regret (a NaN score) sorts last.
fn eviction_order(a: &Candidate, b: &Candidate) -> Ordering {
    b.handed
        .cmp(&a.handed)
        .then(a.regret.total_cmp(&b.regret))
        .then(b.bytes.cmp(&a.bytes))
        .then_with(|| a.card.id.cmp(&b.card.id))
}

/// Build the delete plan.
///
/// 1. Deterministic policy decides each card (protections first).
/// 2. Eligibility: the policy permits a delete, the model clears
///    `delete_floor` (the sharpness gate: a confident model never overrides a
///    "keep", a shy one never overrules a "delete"), and — when the card has a
///    calibrated verdict — P(safe) clears `policy.score_floor`.
/// 3. Eligible items are ranked by [`eviction_order`], using the calibrated
///    verdict's P(safe) when present and the model's probability otherwise (so a
///    baseline 1.0 ranks by size alone).
/// 4. The goal takes that order until it is covered.
pub fn build_plan(
    cards: &[ArchiveCard],
    model: &dyn ArchiveModel,
    policy: &ArchivePolicy,
    delete_floor: f32,
    verdicts: &HashMap<String, ScoreVerdict>,
    goal: &ReclaimGoal,
) -> Plan {
    let decided: Vec<(&ArchiveCard, Reason, f32)> = cards
        .iter()
        .map(|card| {
            let reason = decide(card, policy, verdicts.get(&card.id).copied());
            let probability = model.delete_probability(card, &reason);
            (card, reason, probability.get())
        })
        .collect();

    let mut eligible: Vec<Candidate> = decided
        .iter()
        .filter_map(|(card, reason, probability)| {
            let bytes = reclaims_bytes(reason);
            let verdict = verdicts.get(&card.id);
            let permitted = bytes > 0
                && *probability >= delete_floor
                && verdict.map_or(true, |v| v.p_safe >= policy.score_floor);
            permitted.then(|| Candidate {
                card,
                reason,
                probability: *probability,
                bytes,
                volume: goal.volume_of(&card.id),
                handed: goal.handed(&card.id),
                regret: regret_per_byte(verdict.map_or(*probability, |v| v.p_safe), bytes),
            })
        })
        .collect();
    eligible.sort_by(eviction_order);

    let mut entries = Vec::new();
    let mut reclaimed = 0u64;
    let mut reclaimed_on: BTreeMap<&str, u64> = BTreeMap::new();
    for candidate in &eligible {
        let take = match goal {
            ReclaimGoal::AllSafe => true,
            ReclaimGoal::Bytes(target) => reclaimed < *target,
            ReclaimGoal::PerVolume(goals) => candidate.volume.is_some_and(|volume| {
                reclaimed_on.get(volume).copied().unwrap_or(0) < goals.goal(volume)
            }),
        };
        if !take {
            continue;
        }
        reclaimed = reclaimed.saturating_add(candidate.bytes);
        if let Some(volume) = candidate.volume {
            let on_volume = reclaimed_on.entry(volume).or_insert(0);
            *on_volume = on_volume.saturating_add(candidate.bytes);
        }
        entries.push(PlanEntry {
            id: candidate.card.id.clone(),
            title: candidate.card.title.clone(),
            size_bytes: candidate.bytes,
            reason: candidate.reason.clone(),
            delete_probability: candidate.probability,
        });
    }

    let (goal_bytes, goal_met, eligible_bytes, volumes) = match goal {
        ReclaimGoal::AllSafe => (None, true, total_bytes(eligible.iter()), Vec::new()),
        ReclaimGoal::Bytes(target) => {
            (Some(*target), reclaimed >= *target, total_bytes(eligible.iter()), Vec::new())
        }
        ReclaimGoal::PerVolume(goals) => {
            let volumes = volume_outcomes(goals, &eligible, &reclaimed_on);
            let met = volumes.iter().all(|v| v.reclaimed_bytes >= v.goal_bytes);
            let sum = goals.goals.values().fold(0u64, |acc, bytes| acc.saturating_add(*bytes));
            let on_known = total_bytes(eligible.iter().filter(|c| c.volume.is_some()));
            (Some(sum), met, on_known, volumes)
        }
    };

    Plan {
        entries,
        reclaimed_bytes: reclaimed,
        goal_bytes,
        goal_met,
        eligible_bytes,
        volumes,
        quality: quality_of(&decided),
    }
}

fn total_bytes<'a>(candidates: impl Iterator<Item = &'a Candidate<'a>>) -> u64 {
    candidates.fold(0u64, |acc, candidate| acc.saturating_add(candidate.bytes))
}

/// One row per volume that has a goal or holds an eligible item, so an idle
/// run still reports each volume's reserve.
fn volume_outcomes(
    goals: &VolumeGoals,
    eligible: &[Candidate],
    reclaimed_on: &BTreeMap<&str, u64>,
) -> Vec<VolumeOutcome> {
    let mut rows: BTreeMap<&str, VolumeOutcome> = BTreeMap::new();
    let row = |volume: &str| -> VolumeOutcome {
        VolumeOutcome {
            volume: volume.to_string(),
            goal_bytes: goals.goal(volume),
            reclaimed_bytes: reclaimed_on.get(volume).copied().unwrap_or(0),
            eligible_bytes: 0,
        }
    };
    for volume in goals.goals.keys() {
        rows.insert(volume.as_str(), row(volume));
    }
    for candidate in eligible {
        if let Some(volume) = candidate.volume {
            let entry = rows.entry(volume).or_insert_with(|| row(volume));
            entry.eligible_bytes = entry.eligible_bytes.saturating_add(candidate.bytes);
        }
    }
    rows.into_values().collect()
}

fn quality_of(decided: &[(&ArchiveCard, Reason, f32)]) -> Option<QualityReport> {
    let observations: Vec<Observation> = decided
        .iter()
        .map(|(_, reason, probability)| {
            let label = crate::policy::is_safe_label(reason);
            Observation { predicted: *probability, outcome: label }
        })
        .collect();
    calibration_report(&observations, 10).map(|report| QualityReport {
        items: observations.len(),
        brier: report.brier,
        ece: report.ece,
        sharpness: report.sharpness,
    })
}

#[cfg(test)]
mod tests;
