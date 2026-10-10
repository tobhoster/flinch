//! The operator's choices (`dupes.json`, written only by flinch-web) and the
//! daemon's record of what it removed (`dupes-acted.json`, written only by
//! flinch-arrd). One writer per file, each replaced atomically.
//!
//! A choice names the copy to keep and the copies the group had when it was
//! made: once a copy appears or vanishes, the choice no longer applies and
//! the operator chooses again. Only a confirmed choice is acted on, and it
//! settles a [`crate::maintainerr::Blocked::SeveralPlexCopies`] hold: the
//! operator said which copy the item is.

use super::Group;
use crate::maintainerr::SyncItem;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const FILE: &str = "dupes.json";
pub const ACTED_FILE: &str = "dupes-acted.json";
/// Removals stay listed this long.
const ACTED_KEEP_SECS: u64 = 30 * 86_400;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    /// The copy to keep.
    pub keep: String,
    /// The group's copy ids when the choice was made, sorted.
    pub copies: Vec<String>,
    /// Chosen, then confirmed: only a confirmed choice is acted on.
    pub confirmed: bool,
    pub decided_at_unix: u64,
}

impl Decision {
    /// Whether the choice still names `group`'s copies.
    pub fn matches(&self, group: &Group) -> bool {
        let mut now: Vec<&str> = group.copies.iter().map(|copy| copy.id.as_str()).collect();
        now.sort_unstable();
        group.copy(&self.keep).is_some() && now.iter().copied().eq(self.copies.iter().map(String::as_str))
    }
}

/// `dupes.json`: group id → choice.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decisions {
    #[serde(default)]
    pub groups: BTreeMap<String, Decision>,
}

#[derive(Debug, thiserror::Error)]
pub enum DecisionError {
    #[error("dupes.json unreadable: {0}")]
    Read(String),
    #[error("{0}")]
    Refused(&'static str),
}

impl Decisions {
    /// A missing file is no choice yet; an unreadable one is an error, so a
    /// write never replaces choices it could not read.
    pub fn read(dir: &Path) -> Result<Self, DecisionError> {
        match std::fs::read(dir.join(FILE)) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| DecisionError::Read(error.to_string())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(DecisionError::Read(error.to_string())),
        }
    }

    pub fn write(&self, dir: &Path) -> std::io::Result<()> {
        crate::persist::replace(&dir.join(FILE), &serde_json::to_vec_pretty(self)?)
    }

    /// Choose `keep` in `group` (confirmed or not), or clear the choice with
    /// `None`. Confirming needs the same copy chosen first, so one click never
    /// deletes. `group` is the daemon's latest view of it.
    pub fn choose(&mut self, group: &Group, keep: Option<&str>, confirm: bool, now: u64) -> Result<(), DecisionError> {
        let Some(keep) = keep else {
            self.groups.remove(&group.id);
            return Ok(());
        };
        if group.copy(keep).is_none() {
            return Err(DecisionError::Refused("that copy is not in this group any more; reload the page"));
        }
        if confirm && !self.groups.get(&group.id).is_some_and(|chosen| chosen.keep == keep && chosen.matches(group)) {
            return Err(DecisionError::Refused("choose the copy to keep first, then confirm it"));
        }
        let mut copies: Vec<String> = group.copies.iter().map(|copy| copy.id.clone()).collect();
        copies.sort_unstable();
        self.groups.insert(group.id.clone(), Decision { keep: keep.to_string(), copies, confirmed: confirm, decided_at_unix: now });
        Ok(())
    }

    /// Attach each group's choice; a choice for a group gone is left in the
    /// file (the copies may reappear), but shows nowhere.
    pub fn attach(&self, groups: &mut [Group]) {
        for group in groups {
            group.decision = self.groups.get(&group.id).cloned();
        }
    }
}

/// Turn the `SeveralPlexCopies` hold of every movie with a confirmed choice
/// of a Plex copy into a decided item: the sync acts on the kept copy only.
/// Returns how many items were settled.
pub fn settle(items: &mut [SyncItem], groups: &[Group]) -> usize {
    let mut settled = 0;
    for group in groups {
        let Some(decision) = group.confirmed() else { continue };
        let (Some(card), Some(keep)) = (group.card_id.as_deref(), group.copy(&decision.keep)) else { continue };
        let Some(rating_key) = keep.rating_key.as_deref() else { continue };
        for item in items.iter_mut().filter(|item| item.card_id == card && item.kind == crate::card::LibraryKind::Movie) {
            let Some(plex) = item.plex.as_mut() else { continue };
            if item.copies.iter().all(|key| key == rating_key) && plex.rating_key == rating_key {
                continue;
            }
            plex.rating_key = rating_key.to_string();
            plex.section_id = keep.section_id.or(plex.section_id);
            item.copies = vec![rating_key.to_string()];
            settled += 1;
        }
    }
    settled
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Removed, and the read-back shows the kept copy still there.
    Removed,
    /// A dry run printed the removal.
    Simulated,
    /// Refused or not verified; not tried again until the choice changes.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Acted {
    pub group: String,
    pub copy: String,
    pub title: String,
    pub bytes: u64,
    pub outcome: Outcome,
    #[serde(default)]
    pub detail: String,
    pub at_unix: u64,
}

/// `dupes-acted.json`, newest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActedLog {
    #[serde(default)]
    pub entries: Vec<Acted>,
}

impl ActedLog {
    /// Missing or unreadable reads as empty: at worst a failed removal is
    /// tried once more, and every removal re-checks the copies first.
    pub fn read(dir: &Path) -> Self {
        std::fs::read(dir.join(ACTED_FILE)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
    }

    pub fn write(&self, dir: &Path) -> std::io::Result<()> {
        crate::persist::replace(&dir.join(ACTED_FILE), &serde_json::to_vec_pretty(self)?)
    }

    /// Whether `copy` was removed or failed since the choice was made.
    pub fn settled_since(&self, copy: &str, since: u64) -> bool {
        self.entries.iter().any(|entry| entry.copy == copy && entry.outcome != Outcome::Simulated && entry.at_unix >= since)
    }

    /// Record `entry` first, dropping entries older than 30 days.
    pub fn push(&mut self, entry: Acted) {
        let horizon = entry.at_unix.saturating_sub(ACTED_KEEP_SECS);
        self.entries.insert(0, entry);
        self.entries.retain(|entry| entry.at_unix >= horizon);
    }
}
