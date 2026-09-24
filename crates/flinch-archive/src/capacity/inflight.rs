//! Evictions whose bytes are not free yet.
//!
//! Handing an item to Maintainerr frees nothing for days: it sits in the
//! collection until `deleteAfterDays`, and once deleted Radarr/Sonarr move it
//! into a recycle bin *on the same volume* for `recycleBinCleanupDays`. During
//! that last window the item is gone from the library while the disk still
//! reads full — without a ledger a latched volume would pick the next items,
//! then the next, and over-evict by a whole goal per window.
//!
//! The ledger credits those bytes against the volume's goal until the owning
//! app's recycle window has passed, after which the measurement itself shows
//! the space.

use super::App;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// An item still on disk this long after hand-over was never deleted (removed
/// from the collection by hand, or the rule was disabled): stop tracking it.
pub const STALE_ON_DISK_SECS: u64 = 90 * 86_400;

/// One hand-over to Maintainerr.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Eviction {
    pub app: App,
    pub volume: String,
    pub bytes: u64,
    pub handed_at: u64,
    /// When the item was first seen gone from the library; `None` while on disk.
    #[serde(default)]
    pub gone_at: Option<u64>,
}

/// Every eviction whose space may still be occupied, persisted across runs
/// (`state/evictions.json`). A missing or corrupt file reads as empty — the
/// failure mode is one run without credit, never a phantom credit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvictionLedger {
    #[serde(default)]
    pub entries: BTreeMap<String, Eviction>,
}

impl EvictionLedger {
    pub fn read(path: &std::path::Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn write(&self, path: &std::path::Path) -> std::io::Result<()> {
        crate::persist::replace(path, &serde_json::to_vec(self)?)
    }

    /// Record a hand-over. Re-handing an item keeps its original timestamp.
    pub fn record(&mut self, id: &str, app: App, volume: &str, bytes: u64, now: u64) {
        self.entries.entry(id.to_string()).or_insert_with(|| Eviction {
            app,
            volume: volume.to_string(),
            bytes,
            handed_at: now,
            gone_at: None,
        });
    }

    /// Forget a hand-over that was taken back: if the item later leaves disk
    /// for any other reason, its bytes are not FLINCH's eviction to credit.
    pub fn forget(&mut self, id: &str) {
        self.entries.remove(id);
    }

    /// Advance to `now`: stamp items that left the library, forget the ones
    /// whose recycle window has passed (the disk measurement shows them now),
    /// and forget items that stayed on disk implausibly long.
    pub fn observe(&mut self, on_disk: impl Fn(&str) -> bool, recycle_secs: impl Fn(App) -> u64, now: u64) {
        self.entries.retain(|id, eviction| {
            if on_disk(id) {
                // Back on disk (re-downloaded) or never deleted yet: no credit.
                eviction.gone_at = None;
                return now.saturating_sub(eviction.handed_at) < STALE_ON_DISK_SECS;
            }
            let gone_at = *eviction.gone_at.get_or_insert(now);
            now.saturating_sub(gone_at) < recycle_secs(eviction.app)
        });
    }

    /// Bytes per volume that left the library but may still occupy it.
    pub fn pending_bytes(&self) -> BTreeMap<String, u64> {
        let mut pending: BTreeMap<String, u64> = BTreeMap::new();
        for eviction in self.entries.values().filter(|e| e.gone_at.is_some()) {
            let bytes = pending.entry(eviction.volume.clone()).or_insert(0);
            *bytes = bytes.saturating_add(eviction.bytes);
        }
        pending
    }
}

#[cfg(test)]
mod tests;
