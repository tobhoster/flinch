//! The archive tier in the plan ([`crate::archive`]): which candidates may
//! move to an archive root instead of leaving, grouped as the *arrs move
//! them, and the moves the solver picked.
//!
//! A group is a movie (Radarr) or a whole series (Sonarr's editor moves
//! series, never seasons), so a series moves only as one: every season of it
//! on one source disk. It may move only when every member could be selected at
//! all and none is pinned, partway, already handed over or ruled — a move is
//! the operator's archive, never a way round a keep or a Leaving Soon window
//! already running. Seasons still evict one by one.

use super::knapsack::{MoveGroup, Moves};
use super::{EvictionPlan, MediaCandidate};
use crate::capacity::App;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Where one instance's items may be archived this run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveDestination {
    pub app: App,
    /// The instance ([`crate::ids`]); empty for the default.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instance: String,
    /// The archive root folder, as the *arr names it.
    pub root: String,
    /// The governed volume holding it.
    pub volume: String,
    /// Bytes that volume can take while its own forecast stays under target
    /// ([`crate::archive::headroom`]).
    pub headroom_bytes: u64,
}

/// One planned move: a whole movie or a whole series.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanMove {
    /// The movie's card id or the series' subject: `radarr-<movie id>`,
    /// `sonarr@anime-<series id>`.
    pub id: String,
    pub app: App,
    /// The instance that moves it; empty for the default.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instance: String,
    /// The *arr's movie or series id.
    pub arr_id: u64,
    pub title: String,
    /// The cards that move: the movie, or every season of the series.
    pub cards: Vec<String>,
    pub size_bytes: u64,
    /// The volume it frees.
    pub volume: String,
    /// The volume it lands on, and the root folder there.
    pub archive_volume: String,
    pub root: String,
    /// What evicting it instead would have cost.
    pub regret_avoided: f64,
}

/// The *arr item a card belongs to: its app, instance and movie or series id
/// (a season's series; see [`crate::ids::ArrRef`]).
pub fn arr_item(card_id: &str) -> Option<(App, &str, u64)> {
    crate::ids::ArrRef::card(card_id).map(|item| (item.app, item.instance, u64::from(item.id)))
}

/// The solver's view of the tier, with what each group is in the *arrs.
pub(super) struct Tier<'a> {
    pub moves: Moves,
    items: Vec<((App, &'a str, u64), &'a ArchiveDestination)>,
}

/// Group `candidates` by *arr item; keep the groups whose instance has a
/// destination and whose every member is `movable`.
pub(super) fn tier<'a>(
    candidates: &'a [MediaCandidate],
    destinations: &'a [ArchiveDestination],
    movable: impl Fn(&MediaCandidate) -> bool,
) -> Tier<'a> {
    let mut tier = Tier { moves: Moves::default(), items: Vec::new() };
    if destinations.is_empty() {
        return tier;
    }
    for destination in destinations {
        // Several instances may archive to one disk: its room is shared, never doubled.
        let room = tier.moves.headroom.entry(destination.volume.clone()).or_insert(destination.headroom_bytes);
        *room = (*room).min(destination.headroom_bytes);
    }
    let mut grouped: BTreeMap<(App, &str, u64), Vec<usize>> = BTreeMap::new();
    for (index, candidate) in candidates.iter().enumerate() {
        if let Some(item) = arr_item(&candidate.id) {
            grouped.entry(item).or_default().push(index);
        }
    }
    for (item, members) in grouped {
        let Some(destination) = destinations.iter().find(|destination| destination.app == item.0 && destination.instance == item.1) else {
            continue;
        };
        if members.iter().all(|&index| movable(&candidates[index])) {
            tier.moves.groups.push(MoveGroup { members, archive: destination.volume.clone() });
            tier.items.push((item, destination));
        }
    }
    tier
}

impl Tier<'_> {
    /// The groups the solver moved, as plan entries.
    pub(super) fn plan_moves(&self, moved: &[usize], candidates: &[MediaCandidate]) -> Vec<PlanMove> {
        moved
            .iter()
            .filter_map(|&index| {
                let (group, &((app, instance, arr_id), destination)) = (self.moves.groups.get(index)?, self.items.get(index)?);
                let first = candidates.get(*group.members.first()?)?;
                let title = match app {
                    // A season card reads "Show S3".
                    App::Sonarr => first.title.rsplit_once(" S").map_or(first.title.as_str(), |(show, _)| show),
                    App::Radarr => first.title.as_str(),
                };
                let members = || group.members.iter().map(|&member| &candidates[member]);
                Some(PlanMove {
                    id: match app {
                        App::Radarr => crate::ids::movie_card_id(instance, u32::try_from(arr_id).ok()?),
                        App::Sonarr => crate::ids::show_subject(instance, u32::try_from(arr_id).ok()?),
                    },
                    app,
                    instance: instance.to_string(),
                    arr_id,
                    title: title.to_string(),
                    cards: members().map(|candidate| candidate.id.clone()).collect(),
                    size_bytes: members().map(|candidate| candidate.size_bytes).fold(0, u64::saturating_add),
                    volume: first.volume.clone().unwrap_or_default(),
                    archive_volume: destination.volume.clone(),
                    root: destination.root.clone(),
                    regret_avoided: members().map(|candidate| candidate.regret.value).sum(),
                })
            })
            .collect()
    }
}

impl EvictionPlan {
    /// Bytes the planned moves free on their source volumes.
    pub fn moved_bytes(&self) -> u64 {
        self.moves.iter().map(|planned| planned.size_bytes).fold(0, u64::saturating_add)
    }
}

#[cfg(test)]
mod tests;
