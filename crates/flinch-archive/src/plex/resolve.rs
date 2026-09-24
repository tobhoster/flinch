//! Joining *arr items to Plex, and play rows to *arr items.
//!
//! A target resolves by GUID: the one Plex item sharing a catalogue id with it
//! and contradicting none. Without an id in common, an exact title *and* a year
//! both sides state may stand in — only when exactly one Plex item and exactly
//! one target carry that pair, and no id contradicts it. A season additionally
//! needs Plex to hold as many episodes as Sonarr has files for that season
//! number, because the two can order seasons differently.
//!
//! Play rows (Plex history, Tautulli) then join by ratingKey, never by title,
//! wherever a target resolved — or, where Plex re-added the item since the
//! play, by its `plex://` GUID (see [`super::migration`]). An unresolved movie
//! may fall back to the same exact title+year rule; an unresolved season has no
//! safe fallback and joins nothing.

use super::episodes::EpisodeIds;
use super::guid::{relate, Relation};
use super::join::PlayJoin;
use super::library::{PlexItem, PlexLibrary, PlexSeason};
use super::migration::EpisodeGuids;
use super::{normalise, WatchInfo, WatchTarget};
use crate::card::LibraryKind;
use crate::ids::PlexIds;
use crate::watch::{EvidenceHealth, WatchEntry, WatchSource};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};

/// How a target was resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchedBy {
    /// A shared catalogue id: identity.
    Guid,
    /// A unique exact title and year: correlation, never enough to act on.
    TitleYear,
}

/// The keys a resolved item's plays carry. The GUID lists default to empty,
/// so `items.json` written before they existed still reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlayKeys {
    Movie {
        rating_keys: Vec<String>,
        /// The movie's `plex://movie/…` GUID, for plays recorded under a
        /// ratingKey Plex has since replaced.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        plex_guids: Vec<String>,
    },
    Season {
        /// Shows whose season container was accepted.
        show_rating_keys: Vec<String>,
        season_rating_keys: Vec<String>,
        /// The season number both sides agree on.
        season: u32,
        /// `plex://episode/…` GUIDs of the episodes Plex files under the
        /// season, from the episode GUID index; empty when it was not read.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        episode_guids: Vec<String>,
    },
}

impl PlayKeys {
    /// The ratingKey of every Plex copy a hand-off could act on: each copy of
    /// the movie, or each accepted season container.
    pub fn item_keys(&self) -> &[String] {
        match self {
            Self::Movie { rating_keys, .. } => rating_keys,
            Self::Season { season_rating_keys, .. } => season_rating_keys,
        }
    }
}

/// One resolved target.
#[derive(Debug, Clone, PartialEq)]
pub struct PlexMatch {
    pub by: MatchedBy,
    /// The copy in the lowest library section (ratingKey breaking ties): the
    /// one a hand-off would name. Where `keys` holds more than one copy, the
    /// hand-off refuses the item instead (see [`crate::maintainerr::Blocked`]).
    pub primary: PlexIds,
    pub keys: PlayKeys,
    /// Item state merged over every accepted copy.
    pub watch: WatchInfo,
}

#[derive(Debug, Clone, Default)]
pub struct Resolution {
    matches: HashMap<String, PlexMatch>,
    /// Normalised movie title+year pairs more than one target carries: a title
    /// fallback on them could pick either.
    ambiguous_movies: HashSet<(String, u32)>,
    /// Seasons waiting on episode ids (see [`Unconfirmed`]).
    unconfirmed: Vec<Unconfirmed>,
}

/// A season whose show resolved by GUID and whose Plex season exists, but whose
/// episode count differs from Sonarr's file count: it resolves only once TVDB
/// episode ids confirm it (see [`super::episodes`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unconfirmed {
    pub target_id: String,
    /// Every Plex copy of the show holding a season with that number.
    pub show_rating_keys: Vec<String>,
}

/// Resolve every target against the library. An empty library (Plex disabled
/// or down) still yields a usable [`Resolution`]: nothing resolves, and play
/// joins fall back to the exact title+year rule.
pub fn resolve(targets: &[WatchTarget], library: &PlexLibrary) -> Resolution {
    // (is a movie, normalised title, year) → targets carrying it.
    let mut seen: HashMap<(bool, String, u32), usize> = HashMap::new();
    for target in targets {
        if let Some(year) = target.year {
            *seen.entry((target.kind == LibraryKind::Movie, normalise(&target.title), year)).or_default() += 1;
        }
    }
    let unique =
        |target: &WatchTarget, year: u32| seen.get(&(target.kind == LibraryKind::Movie, normalise(&target.title), year)) == Some(&1);
    let no_episode_ids = EpisodeIds::default();
    let mut resolution = Resolution::default();
    for target in targets {
        let found = match target.kind {
            LibraryKind::Movie => find(&library.movies, target, &unique).and_then(movie_match),
            LibraryKind::Season => find(&library.shows, target, &unique)
                .and_then(|show| season_match(library, target, show, &no_episode_ids, &mut resolution.unconfirmed)),
        };
        if let Some(found) = found {
            resolution.matches.insert(target.id.clone(), found);
        }
    }
    resolution.ambiguous_movies = seen
        .into_iter()
        .filter(|((movie, _, _), count)| *movie && *count > 1)
        .map(|((_, title, year), _)| (title, year))
        .collect();
    resolution
}

fn find<'a>(
    items: &'a [PlexItem],
    target: &WatchTarget,
    unique: &dyn Fn(&WatchTarget, u32) -> bool,
) -> Option<(&'a PlexItem, MatchedBy)> {
    let mut by_id = items.iter().filter(|item| relate(&item.ids, &target.external) == Relation::Same);
    if let Some(first) = by_id.next() {
        // Two items claiming one id is a broken library, not a choice to make.
        return by_id.next().is_none().then_some((first, MatchedBy::Guid));
    }
    let year = target.year?;
    if !unique(target, year) {
        return None;
    }
    let title = normalise(&target.title);
    let mut by_title = items.iter().filter(|item| {
        item.year == Some(year) && normalise(&item.title) == title && relate(&item.ids, &target.external) == Relation::Unknown
    });
    let first = by_title.next()?;
    by_title.next().is_none().then_some((first, MatchedBy::TitleYear))
}

fn movie_match((item, by): (&PlexItem, MatchedBy)) -> Option<PlexMatch> {
    let primary = item.placements.first()?;
    Some(PlexMatch {
        by,
        primary: PlexIds { rating_key: primary.rating_key.clone(), season_rating_key: None, section_id: primary.section_id },
        keys: PlayKeys::Movie {
            rating_keys: item.placements.iter().map(|copy| copy.rating_key.clone()).collect(),
            plex_guids: item.plex_guids.clone(),
        },
        watch: item.watch,
    })
}

/// A season resolves where Plex holds exactly as many episodes as Sonarr has
/// files for it, or — the counts differing — where TVDB episode ids show it is
/// the same season. Otherwise the numbering may differ and the evidence would
/// land on another season, so a GUID-resolved show records the season as
/// unconfirmed for the caller to read episode ids and try again.
fn season_match(
    library: &PlexLibrary,
    target: &WatchTarget,
    (show, by): (&PlexItem, MatchedBy),
    episodes: &EpisodeIds,
    unconfirmed: &mut Vec<Unconfirmed>,
) -> Option<PlexMatch> {
    let season = target.season_index?;
    let files = target.episode_files.filter(|files| *files > 0)?;
    let numbered: Vec<&PlexSeason> = library.seasons_of(show, season).collect();
    let confirmed = |row: &PlexSeason| by == MatchedBy::Guid && episodes.confirm(&target.id, &row.show_rating_key, season);
    let mut accepted: Vec<&PlexSeason> = numbered.iter().copied().filter(|row| row.leaf_count == files || confirmed(row)).collect();
    if accepted.is_empty() {
        if by == MatchedBy::Guid && !numbered.is_empty() {
            let mut show_rating_keys: Vec<String> = numbered.iter().map(|row| row.show_rating_key.clone()).collect();
            show_rating_keys.sort();
            show_rating_keys.dedup();
            unconfirmed.push(Unconfirmed { target_id: target.id.clone(), show_rating_keys });
        }
        return None;
    }
    accepted.sort_by(|a, b| a.placement.cmp(&b.placement));
    let primary = *accepted.first()?;
    let mut show_rating_keys: Vec<String> = accepted.iter().map(|row| row.show_rating_key.clone()).collect();
    show_rating_keys.sort();
    show_rating_keys.dedup();
    Some(PlexMatch {
        by,
        primary: PlexIds {
            rating_key: primary.show_rating_key.clone(),
            season_rating_key: Some(primary.placement.rating_key.clone()),
            section_id: primary.placement.section_id,
        },
        keys: PlayKeys::Season {
            show_rating_keys,
            season_rating_keys: accepted.iter().map(|row| row.placement.rating_key.clone()).collect(),
            season,
            episode_guids: Vec::new(),
        },
        watch: accepted.iter().map(|row| of_files(row.watch, row.leaf_count, files)).reduce(WatchInfo::merge)?,
    })
}

/// Plex's watched share counts the episodes Plex holds. Where Sonarr has more
/// files (Plex has not scanned them yet), those are unwatched: the season is
/// not complete until they are.
fn of_files(watch: WatchInfo, held: u32, files: u32) -> WatchInfo {
    if held >= files {
        return watch;
    }
    WatchInfo { watched_fraction: watch.watched_fraction * held as f32 / files as f32, ..watch }
}

impl Resolution {
    pub fn get(&self, id: &str) -> Option<&PlexMatch> {
        self.matches.get(id)
    }

    /// Resolved by a catalogue id: identity good enough to act on and to claim
    /// absence for.
    pub fn is_guid_resolved(&self, id: &str) -> bool {
        self.matches.get(id).is_some_and(|found| found.by == MatchedBy::Guid)
    }

    pub fn len(&self) -> usize {
        self.matches.len()
    }

    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }

    /// Seasons that resolve once episode ids confirm them.
    pub fn unconfirmed_seasons(&self) -> &[Unconfirmed] {
        &self.unconfirmed
    }

    /// Try the unconfirmed seasons again with episode ids; returns how many
    /// resolved. Only a GUID-resolved show ever had a season to confirm.
    pub fn confirm_seasons(&mut self, targets: &[WatchTarget], library: &PlexLibrary, episodes: &EpisodeIds) -> usize {
        let waiting: HashSet<String> = self.unconfirmed.drain(..).map(|season| season.target_id).collect();
        let mut confirmed = 0;
        for target in targets.iter().filter(|target| waiting.contains(&target.id)) {
            let found = find(&library.shows, target, &|_: &WatchTarget, _: u32| false)
                .and_then(|show| season_match(library, target, show, episodes, &mut self.unconfirmed));
            if let Some(found) = found {
                self.matches.insert(target.id.clone(), found);
                confirmed += 1;
            }
        }
        confirmed
    }

    /// Resolved targets any of whose Plex copies carries one of `keys`: the
    /// movie, the show (so every season of it), or the season itself.
    pub fn marked(&self, keys: &HashSet<String>) -> BTreeSet<String> {
        let any = |list: &[String]| list.iter().any(|key| keys.contains(key));
        self.matches
            .iter()
            .filter(|(_, found)| match &found.keys {
                PlayKeys::Movie { rating_keys, .. } => any(rating_keys),
                PlayKeys::Season { show_rating_keys, season_rating_keys, .. } => any(show_rating_keys) || any(season_rating_keys),
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Plex placement for every GUID-resolved target — what Maintainerr acts on.
    /// Title-resolved and unresolved targets are absent.
    pub fn plex_ids(&self) -> HashMap<String, PlexIds> {
        self.matches
            .iter()
            .filter(|(_, found)| found.by == MatchedBy::Guid)
            .map(|(id, found)| (id.clone(), found.primary.clone()))
            .collect()
    }

    /// Plex shows holding a resolved season: the shows whose episodes the
    /// episode GUID index needs.
    pub fn season_shows(&self) -> BTreeSet<&str> {
        self.matches
            .values()
            .filter_map(|found| match &found.keys {
                PlayKeys::Season { show_rating_keys, .. } => Some(show_rating_keys),
                PlayKeys::Movie { .. } => None,
            })
            .flatten()
            .map(String::as_str)
            .collect()
    }

    /// Give every resolved season the GUIDs of the episodes Plex files under
    /// it now, so plays recorded under an older ratingKey still reach it.
    /// Returns how many seasons have any.
    pub fn attach_episode_guids(&mut self, index: &EpisodeGuids) -> usize {
        let mut attached = 0;
        for found in self.matches.values_mut() {
            if let PlayKeys::Season { show_rating_keys, season, episode_guids, .. } = &mut found.keys {
                let guids: BTreeSet<&String> = show_rating_keys.iter().filter_map(|show| index.season(show, *season)).flatten().collect();
                *episode_guids = guids.into_iter().cloned().collect();
                attached += usize::from(!episode_guids.is_empty());
            }
        }
        attached
    }

    /// Play keys for every resolved target, for persisting beside the item.
    pub fn play_keys(&self) -> HashMap<String, PlayKeys> {
        self.matches.iter().map(|(id, found)| (id.clone(), found.keys.clone())).collect()
    }

    /// How a target's plays are recognised in play logs.
    pub fn join(&self, target: &WatchTarget) -> PlayJoin {
        match self.matches.get(&target.id) {
            Some(found) => PlayJoin::Keys(found.keys.clone()),
            None => match (target.kind, target.year) {
                (LibraryKind::Movie, Some(year)) if !self.ambiguous_movies.contains(&(normalise(&target.title), year)) => {
                    PlayJoin::fallback(target.kind, &target.title, Some(year))
                }
                _ => PlayJoin::Unresolved,
            },
        }
    }

    /// Item-state watch entries for resolved targets.
    ///
    /// Item state belongs to the one account whose token is borrowed. On a
    /// server with several accounts its "nothing played" speaks for that
    /// account only, so it is kept as evidence only when server-wide history and
    /// Tautulli were both complete this cycle; plays it records always count.
    pub fn item_entries(&self, health: &EvidenceHealth) -> HashMap<String, WatchEntry> {
        let zeros_count = health.admin_zero_is_evidence();
        self.matches
            .iter()
            .filter(|(_, found)| zeros_count || found.watch.last_viewed_unix.is_some() || found.watch.watched_fraction > 0.0)
            .map(|(id, found)| {
                let entry = WatchEntry {
                    id: id.clone(),
                    last_watched_epoch: found.watch.last_viewed_unix,
                    progress: found.watch.watched_fraction,
                    rewatch_score: None,
                    source: WatchSource::Plex,
                };
                (id.clone(), entry)
            })
            .collect()
    }
}
