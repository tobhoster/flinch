//! One cycle of capacity governance, from the *arrs' disks to the plan's goal.
//!
//! The daemon fetches; this module decides. It lives in the library so every
//! branch — unmeasured, malformed watermarks, idle, evicting, an item on no
//! governed disk — is tested rather than trusted.

use crate::arr::{ArrMovie, ArrSeries};
use crate::capacity::{
    decide_capacity, App, CapacityDecision, CapacitySnapshot, CapacityStatus, Latch, LibraryVolumes, OnDisk, Watermarks,
};
use crate::daemon::RuntimeSettings;
use crate::plan::{ReclaimGoal, VolumeGoals, VolumeOutcome};
use crate::policy::ArchivePolicy;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Card id → the library volume its files live on, attributed through the app
/// that owns the item (mount paths are container-local). Items whose path no
/// governed mount holds are absent: they can never be evicted.
pub fn volume_map(library: &LibraryVolumes, movies: &[ArrMovie], series: &[ArrSeries]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for movie in movies {
        if let Some(volume) = movie.path.as_deref().and_then(|path| library.volume_of(App::Radarr, path)) {
            map.insert(format!("radarr-{}", movie.id), volume.to_string());
        }
    }
    for show in series {
        if let Some(volume) = show.path.as_deref().and_then(|path| library.volume_of(App::Sonarr, path)) {
            for season in &show.seasons {
                map.insert(format!("sonarr-{}-s{}", show.id, season.season_number), volume.to_string());
            }
        }
    }
    map
}

/// Everything one cycle decided about capacity.
#[derive(Debug, Clone)]
pub struct Governance {
    pub library: LibraryVolumes,
    /// `None` when unmeasured: no library volume, or malformed watermarks.
    pub snapshot: Option<CapacitySnapshot>,
    pub decision: CapacityDecision,
    /// Always per volume: an idle or unmeasured run carries no goals, so it
    /// takes nothing — there is no path from "no measurement" to "everything".
    /// Only *evictable* items are attributed here, so an item FLINCH cannot
    /// hand over can never make a goal look met.
    pub goal: ReclaimGoal,
    /// Every item on a governed disk, evictable or not: for display and the
    /// eviction ledger.
    pub located: HashMap<String, String>,
    /// The operator's watermarks were outside 0 < release ≤ ceiling ≤ 1.
    pub invalid_watermarks: bool,
    /// Library bytes and still-credited evictions per volume (see
    /// [`crate::capacity::EvictionLedger`]): credits come off each goal.
    pub on_disk: OnDisk,
}

/// Measure, decide, and set the plan's goal. May arm the never-played rule on
/// `policy` while a volume is evicting (see [`decide_capacity`]).
///
/// `evictable` says whether FLINCH could actually hand an item over (it has a
/// Plex identity Maintainerr can act on). Anything else stays on its disk for
/// display but never counts toward a goal: counting it made a disk stuck at 85%
/// report its goal as met, cycle after cycle.
pub fn govern(
    library: LibraryVolumes,
    located: HashMap<String, String>,
    evictable: impl Fn(&str) -> bool,
    settings: &RuntimeSettings,
    latch: &Latch,
    on_disk: OnDisk,
    policy: &mut ArchivePolicy,
) -> Governance {
    let marks = Watermarks::new(settings.capacity_ceiling, settings.capacity_release);
    let snapshot = marks.and_then(|marks| CapacitySnapshot::of(&library.volumes, marks));
    let decision = decide_capacity(policy, snapshot.as_ref(), latch, settings.capacity_arm_never_played, &on_disk.credit_totals());
    let volume_of: HashMap<String, String> =
        located.iter().filter(|(id, _)| evictable(id)).map(|(id, volume)| (id.clone(), volume.clone())).collect();
    let goal = ReclaimGoal::PerVolume(VolumeGoals { goals: decision.goals.clone(), volume_of, handed: HashSet::new() });
    Governance { invalid_watermarks: marks.is_none(), library, snapshot, decision, goal, located, on_disk }
}

impl Governance {
    /// Cards already handed to Maintainerr: the plan takes them first while
    /// the policy still permits them (see [`VolumeGoals::handed`]).
    pub fn take_handed_first(&mut self, handed: HashSet<String>) {
        if let ReclaimGoal::PerVolume(goals) = &mut self.goal {
            goals.handed = handed;
        }
    }

    fn volume_of(&self, card_id: &str) -> Option<&str> {
        match &self.goal {
            ReclaimGoal::PerVolume(goals) => goals.volume_of.get(card_id).map(String::as_str),
            ReclaimGoal::AllSafe | ReclaimGoal::Bytes(_) => None,
        }
    }

    /// The volume key an item's files live on, if a governed mount holds it.
    pub fn volume_for(&self, card_id: &str) -> Option<String> {
        self.located.get(card_id).cloned()
    }

    /// Why an item the policy and both floors allow is still on disk. A
    /// candidate in "Keep" with no explanation reads like a bug.
    pub fn held_reason(&self, card_id: &str) -> String {
        let Some(volume) = self.located.get(card_id).map(String::as_str) else {
            return "Eligible, but no governed disk holds it — never evicted".to_string();
        };
        if self.volume_of(card_id).is_none() {
            return "Eligible, but not matched in Plex by id — FLINCH cannot hand it to Maintainerr, so it is never evicted".to_string();
        }
        let Some(snapshot) = &self.snapshot else {
            return "Eligible — held while disk usage is unmeasured".to_string();
        };
        let percent = |fraction: f64| (fraction * 100.0).round();
        match self.decision.goals.get(volume) {
            Some(0) if self.on_disk.credit.get(volume).is_some_and(|credit| credit.held > 0) => {
                format!("Eligible — held while {volume} waits for space handed over earlier that the disk has not released")
            }
            Some(0) => format!("Eligible — held while {volume}'s recycle bin releases space already evicted"),
            Some(_) => format!("Eligible — not needed yet to bring {volume} back to {}%", percent(snapshot.watermarks.release())),
            None => format!("Eligible — held while {volume} is under the {}% ceiling", percent(snapshot.watermarks.ceiling())),
        }
    }

    /// status.json's capacity block; `None` when unmeasured. `handed` lists
    /// every verified FLINCH collection member still on disk, with its bytes.
    pub fn status<'a>(&self, outcomes: &[VolumeOutcome], handed: impl IntoIterator<Item = (&'a str, u64)>) -> Option<CapacityStatus> {
        let snapshot = self.snapshot.as_ref()?;
        let mut per_volume: BTreeMap<String, u64> = BTreeMap::new();
        for (card_id, bytes) in handed {
            if let Some(volume) = self.located.get(card_id) {
                let total = per_volume.entry(volume.clone()).or_insert(0);
                *total = total.saturating_add(bytes);
            }
        }
        Some(CapacityStatus::new(snapshot, &self.decision, outcomes, &self.library.unmatched_roots, &self.on_disk, &per_volume))
    }
}

#[cfg(test)]
mod tests;
