//! The native lifecycle, pure: this cycle's decisions, the Leaving Soon shelf
//! as FLINCH recorded it, and the watch evidence become an ordered list of
//! [`Action`]s. No I/O, so every rule is tested directly.
//!
//! The rules:
//! - On the shelf: an item gone from the library, pinned, played since its
//!   announcement, no longer selected, or announced under another shelf title
//!   is taken back. Without evidence this cycle it is held: neither deleted
//!   nor taken back. Once its window has run out it is deleted, but only when
//!   this cycle's evidence was read in full (a play the read missed must not
//!   lose the household its claim), within `max_deletes`, oldest window first.
//! - Past the grace runs (`eligible`, in plan order): a finished or duplicate
//!   item is deleted at once; one nobody finished is announced, or held while
//!   there is no shelf. New actions fit the per-run caps; deletes also fit
//!   `max_deletes`. The first that does not fit defers the rest, and an item
//!   whose predecessor waits this cycle waits too, so a show never loses a
//!   later season before the one it must follow.
//! - Pinned items are never touched; an item without evidence never acts.

use super::state::NativeState;
use crate::maintainerr::Caps;
use std::collections::{HashMap, HashSet};

/// One eviction past its grace runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item<'a> {
    pub id: &'a str,
    pub bytes: u64,
    /// Nobody finished it: it is announced before it goes.
    pub announce: bool,
    /// The item that must leave first.
    pub after: Option<&'a str>,
}

/// This cycle as the lifecycle needs it.
pub struct Inputs<'a> {
    /// Past the grace runs, in plan order.
    pub eligible: &'a [Item<'a>],
    /// Every card this cycle's plan takes.
    pub selected: &'a HashSet<&'a str>,
    /// Keeps and protected cards: never touched.
    pub pinned: &'a HashSet<&'a str>,
    /// Every card on disk in this cycle's complete *arr read.
    pub in_library: &'a HashSet<&'a str>,
    /// Card → its last play (epoch seconds, `None` never played), for every
    /// card this cycle's watch read covered. A missing key is no evidence.
    pub evidence: &'a HashMap<&'a str, Option<u64>>,
    /// Every configured watch source was read in full this cycle.
    pub complete: bool,
    /// The Leaving Soon title, when items can be announced (its server
    /// configured and a title set); `None` holds every announcement.
    pub shelf: Option<&'a str>,
    /// The server showing the shelf; an item shelved on another moves.
    pub server: super::ShelfServer,
    pub caps: Caps,
    pub max_deletes: usize,
    pub window_secs: u64,
    pub now: u64,
}

/// How a delete is justified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Finished, or another copy stays: no warning needed.
    Finished,
    /// Announced, and its window ran out unplayed.
    LeavingSoon,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Withdrawn {
    Played,
    NotSelected,
    Pinned,
    Gone,
    /// The shelf title changed (or was cleared): it moves, its window restarts.
    ShelfChanged,
}

impl std::fmt::Display for Withdrawn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Played => "played during its window",
            Self::NotSelected => "no longer selected",
            Self::Pinned => "pinned",
            Self::Gone => "gone from the library",
            Self::ShelfChanged => "the Leaving Soon title or server changed",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    /// Nobody finished it and there is nowhere to announce it.
    NoShelf,
    /// This cycle read no watch evidence for it.
    NoEvidence,
    /// Its window ran out, but a watch source was not read in full.
    EvidenceIncomplete,
}

impl std::fmt::Display for Held {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NoShelf => "nobody finished it and Leaving Soon is unavailable (its server or the title is not set)",
            Self::NoEvidence => "no watch evidence this cycle",
            Self::EvidenceIncomplete => "its window ran out, but the watch history was not read in full this cycle",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Announce { id: String, until: u64 },
    Withdraw { id: String, why: Withdrawn },
    Delete { id: String, route: Route },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// Withdrawals first, then deletes of expired windows, then this cycle's
    /// new actions in plan order.
    pub actions: Vec<Action>,
    pub held: Vec<(String, Held)>,
    /// Ready, but over a cap or behind a waiting predecessor.
    pub deferred: Vec<String>,
}

pub fn plan(state: &NativeState, inputs: &Inputs) -> Plan {
    let mut out = Plan::default();
    let mut deletes = 0usize;
    let mut expired = Vec::new();

    let mut shelf: Vec<(&String, &super::Leaving)> = state.leaving.iter().collect();
    shelf.sort_by_key(|(id, entry)| (entry.until, id.as_str()));
    for (id, entry) in shelf {
        let key = id.as_str();
        let withdraw = |why| Action::Withdraw { id: id.clone(), why };
        if !inputs.in_library.contains(key) {
            out.actions.push(withdraw(Withdrawn::Gone));
            continue;
        }
        if inputs.pinned.contains(key) {
            out.actions.push(withdraw(Withdrawn::Pinned));
            continue;
        }
        match inputs.evidence.get(key) {
            None => {
                out.held.push((id.clone(), Held::NoEvidence));
                continue;
            }
            Some(Some(played)) if *played > entry.announced_at => {
                out.actions.push(withdraw(Withdrawn::Played));
                continue;
            }
            Some(_) => {}
        }
        if inputs.shelf != Some(entry.shelf.as_str()) || inputs.server != entry.server {
            out.actions.push(withdraw(Withdrawn::ShelfChanged));
            continue;
        }
        if !inputs.selected.contains(key) {
            out.actions.push(withdraw(Withdrawn::NotSelected));
            continue;
        }
        if inputs.now < entry.until {
            continue;
        }
        if !inputs.complete {
            out.held.push((id.clone(), Held::EvidenceIncomplete));
        } else if deletes < inputs.max_deletes {
            deletes += 1;
            expired.push(Action::Delete { id: id.clone(), route: Route::LeavingSoon });
        } else {
            out.deferred.push(id.clone());
        }
    }
    out.actions.extend(expired);

    let (mut added, mut added_bytes, mut capped) = (0usize, 0u64, false);
    let mut waiting: HashSet<&str> = HashSet::new();
    for item in inputs.eligible {
        if state.leaving.contains_key(item.id) || inputs.pinned.contains(item.id) {
            continue;
        }
        if !inputs.evidence.contains_key(item.id) {
            out.held.push((item.id.to_string(), Held::NoEvidence));
            waiting.insert(item.id);
            continue;
        }
        if item.announce && inputs.shelf.is_none() {
            out.held.push((item.id.to_string(), Held::NoShelf));
            waiting.insert(item.id);
            continue;
        }
        let behind = item.after.is_some_and(|after| waiting.contains(after));
        let fits = inputs.caps.admits(added, added_bytes, item.bytes) && (item.announce || deletes < inputs.max_deletes);
        if capped || behind || !fits {
            capped |= !fits;
            out.deferred.push(item.id.to_string());
            waiting.insert(item.id);
            continue;
        }
        added += 1;
        added_bytes = added_bytes.saturating_add(item.bytes);
        out.actions.push(if item.announce {
            Action::Announce { id: item.id.to_string(), until: inputs.now.saturating_add(inputs.window_secs) }
        } else {
            deletes += 1;
            Action::Delete { id: item.id.to_string(), route: Route::Finished }
        });
    }
    out
}
