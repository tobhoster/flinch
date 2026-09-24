//! What FLINCH owns in Maintainerr: the exclusions it created and the
//! collection memberships it added, each verified by read-back. Everything
//! else in Maintainerr belongs to the operator.
//!
//! Two JSON files live on the state volume, `protected.json` and
//! `scheduled.json`. A missing or corrupt file reads as empty. So does the
//! legacy `protected.json`, a bare list of card ids: none of those exclusions
//! ever landed (they carried card ids, not ratingKeys), so FLINCH owns nothing
//! from them. A third, `operator-keeps.json`, remembers what the operator's own
//! exclusions keep, for cycles that cannot read Maintainerr.

use super::{MaintainerrTarget, Observed};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const PROTECTED_FILE: &str = "protected.json";
const SCHEDULED_FILE: &str = "scheduled.json";
const OPERATOR_KEEPS_FILE: &str = "operator-keeps.json";

/// A target as persisted: `rating_key` is the movie's ratingKey, or the
/// show's for a season, exactly as in [`crate::ids::PlexIds`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Keys {
    rating_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    season_rating_key: Option<String>,
}

impl From<MaintainerrTarget> for Keys {
    fn from(target: MaintainerrTarget) -> Self {
        match target {
            MaintainerrTarget::Movie { rating_key } => Self { rating_key, season_rating_key: None },
            MaintainerrTarget::Season { show_rating_key, season_rating_key } => {
                Self { rating_key: show_rating_key, season_rating_key: Some(season_rating_key) }
            }
        }
    }
}

impl TryFrom<Keys> for MaintainerrTarget {
    type Error = &'static str;

    fn try_from(keys: Keys) -> Result<Self, Self::Error> {
        let blank = |key: &str| key.trim().is_empty();
        match keys.season_rating_key {
            _ if blank(&keys.rating_key) => Err("blank rating_key"),
            None => Ok(Self::Movie { rating_key: keys.rating_key }),
            Some(season) if blank(&season) => Err("blank season_rating_key"),
            Some(season) => Ok(Self::Season { show_rating_key: keys.rating_key, season_rating_key: season }),
        }
    }
}

/// Exclusion rows FLINCH created for one card and verified by read-back: a
/// movie's row, or a season's row plus its episodes' rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtectedEntry {
    #[serde(flatten)]
    pub target: MaintainerrTarget,
    pub exclusion_ids: Vec<i64>,
}

/// A collection membership FLINCH added and verified by read-back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledEntry {
    #[serde(flatten)]
    pub target: MaintainerrTarget,
    pub collection_id: i64,
    /// Unix seconds of the verified add.
    pub added_at: u64,
}

/// Card id → what FLINCH owns for it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnedState {
    pub protected: BTreeMap<String, ProtectedEntry>,
    pub scheduled: BTreeMap<String, ScheduledEntry>,
}

impl OwnedState {
    /// Fail-safe read of both files from the state directory.
    pub fn read(dir: &Path) -> Self {
        Self { protected: read_map(&dir.join(PROTECTED_FILE)), scheduled: read_map(&dir.join(SCHEDULED_FILE)) }
    }

    /// Write both files, each atomically (temp file, then rename), so a crash
    /// never leaves a half-written file that would read as "owns nothing".
    pub fn write(&self, dir: &Path) -> std::io::Result<()> {
        write_atomic(&dir.join(PROTECTED_FILE), &self.protected)?;
        write_atomic(&dir.join(SCHEDULED_FILE), &self.scheduled)
    }

    /// Whether a FLINCH exclusion currently protects the card.
    pub fn is_protected(&self, card_id: &str) -> bool {
        self.protected.contains_key(card_id)
    }

    /// Every exclusion row id FLINCH owns. Any other row is the operator's.
    pub fn exclusion_ids(&self) -> BTreeSet<i64> {
        self.protected.values().flat_map(|entry| entry.exclusion_ids.iter().copied()).collect()
    }

    /// Drop what Maintainerr no longer holds: rows gone from an observed key,
    /// and memberships gone from an observed collection (deleted by its
    /// schedule, or removed by the operator). Anything unobserved stays.
    pub(super) fn prune(&mut self, observed: &Observed) {
        self.scheduled.retain(|_, entry| {
            observed.members.get(&entry.collection_id).is_none_or(|members| members.contains(entry.target.item_key()))
        });
        self.protected.retain(|_, entry| {
            if let Some(rows) = observed.exclusions.get(entry.target.media_id()) {
                entry.exclusion_ids.retain(|id| rows.iter().any(|row| row.id == *id));
            }
            !entry.exclusion_ids.is_empty()
        });
    }

    pub(super) fn forget_exclusion(&mut self, card_id: &str, exclusion_id: i64) {
        if let Some(entry) = self.protected.get_mut(card_id) {
            entry.exclusion_ids.retain(|id| *id != exclusion_id);
            if entry.exclusion_ids.is_empty() {
                self.protected.remove(card_id);
            }
        }
    }
}

/// The cards the operator's own exclusions keep, as last observed. A cycle that
/// cannot read Maintainerr plans with these, so an outage never turns an
/// operator's keeper into a candidate. Missing or corrupt reads as none.
pub fn read_operator_keeps(dir: &Path) -> BTreeSet<String> {
    std::fs::read_to_string(dir.join(OPERATOR_KEEPS_FILE)).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

pub fn write_operator_keeps(dir: &Path, keeps: &BTreeSet<String>) -> std::io::Result<()> {
    write_atomic(&dir.join(OPERATOR_KEEPS_FILE), keeps)
}

fn read_map<T: DeserializeOwned>(path: &Path) -> BTreeMap<String, T> {
    std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

fn write_atomic<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    crate::persist::replace(path, &serde_json::to_vec_pretty(value)?)
}
