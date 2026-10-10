//! One planning cycle: candidates -> plan. It is pure: it decides keep or
//! evict for every candidate and touches nothing. Maintainerr sync
//! ([`crate::maintainerr`]) turns the decisions into exclusions and collection
//! members, and owns what persists between cycles about them.

use crate::capacity::VolumeForecast;
use crate::card::ArchiveCard;
use crate::plan::{generate_plan, ArchiveDestination, EvictionPlan, MediaCandidate, PlanError, PlannerConfig};
use crate::watch;
use std::collections::BTreeSet;

#[derive(Debug, Clone)]
pub struct ReconcileOutput {
    pub plan: EvictionPlan,
    /// Selected cards in the order they leave (prerequisites first). The
    /// per-run caps take a prefix of this order, never a hash order.
    pub deleted_ids: Vec<String>,
    /// Of `deleted_ids`: the evictions nobody finished. They are announced in
    /// Leaving Soon before they go, and wait while it is unusable.
    pub announced_ids: BTreeSet<String>,
    /// Cards Maintainerr must never take, whatever its own rules say: pinned,
    /// or someone is partway through (unless the plan takes it).
    pub protected_ids: Vec<String>,
    pub scanned: usize,
}

/// Plan one cycle over `candidates` (see [`crate::plan::candidates`]); each
/// `archive` destination lets its app's items move there instead of leaving.
pub fn reconcile(
    candidates: &[MediaCandidate],
    forecasts: &[VolumeForecast],
    config: &PlannerConfig,
    archive: &[ArchiveDestination],
) -> Result<ReconcileOutput, PlanError> {
    let plan = generate_plan(candidates, forecasts, config, archive)?;
    let deleted_ids: Vec<String> = plan.items.iter().map(|item| item.id.clone()).collect();
    // Someone partway through raises regret but does not exclude: when the
    // plan still takes it, the eviction wins over the protection.
    let selected: std::collections::HashSet<&str> = deleted_ids.iter().map(String::as_str).collect();
    let protected_ids = candidates
        .iter()
        .filter(|candidate| candidate.protect && !selected.contains(candidate.id.as_str()))
        .map(|candidate| candidate.id.clone())
        .collect();
    Ok(ReconcileOutput {
        announced_ids: plan.items.iter().filter(|item| item.announce).map(|item| item.id.clone()).collect(),
        deleted_ids,
        protected_ids,
        scanned: candidates.len(),
        plan,
    })
}

/// A card the operator protects in Maintainerr (an exclusion FLINCH does not
/// own) is a hard keep, exactly like a keep collection.
pub fn guard_operator_keeps(cards: &mut [ArchiveCard], operator_keeps: &BTreeSet<String>) {
    for card in cards.iter_mut().filter(|card| operator_keeps.contains(&card.id)) {
        card.in_keep_collection = true;
    }
}

/// Why never-played reclaim is held off for a cycle, whatever the operator's
/// switch or disk pressure asks for. Held, the rule permits nothing: its items
/// are kept, and none counts toward a capacity goal. status.json names it as
/// `"incomplete_evidence"` or `"leaving_soon_untitled"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NeverPlayedHold {
    /// A watch source was not read completely (see
    /// [`watch::EvidenceHealth::never_played_reclaim_safe`]): "never played"
    /// would be a guess.
    IncompleteEvidence,
    /// The Leaving Soon title is blank, so nothing unwatched can be announced,
    /// and nothing unwatched leaves unannounced. Armed, the rule's items would
    /// cover the capacity goal and then wait at the hand-off, while the watched
    /// evictions they displaced were never picked: the disk would stay full.
    LeavingSoonUntitled,
}

impl NeverPlayedHold {
    /// This cycle's hold, if any. Incomplete evidence is named first: naming
    /// Leaving Soon would not lift it.
    pub fn of(health: &watch::EvidenceHealth, titles: &crate::maintainerr::CollectionTitles) -> Option<Self> {
        if !health.never_played_reclaim_safe() {
            Some(Self::IncompleteEvidence)
        } else if titles.leaving.trim().is_empty() {
            Some(Self::LeavingSoonUntitled)
        } else {
            None
        }
    }

    /// Until when the held items wait, in the operator's words.
    pub fn until(self) -> &'static str {
        match self {
            Self::IncompleteEvidence => "until the watch evidence is complete",
            Self::LeavingSoonUntitled => "until a Leaving Soon collection is named",
        }
    }
}

#[cfg(test)]
mod tests;

mod settings;
mod state;

pub use settings::*;
pub use state::*;
