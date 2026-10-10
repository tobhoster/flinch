//! The record of every upgrade search (`state/upgrade-searches.json`): what
//! was searched, when, and whether the item reached its cutoff. It is what
//! the daily cap counts and what keeps an item from being searched again
//! before [`RETRY_SECS`].

use super::{Pick, DAY_SECS};
use crate::capacity::App;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// How long a search has to bring the item up to its cutoff before the
/// record says nothing was found.
pub const OUTCOME_WINDOW_SECS: u64 = 14 * DAY_SECS;
/// How long after a search the item is left to the *arr's own schedule.
pub const RETRY_SECS: u64 = 30 * DAY_SECS;
/// How long a record stays.
pub const LEDGER_KEEP_SECS: u64 = 365 * DAY_SECS;
/// Ledger rows the status carries, newest first.
pub const STATUS_ROWS: usize = 50;

/// What became of one search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SearchOutcome {
    /// Still below its cutoff, within [`OUTCOME_WINDOW_SECS`].
    Pending,
    /// No longer below its cutoff; `bytes` is the file now on disk.
    Upgraded { bytes: u64, at: u64 },
    /// [`OUTCOME_WINDOW_SECS`] passed still below the cutoff.
    NothingFound { at: u64 },
    /// It left the library first.
    Gone { at: u64 },
    /// The command was refused; it may be tried again a day later.
    Failed { reason: String },
}

/// One search (`state/upgrade-searches.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Search {
    pub card_id: String,
    pub title: String,
    pub app: App,
    pub arr_id: u32,
    pub season: Option<u32>,
    /// When it was asked, unix seconds.
    pub at: u64,
    pub p_watch: f64,
    pub from_bytes: u64,
    pub expected_bytes: u64,
    pub outcome: SearchOutcome,
}

/// Every upgrade search FLINCH asked for, kept [`LEDGER_KEEP_SECS`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SearchLedger {
    #[serde(default)]
    pub searches: Vec<Search>,
}

impl SearchLedger {
    /// Record a search: `Ok(())` when the command was accepted, `Err(reason)` when not.
    pub fn record(&mut self, pick: &Pick, at: u64, result: Result<(), String>) {
        let outcome = match result {
            Ok(()) => SearchOutcome::Pending,
            Err(reason) => SearchOutcome::Failed { reason },
        };
        self.searches.push(Search {
            card_id: pick.card_id.clone(),
            title: pick.title.clone(),
            app: pick.app,
            arr_id: pick.arr_id,
            season: pick.season,
            at,
            p_watch: pick.p_watch,
            from_bytes: pick.bytes,
            expected_bytes: pick.expected_bytes,
            outcome,
        });
    }

    /// Searches asked (or tried) since `since`: what the daily cap counts.
    pub fn searched_since(&self, since: u64) -> usize {
        self.searches.iter().filter(|search| search.at >= since).count()
    }

    /// Whether the card was searched within [`RETRY_SECS`] (a failed search:
    /// within a day), or is still pending.
    pub fn blocks(&self, card_id: &str, now: u64) -> bool {
        let within = |at: u64, span: u64| at > now || now - at < span;
        self.searches.iter().filter(|search| search.card_id == card_id).any(|search| match search.outcome {
            SearchOutcome::Pending => true,
            SearchOutcome::Failed { .. } => within(search.at, DAY_SECS),
            _ => within(search.at, RETRY_SECS),
        })
    }

    /// Settle pending searches against this cycle's cutoff-unmet cards and
    /// sizes (card id → bytes). `None`, or an empty inventory, is a failed
    /// read more often than a fact: nothing settles.
    pub fn settle(&mut self, unmet: Option<&HashSet<String>>, sizes: &HashMap<&str, u64>, now: u64) {
        let Some(unmet) = unmet.filter(|_| !sizes.is_empty()) else { return };
        for search in self.searches.iter_mut().filter(|search| search.outcome == SearchOutcome::Pending) {
            search.outcome = match sizes.get(search.card_id.as_str()) {
                None => SearchOutcome::Gone { at: now },
                Some(&bytes) if !unmet.contains(&search.card_id) => SearchOutcome::Upgraded { bytes, at: now },
                Some(_) if now.saturating_sub(search.at) >= OUTCOME_WINDOW_SECS => SearchOutcome::NothingFound { at: now },
                Some(_) => SearchOutcome::Pending,
            };
        }
    }

    /// Forget records older than [`LEDGER_KEEP_SECS`].
    pub fn prune(&mut self, now: u64) {
        self.searches.retain(|search| search.at > now || now - search.at < LEDGER_KEEP_SECS);
    }

    /// Searches that brought their item up to its cutoff.
    pub fn upgraded(&self) -> usize {
        self.searches.iter().filter(|search| matches!(search.outcome, SearchOutcome::Upgraded { .. })).count()
    }

    /// The newest records first, at most [`STATUS_ROWS`].
    pub fn recent(&self) -> Vec<Search> {
        let mut recent: Vec<Search> = self.searches.iter().rev().take(STATUS_ROWS).cloned().collect();
        recent.sort_by_key(|search| std::cmp::Reverse(search.at));
        recent
    }
}
