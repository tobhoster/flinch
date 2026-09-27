//! One planning cycle: inventory -> cards -> plan. It is pure: it decides keep
//! or evict for every card and touches nothing. Maintainerr sync
//! ([`crate::maintainerr`]) turns the decisions into exclusions and collection
//! members, and owns what persists between cycles about them.

use crate::arr::{ArrMovie, ArrSeries};
use crate::card::ArchiveCard;
use crate::plan::{build_plan, Baseline, ReclaimGoal, VolumeOutcome};
use crate::policy::{ArchivePolicy, ScoreVerdict};
use crate::watch;
use std::collections::{BTreeSet, HashMap, HashSet};

/// The model's delete gate. The baseline answers 1.0 for every reclaiming
/// reason, so `score_floor` on P(safe) is the gate that bites.
const DELETE_FLOOR: f32 = 0.95;

#[derive(Debug, Clone)]
pub struct ReconcileOutput {
    /// Cards to evict, in eviction order (least regret per byte first). The
    /// per-run caps meet them in this order, never a hash order.
    pub deleted_ids: Vec<String>,
    /// Of `deleted_ids`: the evictions nobody finished (see
    /// [`crate::policy::announces`]). They are announced in Leaving Soon
    /// before they go, and wait while it is unusable. While it is unnamed
    /// there are none: never-played reclaim is held (see [`NeverPlayedHold`]).
    pub announced_ids: BTreeSet<String>,
    /// Cards the policy or a floor keeps: the ones to protect.
    pub kept_ids: Vec<String>,
    /// Cards eligible for eviction that the goal did not need this cycle: the
    /// reserve. The daemon protects them like kept cards (below the ceiling
    /// nothing may go, and the operator's own Maintainerr rules would otherwise
    /// take them); the sync releases that exclusion when one is evicted.
    pub reserve_ids: Vec<String>,
    pub scanned: usize,
    pub delete_candidates: usize,
    /// Everything not evicted: kept plus reserve.
    pub kept: usize,
    pub reclaimed_bytes: u64,
    /// Whether the plan covered its goal. Vacuously true when the goal was
    /// "everything safe" or no volume asked for space.
    pub goal_met: bool,
    /// Everything the goal could have taken: the reserve for when space is needed.
    pub eligible_bytes: u64,
    /// Per-volume accounting when the goal is per volume.
    pub volumes: Vec<VolumeOutcome>,
}

/// Plan one cycle.
///
/// - `movies` / `series`: inventory items (already fetched).
/// - `watch_state`: id -> media-server truth; missing entries fail closed.
/// - `operator_keeps`: cards the operator protects in Maintainerr (see
///   [`crate::maintainerr::operator_keeps`]); a hard keep guard.
/// - `goal`: how much of the eligible set is taken this cycle.
pub fn reconcile(
    movies: &[ArrMovie],
    series: &[ArrSeries],
    watch_state: &HashMap<String, watch::WatchEntry>,
    operator_keeps: &BTreeSet<String>,
    policy: &ArchivePolicy,
    verdicts: &HashMap<String, ScoreVerdict>,
    goal: &ReclaimGoal,
) -> ReconcileOutput {
    let mut cards: Vec<ArchiveCard> =
        movies.iter().filter_map(ArrMovie::to_card).chain(series.iter().flat_map(ArrSeries::to_cards)).collect();
    watch::apply(&mut cards, watch_state);
    guard_operator_keeps(&mut cards, operator_keeps);

    let model = Baseline::new(*policy);
    let plan = build_plan(&cards, &model, policy, DELETE_FLOOR, verdicts, goal);
    // The same plan without a goal takes every permitted card: the eligible set.
    let eligible: HashSet<String> = build_plan(&cards, &model, policy, DELETE_FLOOR, verdicts, &ReclaimGoal::AllSafe)
        .entries
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    let announced_ids: BTreeSet<String> =
        plan.entries.iter().filter(|entry| crate::policy::announces(&entry.reason)).map(|entry| entry.id.clone()).collect();
    let deleted_ids: Vec<String> = plan.entries.into_iter().map(|entry| entry.id).collect();
    let deleted: HashSet<&str> = deleted_ids.iter().map(String::as_str).collect();
    let (reserve_ids, kept_ids): (Vec<String>, Vec<String>) =
        cards.iter().filter(|card| !deleted.contains(card.id.as_str())).map(|card| card.id.clone()).partition(|id| eligible.contains(id));

    ReconcileOutput {
        scanned: cards.len(),
        delete_candidates: deleted_ids.len(),
        kept: kept_ids.len() + reserve_ids.len(),
        deleted_ids,
        announced_ids,
        kept_ids,
        reserve_ids,
        reclaimed_bytes: plan.reclaimed_bytes,
        goal_met: plan.goal_met,
        eligible_bytes: plan.eligible_bytes,
        volumes: plan.volumes,
    }
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

/// Hold never-played reclaim off while `hold` says so: clears the operator's
/// switch on `policy`, and returns the settings capacity governs by, which then
/// never arm the rule under disk pressure either.
pub fn hold_never_played(settings: &RuntimeSettings, hold: Option<NeverPlayedHold>, policy: &mut ArchivePolicy) -> RuntimeSettings {
    policy.unwatched_reclaim.enabled &= hold.is_none();
    RuntimeSettings { capacity_arm_never_played: settings.capacity_arm_never_played && hold.is_none(), ..settings.clone() }
}

/// Whether the operator's settings ask never-played reclaim to run this cycle:
/// its switch, or "While evicting" while a disk evicts. Read from the settings
/// as saved, not as held: held and not asked for, lifting the hold alone runs
/// nothing, so the UI names both steps (status.json `never_played_requested`).
pub fn never_played_requested(settings: &RuntimeSettings, action: &crate::capacity::CapacityAction) -> bool {
    settings.unwatched_reclaim_enabled
        || (settings.capacity_arm_never_played && matches!(action, crate::capacity::CapacityAction::Evict { .. }))
}

#[cfg(test)]
mod tests;

mod settings;
mod state;

pub use settings::*;
pub use state::*;
