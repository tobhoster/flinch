//! Reading Maintainerr, and running a [`SyncPlan`] against it.
//!
//! The executor records ownership only after a read-back shows the change. A
//! failure (409 while Maintainerr holds its lock, a timeout, a refusal) or an
//! unverified success is never marked done. Nothing is recorded for it, so the
//! next cycle plans the same action again.

use super::plan::{Observed, SyncAction, SyncItem, SyncPlan};
use super::validate::{self, CollectionTitles, Handover};
use super::{MaintainerrApi, MaintainerrError, MaintainerrTarget, OwnedState, ProtectedEntry, ScheduledEntry};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Read everything the planner needs: the version, the collections, the
/// members of every titled or FLINCH-used collection, and the exclusion rows
/// of every resolved item and every FLINCH-owned exclusion. Any failed or
/// unreadable read fails the whole observation, so a cycle never plans from
/// a partial view.
pub async fn observe<A: MaintainerrApi>(
    api: &mut A,
    items: &[SyncItem],
    titles: &CollectionTitles,
    owned: &OwnedState,
) -> Result<Observed, MaintainerrError> {
    let version = api.version().await?;
    let collections = api.collections().await?;
    // Every collection a configured title names, on either route.
    let titled = validate::destinations(&collections, titles).into_keys();
    let used = owned.scheduled.values().map(|entry| entry.collection_id);
    let mut members = BTreeMap::new();
    for collection_id in titled.chain(used).collect::<BTreeSet<_>>() {
        members.insert(collection_id, api.collection_members(collection_id).await?.into_iter().collect());
    }
    let keys: BTreeSet<String> = items
        .iter()
        .filter_map(SyncItem::target)
        .chain(owned.protected.values().map(|entry| entry.target.clone()))
        .map(|target| target.media_id().to_string())
        .collect();
    let mut exclusions = BTreeMap::new();
    for key in keys {
        let rows = api.exclusions(&key).await?;
        exclusions.insert(key, rows);
    }
    Ok(Observed { version, collections, members, exclusions })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Applied, and a read-back confirmed it.
    Done,
    /// Dry-run: the sink printed it; nothing was sent or recorded.
    DryRun,
    /// Maintainerr refused or could not be reached. Planned again next cycle.
    Failed(String),
    /// Maintainerr accepted the call but the read-back disagrees. Nothing is
    /// recorded; planned again next cycle.
    Unverified(&'static str),
    /// Not attempted, because an earlier step for the same card did not
    /// complete.
    Skipped,
}

impl Outcome {
    fn completed(&self) -> bool {
        matches!(self, Self::Done | Self::DryRun)
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Done => f.write_str("done, verified"),
            Self::DryRun => f.write_str("dry-run, not sent"),
            Self::Failed(error) => write!(f, "failed, retried next cycle: {error}"),
            Self::Unverified(detail) => write!(f, "not verified, retried next cycle: {detail}"),
            Self::Skipped => f.write_str("skipped: an earlier step for this card did not complete"),
        }
    }
}

/// A plan and what became of each of its actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub plan: SyncPlan,
    /// One per `plan.actions`, in the same order.
    pub outcomes: Vec<Outcome>,
}

impl SyncReport {
    pub fn results(&self) -> impl Iterator<Item = (&SyncAction, &Outcome)> {
        self.plan.actions.iter().zip(&self.outcomes)
    }

    /// Cards newly added to a collection and verified this cycle, with their
    /// bytes: exactly what the eviction ledger records.
    pub fn scheduled(&self) -> impl Iterator<Item = (&str, u64)> {
        self.results().filter_map(|(action, outcome)| match (action, outcome) {
            (SyncAction::Schedule { card_id, bytes, .. }, Outcome::Done) => Some((card_id.as_str(), *bytes)),
            _ => None,
        })
    }

    /// Cards taken back out of a collection and verified this cycle: nothing
    /// of theirs is on its way out any more.
    pub fn unscheduled(&self) -> impl Iterator<Item = &str> {
        self.results().filter_map(|(action, outcome)| match (action, outcome) {
            (SyncAction::Unschedule { card_id, .. }, Outcome::Done) => Some(card_id.as_str()),
            _ => None,
        })
    }
}

/// Run the plan in order. A card's later steps run only after its earlier
/// ones completed, so a keep never follows an unverified un-schedule, and a
/// schedule never follows an exclusion that is still in place. With a
/// simulated (dry-run) API every write is printed by the sink, and `owned` is
/// left untouched.
pub async fn execute<A: MaintainerrApi>(
    api: &mut A,
    plan: SyncPlan,
    observed: &Observed,
    owned: &mut OwnedState,
    now: u64,
) -> SyncReport {
    let simulated = api.simulated();
    if !simulated {
        owned.prune(observed);
    }
    let mut stalled: BTreeSet<String> = BTreeSet::new();
    let mut outcomes = Vec::with_capacity(plan.actions.len());
    for action in &plan.actions {
        let outcome = if stalled.contains(action.card_id()) {
            Outcome::Skipped
        } else if simulated {
            match write(api, action).await {
                Ok(()) => Outcome::DryRun,
                Err(error) => Outcome::Failed(error.to_string()),
            }
        } else {
            match apply(api, action, owned, now).await {
                Ok(outcome) => outcome,
                Err(error) => Outcome::Failed(error.to_string()),
            }
        };
        if !outcome.completed() {
            stalled.insert(action.card_id().to_string());
        }
        outcomes.push(outcome);
    }
    SyncReport { plan, outcomes }
}

/// The write itself, with no read-back.
async fn write<A: MaintainerrApi>(api: &mut A, action: &SyncAction) -> Result<(), MaintainerrError> {
    match action {
        SyncAction::RemoveExclusion { exclusion_id, .. } => api.remove_exclusion(*exclusion_id).await,
        SyncAction::Schedule { target, collection_id, .. } => api.add_to_collection(*collection_id, target).await,
        SyncAction::Unschedule { target, collection_id, .. } => {
            api.remove_from_collection(*collection_id, target.item_key()).await
        }
        SyncAction::Protect { target, .. } => api.add_exclusion(target).await,
    }
}

/// Write, read back, and record ownership only when the read-back agrees.
async fn apply<A: MaintainerrApi>(
    api: &mut A,
    action: &SyncAction,
    owned: &mut OwnedState,
    now: u64,
) -> Result<Outcome, MaintainerrError> {
    match action {
        SyncAction::RemoveExclusion { card_id, target, exclusion_id } => {
            write(api, action).await?;
            if api.exclusions(target.media_id()).await?.iter().any(|row| row.id == *exclusion_id) {
                return Ok(Outcome::Unverified("the row is still there"));
            }
            owned.forget_exclusion(card_id, *exclusion_id);
        }
        SyncAction::Schedule { card_id, target, collection_id, .. } => {
            write(api, action).await?;
            if !is_member(api, *collection_id, target).await? {
                return Ok(Outcome::Unverified("the item is not a member"));
            }
            owned.protected.remove(card_id);
            owned.scheduled.insert(
                card_id.clone(),
                ScheduledEntry { target: target.clone(), collection_id: *collection_id, added_at: now },
            );
        }
        SyncAction::Unschedule { card_id, target, collection_id } => {
            write(api, action).await?;
            if is_member(api, *collection_id, target).await? {
                return Ok(Outcome::Unverified("the item is still a member"));
            }
            owned.scheduled.remove(card_id);
        }
        SyncAction::Protect { card_id, target } => {
            // Rows that existed before the POST are never claimed: Maintainerr
            // reuses an existing row, so only new ids are FLINCH's.
            let before: BTreeSet<i64> = api.exclusions(target.media_id()).await?.iter().map(|row| row.id).collect();
            write(api, action).await?;
            let created: Vec<_> =
                api.exclusions(target.media_id()).await?.into_iter().filter(|row| !before.contains(&row.id)).collect();
            if !created.iter().any(|row| row.media_server_id == target.item_key()) {
                return Ok(Outcome::Unverified("no new row for the item"));
            }
            let exclusion_ids = created.iter().map(|row| row.id).collect();
            owned.protected.insert(card_id.clone(), ProtectedEntry { target: target.clone(), exclusion_ids });
        }
    }
    Ok(Outcome::Done)
}

async fn is_member<A: MaintainerrApi>(
    api: &mut A,
    collection_id: i64,
    target: &MaintainerrTarget,
) -> Result<bool, MaintainerrError> {
    Ok(api.collection_members(collection_id).await?.iter().any(|key| key == target.item_key()))
}

/// status.json's sync block.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncSummary {
    /// Set when Maintainerr could not be read; nothing was planned or sent.
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    pub dry_run: bool,
    /// Verified this cycle.
    pub exclusions_added: usize,
    pub exclusions_removed: usize,
    pub scheduled: usize,
    pub scheduled_bytes: u64,
    /// Of `scheduled`: verified adds into Leaving Soon — announced, not yet
    /// deleted. Planned too in a dry run, so the preview says how many.
    #[serde(default)]
    pub announced: usize,
    #[serde(default)]
    pub announced_bytes: u64,
    pub unscheduled: usize,
    /// Writes a dry run printed instead of sending.
    pub simulated: usize,
    /// Failed or unverified writes, planned again next cycle.
    pub failures: usize,
    /// Writes not attempted because an earlier step for the card failed.
    pub skipped: usize,
    pub already_protected: usize,
    pub already_scheduled: usize,
    /// Cards the operator's own exclusions keep. While Maintainerr is
    /// unreadable: the keeps last read, which still guard the plan.
    pub operator_keeps: usize,
    pub unresolved: usize,
    pub deferred: usize,
    /// Version gate, misconfigured collections and blocked items, in words.
    pub problems: Vec<String>,
}

impl SyncSummary {
    /// Maintainerr could not be read: nothing was planned or sent, and the
    /// `operator_keeps` last read still guard the plan.
    pub fn unavailable(error: &MaintainerrError, dry_run: bool, operator_keeps: usize) -> Self {
        Self { error: Some(error.to_string()), dry_run, operator_keeps, ..Self::default() }
    }

    pub fn new(report: &SyncReport, dry_run: bool) -> Self {
        let plan = &report.plan;
        let mut summary = Self {
            version: Some(plan.handover.version().to_string()),
            dry_run,
            already_protected: plan.already_protected,
            already_scheduled: plan.already_scheduled,
            operator_keeps: plan.operator_keeps.len(),
            unresolved: plan.unresolved.len(),
            deferred: plan.deferred.len(),
            ..Self::default()
        };
        if matches!(plan.handover, Handover::Refused { .. }) {
            summary.problems.push(plan.handover.to_string());
        }
        summary.problems.extend(plan.misconfigured.iter().map(ToString::to_string));
        summary.problems.extend(plan.blocked.iter().map(|(card_id, reason)| format!("{card_id}: {reason}")));
        for (action, outcome) in report.results() {
            if let (Outcome::Done | Outcome::DryRun, SyncAction::Schedule { collection_id, bytes, .. }) = (outcome, action) {
                if plan.leaving.contains(collection_id) {
                    summary.announced += 1;
                    summary.announced_bytes = summary.announced_bytes.saturating_add(*bytes);
                }
            }
            match (outcome, action) {
                (Outcome::Done, SyncAction::Protect { .. }) => summary.exclusions_added += 1,
                (Outcome::Done, SyncAction::RemoveExclusion { .. }) => summary.exclusions_removed += 1,
                (Outcome::Done, SyncAction::Schedule { bytes, .. }) => {
                    summary.scheduled += 1;
                    summary.scheduled_bytes = summary.scheduled_bytes.saturating_add(*bytes);
                }
                (Outcome::Done, SyncAction::Unschedule { .. }) => summary.unscheduled += 1,
                (Outcome::DryRun, _) => summary.simulated += 1,
                (Outcome::Failed(_) | Outcome::Unverified(_), _) => summary.failures += 1,
                (Outcome::Skipped, _) => summary.skipped += 1,
            }
        }
        summary
    }
}
