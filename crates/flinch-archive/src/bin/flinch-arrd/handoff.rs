//! The hand-off to Maintainerr, once per cycle: pinned items and items someone
//! is partway through become exclusions, evictions past the grace window join
//! a collection (in plan order, within the caps): Leaving Soon when nobody
//! finished them, their kind's delete collection otherwise. FLINCH's
//! exclusions on items gone from the library and Plex are released. An
//! enforcing run books in the eviction ledger every membership FLINCH made
//! that Maintainerr still holds, while its item is in the library on a
//! governed disk and younger than the stale limit. Every take-back is
//! forgotten there.

use super::sink::Sink;
use super::state_dir;
use flinch_archive::capacity::{App, EvictionLedger, HandedOver};
use flinch_archive::govern::Governance;
use flinch_archive::maintainerr::{self as mx, MaintainerrError, OwnedState, SyncItem};
use flinch_archive::{LibraryKind, ReconcileOutput};
use std::collections::{HashMap, HashSet};

/// This cycle's decisions, as the hand-off needs them.
pub(super) struct Handoff<'a> {
    /// Every card with its Plex ids, by card id.
    pub(super) items: &'a HashMap<&'a str, &'a SyncItem>,
    /// Card id → title, booked with each hand-over for the operator.
    pub(super) names: &'a HashMap<&'a str, &'a str>,
    pub(super) report: &'a ReconcileOutput,
    /// Evictions past the grace window, in eviction order.
    pub(super) eligible: &'a [String],
    pub(super) titles: &'a mx::CollectionTitles,
    pub(super) caps: mx::Caps,
    pub(super) enforcing: bool,
    pub(super) now: u64,
    /// Every ratingKey Plex listed completely this cycle; `None` proves nothing
    /// gone, so no exclusion is released.
    pub(super) plex_listed: Option<&'a HashSet<String>>,
    /// Whether Maintainerr has Seerr configured; `None` when unread.
    pub(super) seerr: Option<bool>,
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
    let Handoff { items, names, report, eligible, titles, caps, enforcing, now, plex_listed, seerr } = handoff;
    let observed = match observed {
        Ok(observed) => observed,
        Err(error) => return mx::SyncSummary::unavailable(&error, !enforcing, operator_keeps),
    };
    let pick =
        |ids: &[String]| -> Vec<SyncItem> { ids.iter().filter_map(|id| items.get(id.as_str()).map(|item| (*item).clone())).collect() };
    // Exclusions only for what must never go (pinned, or someone partway
    // through). Everything else is left to the operator's own Maintainerr
    // rules: FLINCH no longer shields an item merely because it kept it.
    let desired = mx::Desired {
        protect: pick(&report.protected_ids),
        evict: pick(eligible),
        announced: report.announced_ids.clone(),
        collections: titles.clone(),
        gone: plex_listed.map(|listed| owned.vanished(|id| items.contains_key(id), listed)).unwrap_or_default(),
        unresolved: items.values().filter(|item| item.plex.is_none()).map(|item| item.card_id.clone()).collect(),
        seerr_configured: seerr == Some(true),
    };
    let plan = mx::plan_sync(&desired, &observed, owned, &caps);
    let synced = mx::execute(api, plan, &observed, owned, now).await;
    for (action, outcome) in synced.results() {
        println!("[flinch-arrd] {action}: {outcome}");
    }
    // Taken back: nothing of theirs is on its way out any more. A move to
    // another collection is not a take-back.
    for card_id in synced.taken_back() {
        ledger.forget(card_id);
    }
    if enforcing {
        // Every membership FLINCH made that Maintainerr still holds is a
        // hand-over, booked at its add: this cycle's adds, and any the ledger
        // lost. Only an enforcing sync prunes `owned` to what Maintainerr holds.
        for (card_id, entry) in &owned.scheduled {
            let Some(item) = items.get(card_id.as_str()) else { continue };
            let Some(volume) = governance.volume_for(card_id) else { continue };
            let app = match item.kind {
                LibraryKind::Movie => App::Radarr,
                LibraryKind::Season => App::Sonarr,
            };
            let title = names.get(card_id.as_str()).copied().unwrap_or_default();
            ledger.book(HandedOver { id: card_id, title, app, volume: &volume, bytes: item.bytes }, entry.added_at, now);
        }
        if let Err(error) = owned.write(&state_dir()) {
            eprintln!("[flinch-arrd] protected.json/scheduled.json write failed: {error}");
        }
    }
    mx::SyncSummary::new(&synced, !enforcing)
}
