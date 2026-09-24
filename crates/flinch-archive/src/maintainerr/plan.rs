//! The pure planner: desired state (what FLINCH decided), observed state
//! (what Maintainerr holds) and owned state (what FLINCH made) become an
//! ordered list of [`SyncAction`]s. No I/O, so every rule is tested directly.
//!
//! The rules:
//! - Evict: first remove FLINCH's own exclusions for the item, then add it to
//!   the collection for its kind and route — Leaving Soon for an announced
//!   item, when the operator named one; the delete collection otherwise. An
//!   item that is already a member costs nothing against the caps. The caps
//!   count only new adds, and each is checked as `handed + size > cap`.
//! - Un-schedule: a membership FLINCH added whose card is no longer evicted is
//!   removed, and so is one in the wrong route's collection once the right one
//!   takes it. Un-schedules come first; the executor verifies each.
//! - Protect: an exclusion is added only when no row covers the item. A row
//!   FLINCH does not own is the operator's: it is never touched, and the card
//!   is reported as an operator keep (a hard keep guard for the policy).
//! - A card without PlexIds is unresolved: never protected, never scheduled.
//! - Wrong copy: an eviction whose ratingKey a kept card also resolves to, or
//!   whose Plex item has several copies (FLINCH cannot tell which one its
//!   evidence judged), is held; a membership FLINCH made for it is taken back.
//! - Keep wins: a card in both lists is only protected.
//! - A broken Leaving Soon collection blocks announced items; it never sends
//!   them to the delete collection instead.
//! - Gone: an exclusion FLINCH made for an item Plex no longer holds (see
//!   [`OwnedState::vanished`]) protects nothing and is released. Only
//!   FLINCH's own rows go; the operator's stay.

use super::validate::{self, CollectionTitles, Handover, Misconfigured, Route};
use super::{CollectionInfo, ExclusionRow, MaintainerrTarget, MaintainerrVersion, OwnedState};
use crate::card::LibraryKind;
use crate::ids::PlexIds;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// One card as the sync sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncItem {
    pub card_id: String,
    pub kind: LibraryKind,
    /// `None` when the GUID join found no Plex item.
    pub plex: Option<PlexIds>,
    /// The item ratingKey of every Plex copy the GUID join merged (the
    /// season's own for a season). Empty when unresolved, or when only the
    /// copy in `plex` is known.
    pub copies: Vec<String>,
    pub bytes: u64,
}

impl SyncItem {
    pub fn target(&self) -> Option<MaintainerrTarget> {
        self.plex.as_ref().and_then(|ids| MaintainerrTarget::from_plex(ids, self.kind))
    }

    fn section(&self) -> Option<u32> {
        self.plex.as_ref().and_then(|ids| ids.section_id)
    }

    /// Every ratingKey this card's item could be handed over or protected as.
    fn item_keys(&self) -> impl Iterator<Item = &str> {
        let named = self.plex.as_ref().and_then(|ids| match self.kind {
            LibraryKind::Movie => Some(ids.rating_key.as_str()),
            LibraryKind::Season => ids.season_rating_key.as_deref(),
        });
        named.into_iter().chain(self.copies.iter().map(String::as_str)).filter(|key| !key.trim().is_empty())
    }
}

/// What FLINCH decided this cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Desired {
    /// Cards to keep, protected by an exclusion.
    pub protect: Vec<SyncItem>,
    /// Cards to evict, in eviction order (least expected regret first).
    pub evict: Vec<SyncItem>,
    /// Evictions nobody has watched: announced in Leaving Soon before they go.
    pub announced: BTreeSet<String>,
    pub collections: CollectionTitles,
    /// Cards whose FLINCH exclusion protects nothing any more: the item left
    /// the library and Plex (see [`OwnedState::vanished`]).
    pub gone: BTreeSet<String>,
    /// Maintainerr has Seerr configured, so a collection that leaves Seerr
    /// requests behind is worth a warning.
    pub seerr_configured: bool,
}

/// What Maintainerr holds, as read this cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    pub version: MaintainerrVersion,
    pub collections: Vec<CollectionInfo>,
    /// Collection id → the item keys of its members.
    pub members: BTreeMap<i64, BTreeSet<String>>,
    /// Target `media_id` → the rows `?mediaServerId=` returned for it.
    pub exclusions: BTreeMap<String, Vec<ExclusionRow>>,
}

/// Per-cycle limits on NEW collection adds.
///
/// The first add of a run is never refused for size alone: an item larger than
/// the byte cap would otherwise be deferred forever, and a volume whose
/// eligible items are all that large would never get back under its ceiling.
/// A cap of 0 (items or bytes) still hands nothing over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    pub max_items: usize,
    pub max_bytes: u64,
}

impl Caps {
    pub fn new(max_items: usize, max_gib: u64) -> Self {
        Self { max_items, max_bytes: max_gib.saturating_mul(1 << 30) }
    }

    /// Whether one more add of `bytes` fits after `handed` adds of `handed_bytes`.
    fn admits(&self, handed: usize, handed_bytes: u64, bytes: u64) -> bool {
        handed < self.max_items
            && self.max_bytes > 0
            && (handed == 0 || handed_bytes.saturating_add(bytes) <= self.max_bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncAction {
    /// Drop one FLINCH-owned exclusion row so the card can be deleted.
    RemoveExclusion { card_id: String, target: MaintainerrTarget, exclusion_id: i64 },
    /// Add the card to its kind's deletion collection.
    Schedule { card_id: String, target: MaintainerrTarget, collection_id: i64, bytes: u64 },
    /// Take a FLINCH-added member back out of its collection.
    Unschedule { card_id: String, target: MaintainerrTarget, collection_id: i64 },
    /// Add a global exclusion for a card FLINCH keeps.
    Protect { card_id: String, target: MaintainerrTarget },
}

impl SyncAction {
    pub fn card_id(&self) -> &str {
        match self {
            Self::RemoveExclusion { card_id, .. }
            | Self::Schedule { card_id, .. }
            | Self::Unschedule { card_id, .. }
            | Self::Protect { card_id, .. } => card_id,
        }
    }
}

impl fmt::Display for SyncAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RemoveExclusion { card_id, exclusion_id, .. } => write!(f, "release exclusion {exclusion_id} of {card_id}"),
            Self::Schedule { card_id, target, collection_id, bytes } => write!(
                f,
                "schedule {card_id} (ratingKey {}, {:.1} GiB) into collection {collection_id}",
                target.item_key(),
                *bytes as f64 / f64::from(1u32 << 30)
            ),
            Self::Unschedule { card_id, target, collection_id } => {
                write!(f, "unschedule {card_id} (ratingKey {}) from collection {collection_id}", target.item_key())
            }
            Self::Protect { card_id, target } => write!(f, "protect {card_id} (ratingKey {})", target.item_key()),
        }
    }
}

/// Why an eviction was not handed over this cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocked {
    /// Maintainerr is older than v3.10 (see [`SyncPlan::handover`]).
    HandoverRefused,
    /// The kind's collection is misconfigured (see [`SyncPlan::misconfigured`]).
    CollectionMisconfigured,
    /// No single valid collection of the kind is bound to the item's section.
    NoCollectionForSection(Option<u32>),
    /// The item's rows or its collection's members were not read this cycle.
    Unobserved,
    /// A kept card resolves to the same Plex ratingKey: handing it over would
    /// delete what FLINCH keeps.
    SharesKeptItem { rating_key: String, kept: String },
    /// Plex merged several copies of the item by GUID (e.g. one per library):
    /// Maintainerr acts on one, and FLINCH cannot tell which copy it judged.
    SeveralPlexCopies(Vec<String>),
}

impl fmt::Display for Blocked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HandoverRefused => f.write_str("Maintainerr too old for handover"),
            Self::CollectionMisconfigured => f.write_str("collection misconfigured"),
            Self::NoCollectionForSection(Some(section)) => write!(f, "no single valid collection bound to Plex section {section}"),
            Self::NoCollectionForSection(None) => f.write_str("Plex section unknown and the kind has several collections"),
            Self::Unobserved => f.write_str("Maintainerr state not read this cycle"),
            Self::SharesKeptItem { rating_key, kept } => {
                write!(f, "held: Plex ratingKey {rating_key} is also the kept item {kept}, so handing it over would delete a keeper")
            }
            Self::SeveralPlexCopies(keys) => write!(
                f,
                "held: Plex has {} copies of this item (ratingKeys {}) and FLINCH cannot tell which one it judged; keep one copy in Plex, or keep the item",
                keys.len(),
                keys.join(", ")
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncPlan {
    /// Un-schedules first, then protections, then evictions in eviction order.
    pub actions: Vec<SyncAction>,
    /// Cards covered by an exclusion FLINCH does not own.
    pub operator_keeps: Vec<String>,
    /// Cards without PlexIds; nothing is done for them.
    pub unresolved: Vec<String>,
    /// Evictions over this cycle's caps; a later cycle hands them over.
    pub deferred: Vec<String>,
    pub blocked: Vec<(String, Blocked)>,
    pub misconfigured: Vec<Misconfigured>,
    pub handover: Handover,
    /// Evictions already in their collection: no action, and not counted
    /// against the caps.
    pub already_scheduled: usize,
    /// Keeps whose FLINCH exclusion is already in place.
    pub already_protected: usize,
    /// Leaving Soon collections that took items this cycle, by id: a
    /// schedule into one is an announcement, not a deletion.
    pub leaving: BTreeSet<i64>,
    /// Cards whose exclusions are released because their item is gone.
    pub gone: BTreeSet<String>,
    /// Collection settings that leave cleanup undone; nothing is blocked.
    pub warnings: Vec<String>,
}

/// A covering row FLINCH does not own means the operator keeps the item.
fn operator_owned(target: &MaintainerrTarget, rows: &[ExclusionRow], owned_ids: &BTreeSet<i64>) -> bool {
    rows.iter().any(|row| target.is_covered_by(row) && !owned_ids.contains(&row.id))
}

/// Why handing `item` over could delete something other than what FLINCH
/// judged: a copy a kept card also resolves to, or one of several copies.
fn wrong_copy(item: &SyncItem, kept_keys: &BTreeMap<&str, &str>) -> Option<Blocked> {
    if let Some((key, kept)) = item.item_keys().find_map(|key| kept_keys.get(key).map(|kept| (key, *kept))) {
        return Some(Blocked::SharesKeptItem { rating_key: key.to_string(), kept: kept.to_string() });
    }
    let copies: BTreeSet<&str> = item.item_keys().collect();
    (copies.len() > 1).then(|| Blocked::SeveralPlexCopies(copies.into_iter().map(str::to_string).collect()))
}

/// Cards the operator protects in Maintainerr. The daemon sets their keep
/// guard before planning, so the policy never picks them.
pub fn operator_keeps(items: &[SyncItem], observed: &Observed, owned: &OwnedState) -> BTreeSet<String> {
    let owned_ids = owned.exclusion_ids();
    items
        .iter()
        .filter(|item| {
            item.target().is_some_and(|target| {
                observed.exclusions.get(target.media_id()).is_some_and(|rows| operator_owned(&target, rows, &owned_ids))
            })
        })
        .map(|item| item.card_id.clone())
        .collect()
}

pub fn plan_sync(desired: &Desired, observed: &Observed, owned: &OwnedState, caps: &Caps) -> SyncPlan {
    let owned_ids = owned.exclusion_ids();
    let mut plan = SyncPlan {
        actions: Vec::new(),
        operator_keeps: Vec::new(),
        unresolved: Vec::new(),
        deferred: Vec::new(),
        blocked: Vec::new(),
        misconfigured: Vec::new(),
        handover: validate::handover(&observed.version),
        already_scheduled: 0,
        already_protected: 0,
        leaving: BTreeSet::new(),
        gone: BTreeSet::new(),
        warnings: validate::cleanup_warnings(&observed.collections, &desired.collections, desired.seerr_configured),
    };
    let routes: &[Route] = match desired.collections.route(true) {
        Route::LeavingSoon => &[Route::Delete, Route::LeavingSoon],
        Route::Delete => &[Route::Delete],
    };
    let mut resolved: Vec<((LibraryKind, Route), Option<Vec<&CollectionInfo>>)> = Vec::new();
    for kind in [LibraryKind::Movie, LibraryKind::Season] {
        for route in routes {
            let found = validate::resolve(&observed.collections, &desired.collections, kind, *route)
                .map_err(|problems| plan.misconfigured.extend(problems))
                .ok();
            if *route == Route::LeavingSoon {
                plan.leaving.extend(found.iter().flatten().map(|collection| collection.id));
            }
            resolved.push(((kind, *route), found));
        }
    }
    let candidates_for =
        |kind, route| resolved.iter().find(|(key, _)| *key == (kind, route)).and_then(|(_, found)| found.as_deref());

    let keep: BTreeSet<&str> = desired.protect.iter().map(|item| item.card_id.as_str()).collect();
    // Every ratingKey a kept card resolves to, and which card: no eviction
    // may be handed over under one of them.
    let kept_keys: BTreeMap<&str, &str> =
        desired.protect.iter().flat_map(|item| item.item_keys().map(|key| (key, item.card_id.as_str()))).collect();
    // Cards whose membership stays: resolved evictions, and those this cycle
    // cannot judge. Every other FLINCH membership is un-scheduled.
    let mut evicting: BTreeSet<&str> = BTreeSet::new();
    // Where each placed eviction sits this cycle: a FLINCH membership in any
    // other collection is the item's old route and is taken back.
    let mut placed: BTreeMap<&str, i64> = BTreeMap::new();
    let mut evictions = Vec::new();
    let (mut handed, mut handed_bytes) = (0usize, 0u64);
    for item in desired.evict.iter().filter(|item| !keep.contains(item.card_id.as_str())) {
        let id = item.card_id.as_str();
        let Some(target) = item.target() else {
            plan.unresolved.push(id.to_string());
            evicting.insert(id);
            continue;
        };
        if let Some(reason) = wrong_copy(item, &kept_keys) {
            // Judged, not unjudgeable: a membership FLINCH made for it is
            // taken back below, because it is not `evicting`.
            plan.blocked.push((id.to_string(), reason));
            continue;
        }
        let Some(rows) = observed.exclusions.get(target.media_id()) else {
            plan.blocked.push((id.to_string(), Blocked::Unobserved));
            evicting.insert(id);
            continue;
        };
        if operator_owned(&target, rows, &owned_ids) {
            plan.operator_keeps.push(id.to_string());
            continue;
        }
        evicting.insert(id);
        let blocked = |reason| (id.to_string(), reason);
        if matches!(plan.handover, Handover::Refused { .. }) {
            plan.blocked.push(blocked(Blocked::HandoverRefused));
            continue;
        }
        let route = desired.collections.route(desired.announced.contains(id));
        let Some(candidates) = candidates_for(item.kind, route) else {
            plan.blocked.push(blocked(Blocked::CollectionMisconfigured));
            continue;
        };
        let Some(collection) = validate::for_section(candidates, item.section()) else {
            plan.blocked.push(blocked(Blocked::NoCollectionForSection(item.section())));
            continue;
        };
        let Some(members) = observed.members.get(&collection.id) else {
            plan.blocked.push(blocked(Blocked::Unobserved));
            continue;
        };
        let releases = owned.protected.get(id).into_iter().flat_map(|entry| {
            rows.iter().filter(|row| entry.exclusion_ids.contains(&row.id)).map(|row| SyncAction::RemoveExclusion {
                card_id: id.to_string(),
                target: target.clone(),
                exclusion_id: row.id,
            })
        });
        if members.contains(target.item_key()) {
            // A member still carrying FLINCH's exclusion would be skipped at
            // deletion time, so the exclusion goes.
            plan.already_scheduled += 1;
            placed.insert(id, collection.id);
            evictions.extend(releases);
            continue;
        }
        if !caps.admits(handed, handed_bytes, item.bytes) {
            // Deferred: an old-route membership stays until the new add fits.
            plan.deferred.push(id.to_string());
            continue;
        }
        handed += 1;
        handed_bytes = handed_bytes.saturating_add(item.bytes);
        placed.insert(id, collection.id);
        evictions.extend(releases);
        evictions.push(SyncAction::Schedule {
            card_id: id.to_string(),
            target: target.clone(),
            collection_id: collection.id,
            bytes: item.bytes,
        });
    }

    for (card_id, entry) in &owned.scheduled {
        let still_member = observed.members.get(&entry.collection_id).is_none_or(|m| m.contains(entry.target.item_key()));
        let moved = placed.get(card_id.as_str()).is_some_and(|collection_id| *collection_id != entry.collection_id);
        if still_member && (moved || !evicting.contains(card_id.as_str())) {
            plan.actions.push(SyncAction::Unschedule {
                card_id: card_id.clone(),
                target: entry.target.clone(),
                collection_id: entry.collection_id,
            });
        }
    }

    for item in &desired.protect {
        let id = item.card_id.clone();
        let Some(target) = item.target() else {
            plan.unresolved.push(id);
            continue;
        };
        let Some(rows) = observed.exclusions.get(target.media_id()) else {
            plan.blocked.push((id, Blocked::Unobserved));
            continue;
        };
        if operator_owned(&target, rows, &owned_ids) {
            plan.operator_keeps.push(id);
        } else if rows.iter().any(|row| target.is_covered_by(row)) {
            plan.already_protected += 1;
        } else {
            plan.actions.push(SyncAction::Protect { card_id: id, target });
        }
    }

    // An exclusion on an item Plex no longer holds protects nothing: release
    // FLINCH's own rows for it. A card still kept or evicted is never gone.
    for id in desired.gone.iter().filter(|id| !keep.contains(id.as_str()) && !evicting.contains(id.as_str())) {
        let Some(entry) = owned.protected.get(id) else { continue };
        let Some(rows) = observed.exclusions.get(entry.target.media_id()) else { continue };
        let releases: Vec<SyncAction> = rows
            .iter()
            .filter(|row| entry.exclusion_ids.contains(&row.id))
            .map(|row| SyncAction::RemoveExclusion { card_id: id.clone(), target: entry.target.clone(), exclusion_id: row.id })
            .collect();
        if !releases.is_empty() {
            plan.gone.insert(id.clone());
            plan.actions.extend(releases);
        }
    }

    plan.actions.extend(evictions);
    plan
}
