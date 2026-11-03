//! Season identity from TVDB episode ids, for seasons whose episode counts
//! differ.
//!
//! A season normally resolves only when Plex holds exactly as many episodes as
//! Sonarr has files for it, because the two can group episodes into seasons
//! differently: Sonarr numbers by TVDB's aired order, a Plex library may follow
//! TMDB's. Counts also differ for innocent reasons — Plex has not scanned a new
//! download yet, or holds an episode Sonarr has no file for — and then the
//! episodes' own TVDB ids settle it. Plex's season is Sonarr's season when
//! - it shares at least one episode with Sonarr's files for that season,
//! - every episode Plex files under it is one Sonarr numbers into that season,
//! - and none of Sonarr's files for that season sits in another Plex season.
//!
//! A different grouping breaks one of the last two, so it stays unresolved.

use super::guid::{parse, CatalogueId};
use super::PlexMetadata;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};

type Seasons = BTreeMap<u32, BTreeSet<u32>>;

/// A show's episodes as Plex groups them: season number → TVDB episode ids.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlexEpisodes(Seasons);

impl PlexEpisodes {
    /// From `/library/metadata/{show}/allLeaves?includeGuids=1` rows. An episode
    /// without a season number or a `tvdb://` GUID says nothing and is skipped.
    pub fn from_rows(rows: &[PlexMetadata]) -> Self {
        let mut seasons = Seasons::new();
        for row in rows {
            let tvdb = row.guids.iter().find_map(|guid| match parse(&guid.id) {
                Some(CatalogueId::Tvdb(id)) => Some(id),
                _ => None,
            });
            if let (Some(season), Some(id)) = (row.parent_index, tvdb) {
                seasons.entry(season).or_default().insert(id);
            }
        }
        Self(seasons)
    }
}

/// A series' episodes as Sonarr numbers them, per season: every episode, and
/// the ones with a file — by TVDB id for confirming seasons, and by episode
/// number for counting plays against what is on disk — and the ones Sonarr
/// still wants: monitored, with no file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SonarrEpisodes {
    numbered: Seasons,
    with_files: Seasons,
    /// Season → episode numbers with a file.
    numbers_on_disk: Seasons,
    /// A row did not parse, or had a file but no episode number: the numbers
    /// on disk may be missing one, so none of them are trusted.
    numbers_incomplete: bool,
    /// Season → the first airing (unix seconds) of its monitored episodes
    /// without a file. One with no air date has not aired and is left out.
    wanted_since: BTreeMap<u32, u64>,
    /// A row did not parse or did not say whether it is monitored: which
    /// episodes Sonarr wants is unknown.
    monitoring_incomplete: bool,
}

impl SonarrEpisodes {
    /// From `/api/v3/episode?seriesId=` rows. A row that does not parse, or
    /// whose TVDB id is 0 (unknown to TVDB), says nothing about TVDB ids.
    pub fn from_rows(rows: &[serde_json::Value]) -> Self {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Row {
            season_number: u32,
            #[serde(default)]
            episode_number: Option<u32>,
            #[serde(default)]
            tvdb_id: Option<u32>,
            #[serde(default)]
            has_file: bool,
            #[serde(default)]
            monitored: Option<bool>,
            #[serde(default)]
            air_date_utc: Option<String>,
        }
        let mut episodes = Self::default();
        for row in rows {
            let Ok(row) = Row::deserialize(row) else {
                episodes.numbers_incomplete = true;
                episodes.monitoring_incomplete = true;
                continue;
            };
            match row.monitored {
                None => episodes.monitoring_incomplete = true,
                Some(true) if !row.has_file => {
                    if let Some(aired) = row.air_date_utc.as_deref().and_then(crate::presence::parse_utc) {
                        let first = episodes.wanted_since.entry(row.season_number).or_insert(aired);
                        *first = (*first).min(aired);
                    }
                }
                Some(_) => {}
            }
            if row.has_file {
                match row.episode_number {
                    Some(number) => {
                        episodes.numbers_on_disk.entry(row.season_number).or_default().insert(number);
                    }
                    None => episodes.numbers_incomplete = true,
                }
            }
            let Some(tvdb_id) = row.tvdb_id.filter(|id| *id > 0) else { continue };
            episodes.numbered.entry(row.season_number).or_default().insert(tvdb_id);
            if row.has_file {
                episodes.with_files.entry(row.season_number).or_default().insert(tvdb_id);
            }
        }
        episodes
    }

    /// Whether Sonarr still wants an episode of `season` that has aired by
    /// `now`: monitored, with no file. A monitored episode yet to air is a new
    /// download, not one coming back. `None` when a row did not say.
    pub fn wants_aired(&self, season: u32, now: u64) -> Option<bool> {
        (!self.monitoring_incomplete).then(|| self.wanted_since.get(&season).is_some_and(|aired| *aired <= now))
    }

    /// Episode numbers of `season` with a file, ascending; `files` is Sonarr's
    /// file count for it. `None` unless every row read and the numbers cover at
    /// least `files` (a file holds one episode or more): a number missing here
    /// would let the plays of the others complete the season.
    pub fn on_disk(&self, season: u32, files: u32) -> Option<Vec<u32>> {
        if self.numbers_incomplete {
            return None;
        }
        let numbers: Vec<u32> = self.numbers_on_disk.get(&season)?.iter().copied().collect();
        (numbers.len() >= files as usize && files > 0).then_some(numbers)
    }
}

/// Whether Plex's season `season` is Sonarr's season `season` (see the module
/// docs).
pub fn same_season(season: u32, plex: &PlexEpisodes, sonarr: &SonarrEpisodes) -> bool {
    let (Some(held), Some(numbered), Some(files)) = (plex.0.get(&season), sonarr.numbered.get(&season), sonarr.with_files.get(&season))
    else {
        return false;
    };
    let shares_files = !held.is_disjoint(files);
    let nothing_foreign = held.is_subset(numbered);
    let nothing_elsewhere = plex.0.iter().all(|(index, ids)| *index == season || ids.is_disjoint(files));
    shares_files && nothing_foreign && nothing_elsewhere
}

/// Episode ids read for the seasons whose counts disagreed.
#[derive(Debug, Clone, Default)]
pub struct EpisodeIds {
    /// Plex show ratingKey → the show's episodes.
    pub plex: HashMap<String, PlexEpisodes>,
    /// Season target id → its series' episodes.
    pub sonarr: HashMap<String, SonarrEpisodes>,
}

impl EpisodeIds {
    /// Whether the ids confirm `target_id`'s season `season` under the Plex show
    /// `show_rating_key`. Anything not read confirms nothing.
    pub fn confirm(&self, target_id: &str, show_rating_key: &str, season: u32) -> bool {
        match (self.sonarr.get(target_id), self.plex.get(show_rating_key)) {
            (Some(sonarr), Some(plex)) => same_season(season, plex, sonarr),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests;
