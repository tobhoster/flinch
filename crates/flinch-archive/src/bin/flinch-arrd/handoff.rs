//! The hand-off to Maintainerr, once per cycle: everything FLINCH keeps — the
//! reserve included — becomes an exclusion, evictions past the grace window
//! join a collection (least regret first, within the caps): Leaving Soon when
//! nobody finished them, their kind's delete collection otherwise. Every
//! verified add or take-back is booked in the eviction ledger.

use super::sink::Sink;
use super::state_dir;
use flinch_archive::capacity::{App, EvictionLedger};
use flinch_archive::govern::Governance;
use flinch_archive::maintainerr::{self as mx, MaintainerrError, OwnedState, SyncItem};
use flinch_archive::{LibraryKind, ReconcileOutput};
use std::collections::HashMap;

/// This cycle's decisions, as the hand-off needs them.
pub(super) struct Handoff<'a> {
    /// Every card with its Plex ids, by card id.
    pub(super) items: &'a HashMap<&'a str, &'a SyncItem>,
    pub(super) report: &'a ReconcileOutput,
    /// Evictions past the grace window, in eviction order.
    pub(super) eligible: &'a [String],
    pub(super) titles: &'a mx::CollectionTitles,
    pub(super) caps: mx::Caps,
    pub(super) enforcing: bool,
    pub(super) now: u64,
}

/// Sync Maintainerr with the cycle's decisions and book the ledger. An
/// unreadable Maintainerr syncs nothing; `operator_keeps` is then the number
/// of keeps, as last read, that still guard the plan.
pub(super) async fn sync(
    handoff: Handoff<'_>,
    observed: Result<mx::Observed, MaintainerrError>,
    api: &mut Sink,
    owned: &mut OwnedState,
    governance: &Governance,
    ledger: &mut EvictionLedger,
    operator_keeps: usize,
) -> mx::SyncSummary {
    let Handoff { items, report, eligible, titles, caps, enforcing, now } = handoff;
    let observed = match observed {
        Ok(observed) => observed,
        Err(error) => return mx::SyncSummary::unavailable(&error, !enforcing, operator_keeps),
    };
    let pick = |ids: &[String]| -> Vec<SyncItem> {
        ids.iter().filter_map(|id| items.get(id.as_str()).map(|item| (*item).clone())).collect()
    };
    // Protect everything FLINCH keeps — the reserve included. Below the ceiling
    // nothing may be deleted (watermark governance), but the operator's own
    // Maintainerr rules (e.g. "Watched … Cleanup") would still take exactly the
    // watched, cold reserve items. The planner releases FLINCH's own exclusion
    // before it schedules an item, so a reserve item can still be evicted the
    // moment space is needed.
    let protect_ids: Vec<String> = report.kept_ids.iter().chain(&report.reserve_ids).cloned().collect();
    let desired = mx::Desired {
        protect: pick(&protect_ids),
        evict: pick(eligible),
        announced: report.announced_ids.clone(),
        collections: titles.clone(),
    };
    let plan = mx::plan_sync(&desired, &observed, owned, &caps);
    let synced = mx::execute(api, plan, &observed, owned, now).await;
    for (action, outcome) in synced.results() {
        println!("[flinch-arrd] {action}: {outcome}");
    }
    // Exactly the verified new adds: their bytes are pending until a recycle
    // bin releases them.
    for (card_id, bytes) in synced.scheduled() {
        let Some(volume) = governance.volume_for(card_id) else { continue };
        let app = match items.get(card_id).map(|item| item.kind) {
            Some(LibraryKind::Movie) => App::Radarr,
            Some(LibraryKind::Season) => App::Sonarr,
            None => continue,
        };
        ledger.record(card_id, app, &volume, bytes, now);
    }
    // Taken back: nothing of theirs is on its way out any more.
    for card_id in synced.unscheduled() {
        ledger.forget(card_id);
    }
    if enforcing {
        if let Err(error) = owned.write(&state_dir()) {
            eprintln!("[flinch-arrd] protected.json/scheduled.json write failed: {error}");
        }
    }
    mx::SyncSummary::new(&synced, !enforcing)
}
