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
/// the ones with a file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SonarrEpisodes {
    numbered: Seasons,
    with_files: Seasons,
}

impl SonarrEpisodes {
    /// From `/api/v3/episode?seriesId=` rows. A row that does not parse, or
    /// whose TVDB id is 0 (unknown to TVDB), is skipped.
    pub fn from_rows(rows: &[serde_json::Value]) -> Self {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Row {
            season_number: u32,
            tvdb_id: u32,
            #[serde(default)]
            has_file: bool,
        }
        let mut episodes = Self::default();
        for row in rows.iter().filter_map(|row| Row::deserialize(row).ok()).filter(|row| row.tvdb_id > 0) {
            episodes.numbered.entry(row.season_number).or_default().insert(row.tvdb_id);
            if row.has_file {
                episodes.with_files.entry(row.season_number).or_default().insert(row.tvdb_id);
            }
        }
        episodes
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
