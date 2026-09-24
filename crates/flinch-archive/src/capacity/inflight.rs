//! Evictions whose bytes are not free yet — and whether the disk ever showed
//! them freed.
//!
//! Handing an item to Maintainerr frees nothing for days: it sits in the
//! collection until `deleteAfterDays`, and once deleted Radarr/Sonarr move it
//! into a recycle bin *on the same volume* for `recycleBinCleanupDays`. During
//! that last window the item is gone from the library while the disk still
//! reads full — without a ledger a latched volume would pick the next items,
//! then the next, and over-evict by a whole goal per window.
//!
//! The ledger credits those bytes against the volume's goal until the owning
//! app's recycle window has passed, then checks that the space came back. Per
//! volume it follows *other*: used bytes minus library media minus every
//! credited eviction. FLINCH's own evictions never move it — leaving the
//! library moves an item's bytes from library to credit — so the bin emptying
//! shows as a drop of the item's size. Seen within [`SETTLE_GRACE_SECS`] of
//! the window's end, the credit ends. Not seen, something else still holds the
//! bytes (a torrent seeding the same hardlinked file, a snapshot): the eviction
//! is *held*, stays credited so nothing more is evicted for it, and is
//! reported until the drop shows or [`HELD_CREDIT_SECS`] pass.
//!
//! Downloads in flight raise *other* and can hide a drop for a while: the
//! error is then a held report and less eviction, never more. Imports lower it
//! and can fake a drop: the credit then ends at the window, as it always did.

use super::App;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// An item still on disk this long after hand-over was never deleted (removed
/// from the collection by hand, or the rule was disabled): stop tracking it.
pub const STALE_ON_DISK_SECS: u64 = 90 * 86_400;

/// How long a hand-over is remembered once its eviction is no longer tracked,
/// to tell FLINCH's deletions from everyone else's: a Maintainerr window of
/// up to [`STALE_ON_DISK_SECS`], plus the days the list of deletions FLINCH
/// did not make looks back.
pub const HANDOFF_MEMORY_SECS: u64 = STALE_ON_DISK_SECS + crate::outside::WINDOW_SECS;

/// How long after its recycle window an eviction's drop may still show. The
/// *arrs empty their bins once a day, so two days leave a full day of margin.
pub const SETTLE_GRACE_SECS: u64 = 2 * 86_400;

/// How long held bytes stay credited. Past it, FLINCH stops waiting for the
/// space and the measurement alone drives eviction again.
pub const HELD_CREDIT_SECS: u64 = 14 * 86_400;

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
    /// When the grace after its recycle window ran out with no drop on disk.
    #[serde(default)]
    pub held_since: Option<u64>,
    /// The item's title at hand-over, for the operator.
    #[serde(default)]
    pub title: String,
}

/// What one hand-over the ledger records carries.
#[derive(Debug, Clone, Copy)]
pub struct HandedOver<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub app: App,
    pub volume: &'a str,
    pub bytes: u64,
}

/// A volume watched since its first unsettled eviction left the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watch {
    /// *Other* just before that eviction left the library.
    pub baseline: i64,
    /// The lowest *other* seen since, raised by each eviction settled as freed.
    pub low: i64,
    /// When the watch began.
    pub since: u64,
    /// Whether `baseline` is a reading from before the watch began. Without
    /// one, an eviction already gone may have freed its bytes unseen, so it is
    /// never judged held.
    pub anchored: bool,
}

/// Every eviction whose space may still be occupied, persisted across runs
/// (`state/evictions.json`). A missing or corrupt file reads as empty — the
/// failure mode is one run without credit, never a phantom credit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvictionLedger {
    #[serde(default)]
    pub entries: BTreeMap<String, Eviction>,
    /// Card id → hand-over time, remembered while its eviction is tracked and
    /// for [`HANDOFF_MEMORY_SECS`] after hand-over, unless taken back: which
    /// deletions were FLINCH's.
    #[serde(default)]
    pub handoffs: BTreeMap<String, u64>,
    #[serde(default)]
    pub watches: BTreeMap<String, Watch>,
    /// Per volume, *other* as the next cycle will read it: where a watch starts.
    #[serde(default)]
    pub last_other: BTreeMap<String, i64>,
}

/// One volume's bytes this cycle, as far as FLINCH can name them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Occupancy {
    pub used: u64,
    /// Bytes of the library items on the volume.
    pub library: u64,
}

/// Evicted bytes one volume still credits against its goal, by why.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Credit {
    /// Gone from the library, inside the recycle window or the grace after it.
    pub pending: u64,
    /// Past the grace with no drop on disk: something else holds them.
    pub held: u64,
}

impl Credit {
    pub fn total(&self) -> u64 {
        self.pending.saturating_add(self.held)
    }
}

/// A held eviction, as status.json reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldEviction {
    pub id: String,
    pub title: String,
    pub bytes: u64,
    pub held_since: u64,
    /// When FLINCH stops crediting it and evicts by the measurement again.
    pub until: u64,
}

fn signed(bytes: u64) -> i64 {
    i64::try_from(bytes).unwrap_or(i64::MAX)
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
    pub fn record(&mut self, handed: HandedOver<'_>, now: u64) {
        self.entries.entry(handed.id.to_string()).or_insert_with(|| Eviction {
            app: handed.app,
            volume: handed.volume.to_string(),
            bytes: handed.bytes,
            handed_at: now,
            gone_at: None,
            held_since: None,
            title: handed.title.to_string(),
        });
        self.handoffs.entry(handed.id.to_string()).or_insert(now);
    }

    /// Forget a hand-over that was taken back: if the item later leaves disk
    /// for any other reason, neither its bytes nor its deletion are FLINCH's.
    pub fn forget(&mut self, id: &str) {
        self.entries.remove(id);
        self.handoffs.remove(id);
    }

    /// Advance to `now`: stamp items that left the library, settle the ones
    /// past their recycle window against every `measured` volume, and drop
    /// what has been tracked too long. An unmeasured volume settles nothing.
    pub fn observe(
        &mut self,
        on_disk: impl Fn(&str) -> bool,
        recycle_secs: impl Fn(App) -> u64,
        measured: &BTreeMap<String, Occupancy>,
        now: u64,
    ) {
        self.entries.retain(|id, eviction| {
            if on_disk(id) {
                // Back on disk (re-downloaded) or never deleted yet: no credit.
                eviction.gone_at = None;
                eviction.held_since = None;
                return now.saturating_sub(eviction.handed_at) < STALE_ON_DISK_SECS;
            }
            let gone_at = *eviction.gone_at.get_or_insert(now);
            let waited_out = eviction.held_since.is_some_and(|since| now.saturating_sub(since) >= HELD_CREDIT_SECS);
            // Never before its recycle window ends; on a disk never measured
            // again, not past the last moment it could still have been held.
            let longest = recycle_secs(eviction.app).saturating_add(SETTLE_GRACE_SECS).saturating_add(HELD_CREDIT_SECS);
            !waited_out && now.saturating_sub(gone_at) < longest
        });
        for (volume, occupancy) in measured {
            self.settle(volume, *occupancy, &recycle_secs, now);
        }
        for (id, eviction) in &self.entries {
            self.handoffs.entry(id.clone()).or_insert(eviction.handed_at);
        }
        let tracked = &self.entries;
        self.handoffs
            .retain(|id, handed_at| tracked.contains_key(id) || now.saturating_sub(*handed_at) < HANDOFF_MEMORY_SECS);
    }

    /// Settle `volume`'s evictions whose recycle window has passed, in the
    /// order the windows ended: freed once *other* has dropped by at least half
    /// an eviction's size since it left the library — each drop pays for one
    /// eviction only — and held when the grace runs out first.
    fn settle(&mut self, volume: &str, occupancy: Occupancy, recycle_secs: &impl Fn(App) -> u64, now: u64) {
        let mut gone: Vec<(u64, u64, String)> = Vec::new();
        let mut credited = 0u64;
        for (id, eviction) in self.entries.iter().filter(|(_, eviction)| eviction.volume == volume) {
            if let Some(gone_at) = eviction.gone_at {
                gone.push((gone_at.saturating_add(recycle_secs(eviction.app)), gone_at, id.clone()));
                credited = credited.saturating_add(eviction.bytes);
            }
        }
        let other = signed(occupancy.used) - signed(occupancy.library) - signed(credited);
        if gone.is_empty() {
            self.watches.remove(volume);
            self.last_other.insert(volume.to_string(), other);
            return;
        }
        let previous = self.last_other.get(volume).copied();
        let watch = self.watches.entry(volume.to_string()).or_insert_with(|| {
            let baseline = previous.unwrap_or(other);
            Watch { baseline, low: baseline, since: now, anchored: previous.is_some() }
        });
        watch.low = watch.low.min(other);
        gone.sort();
        let mut released = 0i64;
        for (due_at, gone_at, id) in gone.into_iter().filter(|(due_at, _, _)| now >= *due_at) {
            let Some(eviction) = self.entries.get_mut(&id) else { continue };
            let bytes = signed(eviction.bytes);
            let unjudgeable = !watch.anchored && gone_at <= watch.since;
            let dropped = 2 * (watch.baseline - watch.low) >= bytes;
            if unjudgeable || dropped {
                if !unjudgeable {
                    watch.low += bytes;
                }
                released += bytes;
                self.entries.remove(&id);
            } else if eviction.held_since.is_none() && now.saturating_sub(due_at) >= SETTLE_GRACE_SECS {
                eviction.held_since = Some(now);
            }
        }
        if !self.entries.values().any(|eviction| eviction.volume == volume && eviction.gone_at.is_some()) {
            self.watches.remove(volume);
        }
        // Their credit is gone next cycle, so the next reading sits that much higher.
        self.last_other.insert(volume.to_string(), other + released);
    }

    /// Evicted bytes each volume still credits against its goal.
    pub fn credits(&self) -> BTreeMap<String, Credit> {
        let mut credits: BTreeMap<String, Credit> = BTreeMap::new();
        for eviction in self.entries.values().filter(|eviction| eviction.gone_at.is_some()) {
            let credit = credits.entry(eviction.volume.clone()).or_default();
            let slot = if eviction.held_since.is_some() { &mut credit.held } else { &mut credit.pending };
            *slot = slot.saturating_add(eviction.bytes);
        }
        credits
    }

    /// Held evictions per volume, oldest first.
    pub fn held(&self) -> BTreeMap<String, Vec<HeldEviction>> {
        let mut held: BTreeMap<String, Vec<HeldEviction>> = BTreeMap::new();
        for (id, eviction) in &self.entries {
            let Some(since) = eviction.held_since else { continue };
            held.entry(eviction.volume.clone()).or_default().push(HeldEviction {
                id: id.clone(),
                title: eviction.title.clone(),
                bytes: eviction.bytes,
                held_since: since,
                until: since.saturating_add(HELD_CREDIT_SECS),
            });
        }
        for list in held.values_mut() {
            list.sort_by(|a, b| (a.held_since, &a.id).cmp(&(b.held_since, &b.id)));
        }
        held
    }
}

#[cfg(test)]
mod tests;
