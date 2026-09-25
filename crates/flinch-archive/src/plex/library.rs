//! The Plex library, indexed by external catalogue id.
//!
//! One item per catalogue identity, however many libraries hold it: a film in
//! "Movies" and "4K Movies" is one film. The old title index dropped such
//! duplicates as ambiguous, so every title held twice had no evidence at all.
//! Here copies that share a GUID merge — the most recent play and the furthest
//! progress either copy saw — and every copy's ratingKey and section is kept,
//! because Maintainerr acts per section. Each item also keeps Plex's own
//! `plex://` GUID, which survives a re-add, where exactly one item carries it.

use super::guid::{catalogue_ids, plex_guid, relate, CatalogueId, Relation};
use super::{PlexMetadata, WatchInfo};
use crate::ids::ExternalIds;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Where one copy of an item lives.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Placement {
    /// Library section first, so sorting puts the lowest section first.
    pub section_id: Option<u32>,
    pub rating_key: String,
}

/// A movie or show, merged across every library that holds it.
#[derive(Debug, Clone)]
pub struct PlexItem {
    pub ids: ExternalIds,
    pub title: String,
    pub year: Option<u32>,
    /// Every copy, sorted by (section, ratingKey).
    pub placements: Vec<Placement>,
    /// Whatever any copy saw.
    pub watch: WatchInfo,
    /// Its copies' `plex://movie/…` (or `plex://show/…`) GUIDs that no other
    /// item carries: the join for plays recorded under a ratingKey Plex has
    /// since replaced.
    pub plex_guids: Vec<String>,
}

impl PlexItem {
    pub fn holds(&self, rating_key: &str) -> bool {
        self.placements.iter().any(|placement| placement.rating_key == rating_key)
    }
}

/// One season container, as `/library/sections/{key}/all?type=3` lists it.
#[derive(Debug, Clone)]
pub struct PlexSeason {
    pub placement: Placement,
    pub show_rating_key: String,
    /// The season number as Plex orders it.
    pub index: u32,
    /// Episodes Plex holds for it.
    pub leaf_count: u32,
    pub watch: WatchInfo,
}

#[derive(Debug, Clone, Default)]
pub struct PlexLibrary {
    pub movies: Vec<PlexItem>,
    pub shows: Vec<PlexItem>,
    pub seasons: Vec<PlexSeason>,
}

impl PlexLibrary {
    /// Index movie, show and season rows. Rows without a ratingKey cannot be
    /// acted on or joined and are left out; season rows also need their show's
    /// key, their number and their episode count — a count Plex did not send
    /// is unknown, and reading it as zero made watched seasons "never played".
    pub fn new(movies: &[PlexMetadata], shows: &[PlexMetadata], seasons: &[PlexMetadata]) -> Self {
        Self {
            movies: merge_by_id(movies, PlexMetadata::movie_watch, "movie"),
            shows: merge_by_id(shows, PlexMetadata::leaf_watch, "show"),
            seasons: seasons
                .iter()
                .filter(|row| !row.rating_key.is_empty())
                .filter_map(|row| {
                    Some(PlexSeason {
                        placement: placement(row),
                        show_rating_key: row.parent_rating_key.clone().filter(|key| !key.is_empty())?,
                        index: row.index?,
                        leaf_count: row.leaf_count?,
                        watch: row.leaf_watch(),
                    })
                })
                .collect(),
        }
    }

    /// Season containers numbered `index` under any copy of `show`.
    pub fn seasons_of<'a>(&'a self, show: &'a PlexItem, index: u32) -> impl Iterator<Item = &'a PlexSeason> + 'a {
        self.seasons.iter().filter(move |season| season.index == index && show.holds(&season.show_rating_key))
    }
}

fn placement(row: &PlexMetadata) -> Placement {
    Placement { section_id: row.library_section_id, rating_key: row.rating_key.clone() }
}

/// Group rows that share a catalogue id and disagree on none. `kind` is the
/// `plex://{kind}/…` GUID the rows carry.
fn merge_by_id(rows: &[PlexMetadata], watch: fn(&PlexMetadata) -> WatchInfo, kind: &str) -> Vec<PlexItem> {
    let mut items: Vec<PlexItem> = Vec::new();
    let mut by_id: HashMap<CatalogueId, usize> = HashMap::new();
    for row in rows.iter().filter(|row| !row.rating_key.is_empty()) {
        let ids = row.external_ids();
        let plex = row.guid.as_deref().and_then(|guid| plex_guid(guid, kind));
        let existing = catalogue_ids(&ids)
            .iter()
            .find_map(|id| by_id.get(id).copied())
            .filter(|index| relate(&items[*index].ids, &ids) == Relation::Same);
        let index = match existing {
            Some(index) => {
                let item = &mut items[index];
                item.ids.tmdb = item.ids.tmdb.or(ids.tmdb);
                item.ids.tvdb = item.ids.tvdb.or(ids.tvdb);
                item.ids.imdb = item.ids.imdb.take().or(ids.imdb);
                item.watch = item.watch.merge(watch(row));
                let copy = placement(row);
                if let Err(at) = item.placements.binary_search(&copy) {
                    item.placements.insert(at, copy);
                }
                if let Some(guid) = plex.filter(|guid| !item.plex_guids.iter().any(|held| held == guid)) {
                    item.plex_guids.push(guid.to_string());
                }
                index
            }
            None => {
                items.push(PlexItem {
                    ids,
                    title: row.title.clone(),
                    year: row.year,
                    placements: vec![placement(row)],
                    watch: watch(row),
                    plex_guids: plex.map(str::to_string).into_iter().collect(),
                });
                items.len() - 1
            }
        };
        for id in catalogue_ids(&items[index].ids) {
            by_id.entry(id).or_insert(index);
        }
    }
    drop_shared_plex_guids(&mut items);
    items
}

/// A `plex://` GUID two items carry names neither of them for certain: it is
/// dropped from both, so a play joins by it only where it is unambiguous.
fn drop_shared_plex_guids(items: &mut [PlexItem]) {
    let mut carriers: HashMap<String, usize> = HashMap::new();
    for guid in items.iter().flat_map(|item| &item.plex_guids) {
        *carriers.entry(guid.clone()).or_default() += 1;
    }
    for item in items {
        item.plex_guids.retain(|guid| carriers.get(guid) == Some(&1));
    }
}
