//! Who acts on the plan, and the native executor's I/O. Grace streaks count
//! only cycles that can act ([`eligible`]); with `executor: native` the pure
//! lifecycle ([`flinch_archive::executor::lifecycle`]) then decides from the
//! plan, the shelf recorded in `native.json` and this cycle's evidence;
//! [`act`] announces, takes back and deletes; [`restore`] runs the undo queue;
//! [`shelf`] then orders and dates the Plex shelf, keeps poster badges in step
//! and removes FLINCH's emptied collections. A dry run reads everything,
//! prints every write, and records nothing.

mod act;
mod look;
mod restore;
mod shelf;

use super::{evidence, state_dir, Args};
use flinch_archive::capacity::EvictionLedger;
use flinch_archive::daemon::RuntimeSettings;
use flinch_archive::executor::lifecycle::{self, Inputs, Item};
use flinch_archive::executor::{acting, Acting, Executor, NativeState, NativeStatus, ShelfServer};
use flinch_archive::govern::Governance;
use flinch_archive::jellyfin::JellyfinClient;
use flinch_archive::maintainerr::{Caps, OwnedState, Route, SyncItem};
use flinch_archive::plan::candidates;
use flinch_archive::torrents::{Holding, Torrents};
use flinch_archive::watch::EvidenceHealth;
use flinch_archive::{ItemSnapshot, ReconcileOutput};
use std::collections::{HashMap, HashSet};

/// This cycle, as the native executor needs it.
pub(super) struct Cycle<'a> {
    pub(super) args: &'a Args,
    pub(super) http: &'a reqwest::Client,
    pub(super) settings: &'a RuntimeSettings,
    /// Cards (keeps already marked), inventory, watch evidence, and `now`:
    /// when the evidence was read, so a play after it was missed.
    pub(super) library: &'a candidates::Library<'a>,
    pub(super) report: &'a ReconcileOutput,
    /// Evictions past the grace runs, in plan order.
    pub(super) eligible: &'a [String],
    pub(super) plex_ids: &'a HashMap<String, flinch_archive::ids::PlexIds>,
    /// Card id → Jellyfin/Emby item id (movie or season).
    pub(super) jellyfin_ids: &'a HashMap<String, String>,
    pub(super) health: &'a EvidenceHealth,
    pub(super) governance: &'a Governance,
    pub(super) torrents: (Option<&'a Torrents>, &'a HashMap<String, Vec<Holding>>),
    pub(super) dry_run: bool,
}

/// What may act this cycle: items past their grace streak, in plan order. A
/// cycle that cannot act is not an appearance: one that cannot read
/// Maintainerr hands nothing over and planned without the operator's
/// exclusions, so counting it would let an outage run down an item's grace
/// window. The native executor needs no Maintainerr.
pub(super) fn eligible(
    settings: &RuntimeSettings,
    maintainerr_readable: bool,
    report: &ReconcileOutput,
    handed: &HashSet<String>,
    interval_s: u64,
    now: u64,
    dry_run: bool,
) -> anyhow::Result<Vec<String>> {
    if dry_run {
        println!("[flinch-arrd] dry run: plan written, every write is printed and none is sent");
    }
    if acting(settings.executor, maintainerr_readable) == Acting::Nobody {
        return Ok(Vec::new());
    }
    let path = state_dir().join("candidates.json");
    let mut streaks = flinch_archive::daemon::read_candidate_state(&path);
    let grace = flinch_archive::daemon::Grace { runs: settings.grace_runs, interval_s };
    let past_grace = flinch_archive::daemon::advance_streaks(&mut streaks, &report.deleted_ids, grace, now);
    flinch_archive::daemon::write_candidate_state(&path, &streaks)?;
    Ok(report.plan.releasable(&past_grace, handed))
}

/// What FLINCH has handed over and still answers for: the shelf when native,
/// Maintainerr memberships otherwise. The plan takes these first, so a
/// cheaper newcomer never restarts a window.
pub(super) fn handed(executor: Executor, owned: &OwnedState) -> HashSet<String> {
    match executor {
        Executor::Native => NativeState::read(&state_dir()).leaving.into_keys().collect(),
        Executor::Maintainerr => owned.scheduled.keys().cloned().collect(),
    }
}

/// [`handed`] with each item's bytes, for the capacity status.
pub(super) fn handed_bytes<'a>(executor: Executor, owned: &OwnedState, items: &HashMap<&'a str, &SyncItem>) -> Vec<(&'a str, u64)> {
    handed(executor, owned).iter().filter_map(|id| items.get_key_value(id.as_str()).map(|(id, item)| (*id, item.bytes))).collect()
}

/// Shelf items read as Maintainerr's Leaving Soon members do: route, when
/// announced and when they may leave.
pub(super) fn annotate(items: &mut [ItemSnapshot], status: Option<&NativeStatus>) {
    let Some(status) = status else { return };
    let shelf: HashMap<&str, _> = status.leaving.iter().map(|entry| (entry.id.as_str(), entry)).collect();
    for item in items {
        if let Some(entry) = shelf.get(item.id.as_str()) {
            item.route = Some(Route::LeavingSoon);
            item.handed_at = Some(entry.announced_at);
            item.leaves_at = Some(entry.until);
        }
    }
}

pub(super) async fn run(cycle: Cycle<'_>, ledger: &mut EvictionLedger) -> NativeStatus {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut state = NativeState::read(&state_dir());
    let settings = cycle.settings;
    let config = &settings.native;
    state.prune(now);
    let plex = evidence::plex_connection(settings);
    let jellyfin = jellyfin_client(settings);
    let server = config.leaving_soon_server;
    let reachable = match server {
        ShelfServer::Plex => plex.is_some(),
        ShelfServer::Jellyfin => jellyfin.is_some(),
    };
    let title = settings.collection_leaving.trim();
    let shelf = (reachable && !title.is_empty()).then_some(title);

    let cards = cycle.library.cards;
    let in_library: HashSet<&str> = cards.iter().map(|card| card.id.as_str()).collect();
    let selected: HashSet<&str> = cycle.report.deleted_ids.iter().map(String::as_str).collect();
    // Keeps are marked on the cards before planning (favorites, keep tag,
    // keep collections, the operator's Maintainerr exclusions).
    let pinned: HashSet<&str> = cards
        .iter()
        .filter(|card| card.in_keep_collection || card.is_favorite)
        .map(|card| card.id.as_str())
        .chain(cycle.report.protected_ids.iter().map(String::as_str))
        .collect();
    let evidence: HashMap<&str, Option<u64>> =
        cycle.library.watch.iter().map(|(id, entry)| (id.as_str(), entry.last_watched_epoch)).collect();
    let planned: HashMap<&str, &flinch_archive::PlanItem> = cycle.report.plan.items.iter().map(|item| (item.id.as_str(), item)).collect();
    let eligible: Vec<Item> = cycle
        .eligible
        .iter()
        .filter_map(|id| planned.get(id.as_str()))
        .map(|item| Item { id: &item.id, bytes: item.size_bytes, announce: item.announce, after: item.after.as_deref() })
        .collect();
    let plan = lifecycle::plan(
        &state,
        &Inputs {
            eligible: &eligible,
            selected: &selected,
            pinned: &pinned,
            in_library: &in_library,
            evidence: &evidence,
            complete: cycle.health.never_played_reclaim_safe(),
            shelf,
            server,
            caps: Caps::new(settings.max_items, settings.max_gib),
            max_deletes: config.max_deletes_per_run,
            window_secs: config.window_secs(),
            now,
        },
    );

    let names: HashMap<&str, &str> = cards.iter().map(|card| (card.id.as_str(), card.title.as_str())).collect();
    let name =
        |id: &str| names.get(id).copied().or_else(|| state.leaving.get(id).map(|entry| entry.title.as_str())).unwrap_or(id).to_string();
    let mut status = NativeStatus {
        dry_run: cycle.dry_run,
        leaving_route: shelf.is_some(),
        window_days: config.leaving_soon_days,
        deferred: plan.deferred.len(),
        held: plan.held.iter().map(|(id, why)| format!("{}: {why}", name(id))).collect(),
        ..NativeStatus::default()
    };
    if shelf.is_none() {
        status.problems.push(if !reachable {
            format!("Leaving Soon is set to {}, which is not configured: items nobody finished are held", server.label())
        } else {
            "the Leaving Soon title is blank: items nobody finished are held".to_string()
        });
    }

    let plex_shelf = plex.as_ref().map(|(url, token)| shelf::PlexShelf {
        plex: flinch_archive::plex::collections::PlexCollections::new(cycle.http, url, token, cycle.dry_run),
        settings,
        plex_ids: cycle.plex_ids,
        now,
        dry_run: cycle.dry_run,
    });
    let actor =
        act::Actor::new(&cycle, plex.as_ref().map(|(url, token)| (url.as_str(), token.as_str())), jellyfin.as_ref(), (shelf, server), now);
    if let Some(tidy) = &plex_shelf {
        tidy.before_leaving(&plan.actions, &state, &mut status).await;
    }
    actor.execute(plan.actions, &mut state, ledger, &mut status).await;
    restore::process(&actor, &mut state, &mut status, now).await;
    if let Some(tidy) = &plex_shelf {
        tidy.tidy(&mut state, &mut status).await;
    }
    for problem in &status.problems {
        eprintln!("[flinch-arrd] native: {problem}");
    }
    if !cycle.dry_run {
        if let Err(error) = state.write(&state_dir()) {
            eprintln!("[flinch-arrd] native.json write failed: {error}");
        }
    }
    status.lists(&state, &flinch_archive::executor::state::pending_restores(&state_dir()));
    status
}

/// The Jellyfin/Emby client for shelf writes and the last look; `None` when
/// unconfigured or keyless (the evidence read reports why).
fn jellyfin_client(settings: &RuntimeSettings) -> Option<JellyfinClient> {
    let config = &settings.jellyfin;
    let key = config.api_key().filter(|_| config.enabled())?;
    JellyfinClient::new(&evidence::with_scheme(config.url.trim()), &key, config.kind)
        .map_err(|error| eprintln!("[flinch-arrd] native: {} client: {error}", config.kind.label()))
        .ok()
}
