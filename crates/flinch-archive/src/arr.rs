//! Minimal, honest *arr inventory clients.
//!
//! Scope: exactly the fields the archive reflex needs. A fat client for a whole
//! media suite would be noise; these two structs fetch library items and map
//! them to cards. Watch state is NOT here on purpose — *arr tracks files and
//! statistics, not "has the household watched it". That lives in the media
//! server and arrives via `watch.rs`.

use crate::card::{ArchiveCard, LibraryKind, SeriesType};
use serde::{Deserialize, Serialize};

mod dwell;
pub mod history;

use dwell::{chrono_lite, days_on_disk};

/// One volume from `/api/v3/diskspace` (Radarr and Sonarr both serve it).
/// The *arrs own the files, so they are the authoritative witness for how full
/// the library volume is — FLINCH never mounts the store and never has to.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArrDiskSpace {
    pub path: String,
    pub free_space: u64,
    pub total_space: u64,
}

impl From<&ArrDiskSpace> for crate::capacity::Volume {
    fn from(entry: &ArrDiskSpace) -> Self {
        crate::capacity::Volume {
            path: entry.path.clone(),
            total_bytes: entry.total_space,
            free_bytes: entry.free_space,
        }
    }
}

/// *arr artwork. `remote_url` is the upstream (TMDB/TVDB) URL, which a browser
/// can load directly; `url` is the arr-local path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArrImage {
    #[serde(default)]
    pub cover_type: String,
    #[serde(default)]
    pub remote_url: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QualityName {
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Quality {
    pub quality: QualityName,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MovieFile {
    #[serde(default)]
    pub quality: Option<Quality>,
    /// When the file arrived: the dwell clock. `added` on the movie is when
    /// it was *requested*, which can be years before a release existed.
    #[serde(default)]
    pub date_added: Option<String>,
}

/// `null` reads as the default: the *arrs send null statistics for items they
/// have not measured yet, and one such row must not fail the whole library.
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArrMovie {
    pub id: u32,
    pub title: String,
    /// What the Radarr web UI routes by (`/movie/<slug>`); the numeric id is not.
    #[serde(default)]
    pub title_slug: Option<String>,
    pub year: Option<u32>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub size_on_disk: u64,
    /// `null` (unmeasured) reads as no file: no card, nothing to reclaim.
    #[serde(default, deserialize_with = "null_as_default")]
    pub has_file: bool,
    #[serde(default)]
    pub added: Option<String>,
    #[serde(default)]
    pub images: Vec<ArrImage>,
    #[serde(default)]
    pub movie_file: Option<MovieFile>,
    /// The movie's folder (`/media/movies/Film (2020)`): which library volume
    /// its bytes live on, so eviction only frees the disk that needs it.
    #[serde(default)]
    pub path: Option<String>,
    /// Catalogue ids: the exact join to Plex (GUIDs), never the title.
    #[serde(default)]
    pub tmdb_id: Option<u32>,
    #[serde(default)]
    pub imdb_id: Option<String>,
    #[serde(default)]
    pub tags: Vec<u32>,
    #[serde(default)]
    pub monitored: Option<bool>,
    #[serde(default)]
    pub genres: Vec<String>,
    /// Carries the operator's keep tag (resolved by the daemon from tag
    /// labels): a hard guard, exactly like a favorite.
    #[serde(skip)]
    pub keep: bool,
    /// When its files were on disk, from *arr history (set by the daemon, see
    /// [`crate::presence`]); empty when there is none.
    #[serde(skip)]
    pub on_disk: Vec<crate::presence::Span>,
}

/// Sonarr 4.x nests per-season statistics under `statistics` (verified against
/// the live homelab API 2026-09-21).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeasonStats {
    #[serde(default, deserialize_with = "null_as_default")]
    pub episode_file_count: u32,
    #[serde(default, deserialize_with = "null_as_default")]
    pub episode_count: u32,
    #[serde(default, deserialize_with = "null_as_default")]
    pub total_episode_count: u32,
    #[serde(default, deserialize_with = "null_as_default")]
    pub size_on_disk: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesSeason {
    pub season_number: u32,
    /// Whether Sonarr searches for this season's missing episodes.
    #[serde(default)]
    pub monitored: Option<bool>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub statistics: SeasonStats,
    /// Newest file arrival in this season (from `/api/v3/episodefile`, filled
    /// by the daemon): the season's dwell clock. Unknown reads as fresh.
    #[serde(default)]
    pub files_added: Option<String>,
    /// When this season had files on disk (see [`ArrMovie::on_disk`]).
    #[serde(skip)]
    pub on_disk: Vec<crate::presence::Span>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArrSeries {
    pub id: u32,
    pub title: String,
    /// What the Sonarr web UI routes by (`/series/<slug>`); the numeric id is not.
    #[serde(default)]
    pub title_slug: Option<String>,
    #[serde(default)]
    pub year: Option<u32>,
    pub series_type: String,
    pub seasons: Vec<SeriesSeason>,
    #[serde(default)]
    pub added: Option<String>,
    #[serde(default)]
    pub images: Vec<ArrImage>,
    /// The series folder, for volume attribution (see [`ArrMovie::path`]).
    #[serde(default)]
    pub path: Option<String>,
    /// "continuing" | "ended" | "upcoming" | "deleted".
    #[serde(default)]
    pub status: Option<String>,
    /// ISO datetime of the most recent aired episode; absent before the first.
    #[serde(default)]
    pub previous_airing: Option<String>,
    /// Catalogue ids shared by every season of the show.
    #[serde(default)]
    pub tvdb_id: Option<u32>,
    #[serde(default)]
    pub tmdb_id: Option<u32>,
    #[serde(default)]
    pub imdb_id: Option<String>,
    #[serde(default)]
    pub tags: Vec<u32>,
    #[serde(default)]
    pub monitored: Option<bool>,
    #[serde(default)]
    pub genres: Vec<String>,
    /// Carries the operator's keep tag (see [`ArrMovie::keep`]).
    #[serde(skip)]
    pub keep: bool,
}

/// A catalogue id the *arrs report as `0` or `""` when unknown is no id.
fn known_number(id: Option<u32>) -> Option<u32> {
    id.filter(|id| *id > 0)
}

fn known_text(id: Option<&str>) -> Option<String> {
    id.map(str::trim).filter(|id| !id.is_empty()).map(str::to_string)
}

/// Card id → the catalogue ids its *arr knows; seasons carry their show's ids
/// (the season itself is identified by its number, on the card).
pub fn external_ids(movies: &[ArrMovie], series: &[ArrSeries]) -> std::collections::HashMap<String, crate::ids::ExternalIds> {
    let mut ids = std::collections::HashMap::new();
    for movie in movies {
        ids.insert(format!("radarr-{}", movie.id), movie.external_ids());
    }
    for show in series {
        let show_ids = show.external_ids();
        for season in &show.seasons {
            ids.insert(format!("sonarr-{}-s{}", show.id, season.season_number), show_ids.clone());
        }
    }
    ids
}

/// One entry of `/api/v3/rootfolder`: where an *arr keeps its library.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArrRootFolder {
    pub path: String,
    /// `false` when the *arr cannot reach the folder (unmounted share): its
    /// mount would fall back to `/`, so such a root is left ungoverned.
    #[serde(default)]
    pub accessible: Option<bool>,
    /// Free bytes the *arr measured at the folder itself: the one reading that
    /// shows which filesystem the folder is really on.
    #[serde(default)]
    pub free_space: Option<u64>,
}

impl From<ArrRootFolder> for crate::capacity::RootFolder {
    fn from(root: ArrRootFolder) -> Self {
        crate::capacity::RootFolder { path: root.path, free_bytes: root.free_space }
    }
}

/// The part of `/api/v3/config/mediamanagement` that decides when a delete
/// frees space: the recycle bin (empty path = disabled) and its cleanup days
/// (0 = never emptied).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArrMediaManagement {
    #[serde(default)]
    pub recycle_bin: String,
    #[serde(default)]
    pub recycle_bin_cleanup_days: u32,
}

/// Poster preference: upstream CDN first (browser-loadable), then arr-local.
pub fn poster_url(images: &[ArrImage]) -> Option<String> {
    images
        .iter()
        .find(|i| i.cover_type == "poster")
        .and_then(|i| i.remote_url.clone().or_else(|| i.url.clone()))
}

impl ArrMovie {
    pub fn external_ids(&self) -> crate::ids::ExternalIds {
        crate::ids::ExternalIds {
            tmdb: known_number(self.tmdb_id),
            tvdb: None,
            imdb: known_text(self.imdb_id.as_deref()),
        }
    }

    pub fn quality(&self) -> Option<String> {
        self.movie_file
            .as_ref()
            .and_then(|f| f.quality.as_ref())
            .map(|q| q.quality.name.clone())
            .filter(|name| !name.is_empty())
    }

    pub fn to_card(&self) -> Option<ArchiveCard> {
        if !self.has_file {
            return None; // nothing on disk, nothing to reclaim
        }
        Some(ArchiveCard {
            id: format!("radarr-{}", self.id),
            title: self.title.clone(),
            kind: LibraryKind::Movie,
            size_bytes: self.size_on_disk,
            added_days_ago: days_on_disk(&self.on_disk, self.movie_file.as_ref().and_then(|f| f.date_added.as_deref())),
            last_watched_days: None, // filled by WatchState::apply
            in_keep_collection: false,
            is_favorite: self.keep,
            duplicate_count: 0,
            series_type: None,
            season_state: None,
            season_index: None,
            is_newest_season: None,
            episodes_total: None,
            episodes_watched: None,
            is_watched: None,
            rewatch_score: None,
            movie_year: self.year,
            show_title: None,
        })
    }
}

impl ArrSeries {
    pub fn external_ids(&self) -> crate::ids::ExternalIds {
        crate::ids::ExternalIds {
            tmdb: known_number(self.tmdb_id),
            tvdb: known_number(self.tvdb_id),
            imdb: known_text(self.imdb_id.as_deref()),
        }
    }

    /// When the series last aired an episode, as unix seconds.
    pub fn last_aired_epoch(&self) -> Option<u64> {
        self.previous_airing.as_deref().and_then(chrono_lite)
    }

    pub fn to_cards(&self) -> Vec<ArchiveCard> {
        let mut out = Vec::new();
        // "Newest" counts only seasons that ARE on disk. An announced future
        // season with no files must not shield the latest download from the
        // archive reflex — the household cannot be mid-binge on nothing.
        let on_disk: Vec<&SeriesSeason> = self
            .seasons
            .iter()
            .filter(|s| s.statistics.episode_file_count > 0 || s.statistics.size_on_disk > 0)
            .collect();
        let newest: Option<u32> = on_disk.iter().map(|s| s.season_number).max();
        for season in on_disk {
            let stats = &season.statistics;
            let kind = match self.series_type.as_str() {
                "anime" => SeriesType::Anime,
                "documentary" => SeriesType::Documentary,
                "reality" => SeriesType::Reality,
                _ => SeriesType::Standard,
            };
            out.push(ArchiveCard {
                id: format!("sonarr-{}-s{}", self.id, season.season_number),
                title: format!("{} S{}", self.title, season.season_number),
                kind: LibraryKind::Season,
                size_bytes: stats.size_on_disk,
                added_days_ago: days_on_disk(&season.on_disk, season.files_added.as_deref()),
                last_watched_days: None,
                in_keep_collection: false,
                is_favorite: self.keep,
                duplicate_count: 0,
                series_type: Some(kind),
                season_state: None,
                season_index: Some(season.season_number),
                is_newest_season: Some(newest == Some(season.season_number)),
                // Episodes on disk, not every announced one: an unaired or
                // missing episode would keep a fully watched season from ever
                // reading as completed.
                episodes_total: Some(stats.episode_file_count),
                episodes_watched: None,
                is_watched: None,
                rewatch_score: None,
                movie_year: None,
                show_title: Some(self.title.clone()),
            });
        }
        out
    }
}

#[cfg(test)]
mod tests;
