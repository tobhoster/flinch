//! Duplicate copies: find them, recommend which one stays, and remove the
//! rest once the operator confirmed.
//!
//! Before this, an item Plex held twice was only ever *held*
//! ([`crate::maintainerr::Blocked::SeveralPlexCopies`]): duplicates never
//! competed and nobody was told which copy wasted the space. Three kinds are
//! found ([`group`]):
//! - a movie with several Plex `Media` (versions), in one item or merged by
//!   GUID across sections;
//! - the same catalogue id in several *arr instances (copies are keyed by
//!   instance-qualified ids, so a second Radarr merges in when it appears);
//! - large video folders under an *arr root folder that no *arr item owns,
//!   the biggest share of the volume's "not library media". Only listed:
//!   FLINCH has no API that removes them, and they may be the operator's.
//!
//! The recommendation ([`pick`]) keeps the copy an *arr tracks (deleting it
//! would only make the *arr download it again), then the copy the household
//! played, then the operator's intent (highest quality, or HD where quality
//! advice says downgrade). Nothing is removed on a recommendation: the
//! operator chooses and confirms in the UI (`dupes.json`, [`decisions`]), and
//! acting is a separate opt-in that honours the dry run ([`remove`]).

use serde::{Deserialize, Serialize};

pub mod decisions;
pub mod group;
pub mod pick;
pub mod remove;

#[cfg(test)]
mod tests;

pub use decisions::{Acted, Decision, Decisions};

/// Settings → Duplicates. Off by default: finding reads extra Plex and *arr
/// data, and acting deletes files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DupesConfig {
    /// Find duplicates and publish them with a recommendation.
    pub enabled: bool,
    /// Remove the redundant copies of confirmed choices.
    pub act: bool,
    /// Which copy the recommendation prefers when nothing else decides.
    pub prefer: KeepPreference,
    /// Redundant copies removed per cycle at most.
    pub max_per_run: u32,
    /// Unowned folders smaller than this are not listed.
    pub unowned_min_gib: u32,
    /// Unowned folders measured per *arr per cycle (one Radarr/Sonarr
    /// manual-import scan each); 0 lists none.
    pub unowned_max_folders: u32,
}

impl Default for DupesConfig {
    fn default() -> Self {
        Self { enabled: false, act: false, prefer: KeepPreference::Highest, max_per_run: 3, unowned_min_gib: 2, unowned_max_folders: 25 }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct InvalidDupesConfig(pub &'static str);

impl DupesConfig {
    pub fn validate(&self) -> Result<(), InvalidDupesConfig> {
        if !(1..=50).contains(&self.max_per_run) {
            return Err(InvalidDupesConfig("duplicate removals per run (dupes.max_per_run) must be between 1 and 50"));
        }
        if !(1..=10_000).contains(&self.unowned_min_gib) {
            return Err(InvalidDupesConfig("the unowned folder size (dupes.unowned_min_gib) must be between 1 and 10000 GiB"));
        }
        if self.unowned_max_folders > 200 {
            return Err(InvalidDupesConfig("unowned folders measured (dupes.unowned_max_folders) must be at most 200"));
        }
        Ok(())
    }
}

/// The intent behind several copies: a 4K library beside an HD one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeepPreference {
    /// Keep the best picture.
    #[default]
    Highest,
    /// Keep 1080p (then 720p) over 4K: the space matters more.
    Hd,
}

/// Where a copy is seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// A Plex `Media`, possibly also an *arr's file ([`Copy::owner`]).
    Plex,
    /// An *arr's file Plex does not list.
    Arr,
}

/// The file an *arr instance tracks for a movie.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrFile {
    /// The instance key ([`crate::ids::instance_key`]): `radarr`, `radarr@4k`.
    pub instance: String,
    pub movie_id: u32,
    pub file_id: u32,
    pub path: String,
    pub bytes: u64,
    #[serde(default)]
    pub quality: Option<String>,
}

/// One physical copy of a movie.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Copy {
    /// `plex:{ratingKey}:{mediaId}` or `{instance}:{movieId}:{fileId}`.
    pub id: String,
    pub source: Source,
    #[serde(default)]
    pub rating_key: Option<String>,
    #[serde(default)]
    pub section_id: Option<u32>,
    #[serde(default)]
    pub media_id: Option<u64>,
    /// The *arr file this copy is; `None` when no *arr tracks it.
    #[serde(default)]
    pub owner: Option<ArrFile>,
    /// `2160`, `1080`, `720`, `sd`, or `None` when unknown.
    #[serde(default)]
    pub resolution: Option<String>,
    pub bytes: u64,
    /// The copy's (first) file.
    #[serde(default)]
    pub file: Option<String>,
    /// Plays logged under the copy's ratingKey; Plex does not say which
    /// `Media` of one item was played, so versions of one item share them.
    pub plays: u32,
}

/// Copies of one movie, with the copy FLINCH recommends keeping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    /// `tmdb:{id}`, or `plex:{ratingKey}` without one.
    pub id: String,
    #[serde(default)]
    pub card_id: Option<String>,
    pub title: String,
    #[serde(default)]
    pub year: Option<u32>,
    pub copies: Vec<Copy>,
    /// The recommended copy's id.
    pub recommended: String,
    /// Why, most decisive first.
    pub reasons: Vec<String>,
    /// The operator's choice, as `dupes.json` holds it.
    #[serde(default)]
    pub decision: Option<Decision>,
    /// Why a confirmed choice is not acted on this cycle.
    #[serde(default)]
    pub held: Option<String>,
    /// What removing every copy but the recommended one would free.
    pub redundant_bytes: u64,
}

impl Group {
    pub fn copy(&self, id: &str) -> Option<&Copy> {
        self.copies.iter().find(|copy| copy.id == id)
    }

    /// The operator's confirmed choice, when it still names this cycle's
    /// copies: a copy added or gone since means choosing again.
    pub fn confirmed(&self) -> Option<&Decision> {
        self.decision.as_ref().filter(|decision| decision.confirmed && decision.matches(self))
    }
}

/// A folder under an *arr root that no *arr item owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnownedFolder {
    pub app: String,
    pub path: String,
    pub bytes: u64,
    pub files: u32,
}

/// `status.dupes`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DupesStatus {
    /// Whether confirmed choices are acted on.
    pub act: bool,
    pub dry_run: bool,
    pub groups: Vec<Group>,
    /// Largest first.
    pub unowned: Vec<UnownedFolder>,
    /// Removals of the last 30 days, newest first.
    pub acted: Vec<Acted>,
    /// Reads that failed this cycle; their duplicates may be missing.
    pub problems: Vec<String>,
}
