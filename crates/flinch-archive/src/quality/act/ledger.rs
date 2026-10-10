//! The record of every move (`state/quality-actions.json`): what was moved,
//! when, and whether a smaller file landed. It is what the daily cap counts
//! and what keeps an item from being moved twice.

use super::{Move, DAY_SECS};
use crate::capacity::App;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A file at most this share of the one moved reads as a smaller file landed.
pub const LANDED_SHARE: f64 = 0.90;
/// How long a move has to land a smaller file before the record says none did.
pub const OUTCOME_WINDOW_SECS: u64 = 14 * DAY_SECS;
/// How long a record stays: an item is never moved twice while it does.
pub const LEDGER_KEEP_SECS: u64 = 365 * DAY_SECS;
/// Ledger rows the status carries, newest first.
pub const STATUS_ROWS: usize = 50;

/// What became of one moved item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Outcome {
    /// Waiting for a smaller file.
    Pending,
    /// A file at most [`LANDED_SHARE`] of the old one is on disk.
    Landed { bytes: u64, at: u64 },
    /// [`OUTCOME_WINDOW_SECS`] passed without one.
    NothingSmaller { at: u64 },
    /// It left the library before a smaller file landed.
    Gone { at: u64 },
    /// The move was refused or did not read back; it is tried again a day later.
    Failed { reason: String },
}

/// One moved item (`state/quality-actions.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Action {
    pub card_id: String,
    pub title: String,
    pub app: App,
    pub arr_id: u32,
    pub season: Option<u32>,
    /// When it was moved, unix seconds.
    pub at: u64,
    pub from_bytes: u64,
    pub from_profile: u32,
    pub to_profile: u32,
    pub release_bytes: Option<u64>,
    /// Whether the search command was accepted.
    pub searched: bool,
    pub outcome: Outcome,
}

/// Every move FLINCH made, kept [`LEDGER_KEEP_SECS`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionLedger {
    #[serde(default)]
    pub actions: Vec<Action>,
}

impl ActionLedger {
    /// Record a move's items: `Ok(searched)` when the profile read back
    /// moved, `Err(reason)` when it did not.
    pub fn record(&mut self, movement: &Move, at: u64, result: Result<bool, String>) {
        for card in &movement.cards {
            let (searched, outcome) = match &result {
                Ok(searched) => (*searched, Outcome::Pending),
                Err(reason) => (false, Outcome::Failed { reason: reason.clone() }),
            };
            self.actions.push(Action {
                card_id: card.card_id.clone(),
                title: card.title.clone(),
                app: movement.app,
                arr_id: movement.id,
                season: card.season,
                at,
                from_bytes: card.bytes,
                from_profile: movement.from_profile,
                to_profile: movement.to_profile,
                release_bytes: card.release_bytes,
                searched,
                outcome,
            });
        }
    }

    /// Items moved (or tried) since `since`: what the daily cap counts.
    pub fn acted_since(&self, since: u64) -> usize {
        self.actions.iter().filter(|action| action.at >= since).count()
    }

    /// Whether the card was moved before, or failed to move within a day.
    pub fn blocks(&self, card_id: &str, now: u64) -> bool {
        self.actions.iter().filter(|action| action.card_id == card_id).any(|action| match action.outcome {
            Outcome::Failed { .. } => action.at > now || now - action.at < DAY_SECS,
            _ => true,
        })
    }

    /// Settle pending moves against today's sizes (card id → bytes). An empty
    /// inventory is a failed read more often than an empty library: nothing settles.
    pub fn settle(&mut self, sizes: &HashMap<&str, u64>, now: u64) {
        if sizes.is_empty() {
            return;
        }
        for action in self.actions.iter_mut().filter(|action| action.outcome == Outcome::Pending) {
            action.outcome = match sizes.get(action.card_id.as_str()) {
                None => Outcome::Gone { at: now },
                Some(&bytes) if bytes as f64 <= action.from_bytes as f64 * LANDED_SHARE => Outcome::Landed { bytes, at: now },
                Some(_) if now.saturating_sub(action.at) >= OUTCOME_WINDOW_SECS => Outcome::NothingSmaller { at: now },
                Some(_) => Outcome::Pending,
            };
        }
    }

    /// Forget records older than [`LEDGER_KEEP_SECS`].
    pub fn prune(&mut self, now: u64) {
        self.actions.retain(|action| action.at > now || now - action.at < LEDGER_KEEP_SECS);
    }

    /// Bytes the landed moves gave back.
    pub fn reclaimed_bytes(&self) -> u64 {
        let freed = |action: &Action| match action.outcome {
            Outcome::Landed { bytes, .. } => action.from_bytes.saturating_sub(bytes),
            _ => 0,
        };
        self.actions.iter().map(freed).sum()
    }

    /// The newest records first, at most [`STATUS_ROWS`].
    pub fn recent(&self) -> Vec<Action> {
        let mut recent: Vec<Action> = self.actions.iter().rev().take(STATUS_ROWS).cloned().collect();
        recent.sort_by_key(|action| std::cmp::Reverse(action.at));
        recent
    }
}
