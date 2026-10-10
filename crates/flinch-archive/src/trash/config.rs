//! What the operator asks of the TRaSH sync, in Recyclarr's words so a
//! Recyclarr config reads across field by field: which guide profiles each
//! *arr gets, the upgrade ceiling, score overrides and relative adjustments,
//! the size table, and which source each instance reads (TRaSH-Guides or a
//! Profilarr Compliant Database, never both: two sources would fight over the
//! same profiles).

use super::language::LanguagePreset;
use super::presets;
use serde::{Deserialize, Serialize};

/// The TRaSH-Guides commit the sync reads by default. A pinned commit makes
/// the preview reproducible: the guide changes only when the operator (or a
/// FLINCH release) moves the pin, never under a running install.
pub const GUIDE_COMMIT: &str = "d6a23d6137f8549a66ade74846faf8bdf94abf2f";

/// The Dictionarry database (<https://github.com/Dictionarry-Hub/database>,
/// branch `v2`) at the commit the PCD source reads by default. Its `pcd.json`
/// declares `"license": "MIT"` (checked 2026-10-10; GitHub detects no
/// LICENSE file, so the manifest is the grant FLINCH relies on, and every
/// fetch reads it again first: see [`super::pcd`]).
pub const PCD_REPOSITORY: &str = "Dictionarry-Hub/database";
pub const PCD_COMMIT: &str = "faeeeaea5f1c87de6577222412f7544be7d04899";
/// The schema the database depends on (`pcd.json`: schema 1.1.0), tag
/// `1.1.0` of <https://github.com/Dictionarry-Hub/schema> (MIT).
pub const PCD_SCHEMA_REPOSITORY: &str = "Dictionarry-Hub/schema";
pub const PCD_SCHEMA_COMMIT: &str = "e1c2bd73d7003f254ad135eeefbbed7b47f095b1";

/// Off by default, and preview-only until the operator applies or switches
/// `apply` on: quality profiles decide what every future download costs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrashConfig {
    pub enabled: bool,
    /// Hours between previews (and, with `apply`, syncs). A changed setting or
    /// an apply request from the page refreshes sooner.
    pub schedule_hours: u32,
    /// Apply every previewed change on schedule, like a Recyclarr cron. Off:
    /// changes wait for the operator's apply on the Quality profiles page.
    pub apply: bool,
    /// Offer to delete custom formats nobody asked for and FLINCH did not
    /// create. Off: an operator's own formats are never deleted.
    pub delete_unmanaged_custom_formats: bool,
    /// Offer to delete quality profiles the sync does not manage and no movie
    /// or series uses. Off: no profile is ever deleted; on, a profile in use
    /// (or any profile while the library's use is unknown) still never is.
    pub delete_unused_profiles: bool,
    /// The TRaSH-Guides commit to read, 40 hex digits.
    pub guide_commit: String,
    /// The Profilarr Compliant Database instances with `source: pcd` read.
    pub pcd: PcdConfig,
    pub instances: Instances,
}

impl Default for TrashConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            schedule_hours: 24,
            apply: false,
            delete_unmanaged_custom_formats: false,
            delete_unused_profiles: false,
            guide_commit: GUIDE_COMMIT.to_string(),
            pcd: PcdConfig::default(),
            instances: Instances::default(),
        }
    }
}

/// A Profilarr Compliant Database and the schema it builds on, each a GitHub
/// `owner/name` at a pinned commit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PcdConfig {
    pub repository: String,
    pub commit: String,
    pub schema_repository: String,
    pub schema_commit: String,
}

impl Default for PcdConfig {
    fn default() -> Self {
        Self {
            repository: PCD_REPOSITORY.to_string(),
            commit: PCD_COMMIT.to_string(),
            schema_repository: PCD_SCHEMA_REPOSITORY.to_string(),
            schema_commit: PCD_SCHEMA_COMMIT.to_string(),
        }
    }
}

/// The default Radarr and Sonarr's instances, each defaulting to FLINCH's
/// built-in preset, and any extra instance's ([`crate::arr::instances`]) by
/// its key (`radarr@4k`). An extra instance without an entry is not synced:
/// a 4K or anime library rarely wants the HD preset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Instances {
    #[serde(default = "presets::radarr")]
    pub radarr: InstanceConfig,
    #[serde(default = "presets::sonarr")]
    pub sonarr: InstanceConfig,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub extra: std::collections::BTreeMap<String, InstanceConfig>,
}

impl Default for Instances {
    fn default() -> Self {
        Self { radarr: presets::radarr(), sonarr: presets::sonarr(), extra: std::collections::BTreeMap::new() }
    }
}

impl Instances {
    /// What `app`'s `instance` (empty: the default) syncs; `None` for an extra
    /// instance without an entry.
    pub fn of(&self, app: crate::capacity::App, instance: &str) -> Option<&InstanceConfig> {
        match (app, instance.is_empty()) {
            (crate::capacity::App::Radarr, true) => Some(&self.radarr),
            (crate::capacity::App::Sonarr, true) => Some(&self.sonarr),
            (_, false) => self.extra.get(&crate::ids::instance_key(app, instance)),
        }
    }
}

/// Where an instance's profiles and formats come from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    #[default]
    Trash,
    /// The Profilarr Compliant Database of [`TrashConfig::pcd`]. Its profiles
    /// and formats carry no trash ids; each gets [`super::pcd::id`] of its name.
    Pcd,
}

/// An instance as written is what is synced: an empty list manages nothing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InstanceConfig {
    pub source: Source,
    pub quality_profiles: Vec<ProfileConfig>,
    pub custom_formats: Vec<CustomFormatConfig>,
    pub quality_definition: Option<QualityDefinitionConfig>,
    /// TRaSH's language formats on every synced profile (TRaSH source only).
    pub language: Option<LanguagePreset>,
}

/// A guide quality profile and what the operator changes about it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileConfig {
    pub trash_id: String,
    /// The profile's name in the *arr; the guide's name when absent.
    #[serde(default)]
    pub name: Option<String>,
    /// Score 0 for every custom format this profile does not ask for.
    #[serde(default = "yes")]
    pub reset_unmatched_scores: bool,
    #[serde(default)]
    pub upgrade: Option<UpgradeConfig>,
    #[serde(default)]
    pub min_upgrade_format_score: Option<i32>,
    /// The quality ladder, best first; empty keeps the guide's. Qualities left
    /// out stay in the profile, disabled, below the ones listed.
    #[serde(default)]
    pub qualities: Vec<QualityConfig>,
    /// The compact profile FLINCH's downgrade advice moves items into.
    #[serde(default)]
    pub compact: bool,
    /// Scales every guide score of this profile (Recyclarr #208), rounded to
    /// the nearest whole score. An absolute `score` override is not scaled;
    /// an `adjust_score` is added after scaling.
    #[serde(default)]
    pub score_multiplier: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpgradeConfig {
    #[serde(default = "yes")]
    pub allowed: bool,
    #[serde(default)]
    pub until_quality: Option<String>,
    #[serde(default)]
    pub until_score: Option<i32>,
}

/// One rung of the ladder: a quality, or a group when `qualities` is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualityConfig {
    pub name: String,
    #[serde(default)]
    pub qualities: Vec<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
}

/// A guide custom format added to profiles, or a score override: `score`
/// replaces the guide's score, `adjust_score` moves it by that much (Recyclarr
/// #208), for every profile it names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomFormatConfig {
    pub trash_id: String,
    /// The guide's score when absent.
    #[serde(default)]
    pub score: Option<i32>,
    /// Added to the guide's (scaled) score; never with `score`.
    #[serde(default)]
    pub adjust_score: Option<i32>,
    /// Profile trash ids; empty means every profile of this instance.
    #[serde(default)]
    pub profiles: Vec<String>,
}

/// A guide size table (`movie`, `series`, `anime`, …) and where to place the
/// preferred size between min and max.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualityDefinitionConfig {
    #[serde(rename = "type")]
    pub kind: String,
    /// 0 puts preferred at min, 1 at max; the guide's preferred when absent.
    #[serde(default)]
    pub preferred_ratio: Option<f64>,
}

fn yes() -> bool {
    true
}

/// A [`TrashConfig`] outside its bounds; the message names the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidTrashConfig(pub &'static str);

fn hex(text: &str, length: usize) -> bool {
    text.len() == length && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A GitHub `owner/name`: it is spliced into URLs, so nothing else passes.
fn repository(text: &str) -> bool {
    let part = |part: &str| {
        !part.is_empty() && part != "." && part != ".." && part.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    };
    text.split_once('/').is_some_and(|(owner, name)| part(owner) && part(name))
}

impl TrashConfig {
    pub fn validate(&self) -> Result<(), InvalidTrashConfig> {
        if !(1..=720).contains(&self.schedule_hours) {
            return Err(InvalidTrashConfig("the TRaSH sync schedule (trash.schedule_hours) must be 1 to 720 hours"));
        }
        if !hex(&self.guide_commit, 40) {
            return Err(InvalidTrashConfig("the TRaSH-Guides commit (trash.guide_commit) must be 40 lowercase hex digits"));
        }
        if !repository(&self.pcd.repository) || !repository(&self.pcd.schema_repository) {
            return Err(InvalidTrashConfig("the PCD repositories (trash.pcd) must be GitHub owner/name"));
        }
        if !hex(&self.pcd.commit, 40) || !hex(&self.pcd.schema_commit, 40) {
            return Err(InvalidTrashConfig("the PCD commits (trash.pcd.commit, schema_commit) must be 40 lowercase hex digits"));
        }
        self.instances.radarr.validate()?;
        self.instances.sonarr.validate()?;
        for (key, instance) in &self.instances.extra {
            let named = crate::ids::ArrRef::parse(&format!("{key}-1")).is_some_and(|item| !item.instance.is_empty());
            if !named {
                return Err(InvalidTrashConfig("trash.instances.extra keys name an extra instance: radarr@<name> or sonarr@<name>"));
            }
            instance.validate()?;
        }
        Ok(())
    }

    /// Whether any instance reads `source`: the other is never fetched.
    pub fn uses(&self, source: Source) -> bool {
        [&self.instances.radarr, &self.instances.sonarr]
            .into_iter()
            .chain(self.instances.extra.values())
            .any(|instance| instance.source == source)
    }
}

impl InstanceConfig {
    fn validate(&self) -> Result<(), InvalidTrashConfig> {
        let mut profiles = std::collections::HashSet::new();
        for profile in &self.quality_profiles {
            if !hex(&profile.trash_id, 32) {
                return Err(InvalidTrashConfig("every quality profile trash_id must be 32 lowercase hex digits"));
            }
            if !profiles.insert(profile.trash_id.as_str()) {
                return Err(InvalidTrashConfig("a quality profile is listed twice for one app"));
            }
            if profile.name.as_deref().is_some_and(|name| name.trim().is_empty()) {
                return Err(InvalidTrashConfig("a quality profile name, when set, must not be blank"));
            }
            if profile.min_upgrade_format_score.is_some_and(|score| score < 1) {
                return Err(InvalidTrashConfig("min_upgrade_format_score must be at least 1 (Radarr and Sonarr refuse 0)"));
            }
            if profile
                .qualities
                .iter()
                .any(|quality| quality.name.trim().is_empty() || quality.qualities.iter().any(|q| q.trim().is_empty()))
            {
                return Err(InvalidTrashConfig("every quality in a profile's ladder needs a name"));
            }
            if !profile.qualities.is_empty() && !profile.qualities.iter().any(|quality| quality.enabled) {
                return Err(InvalidTrashConfig("a profile's quality ladder needs at least one enabled quality"));
            }
            if profile.score_multiplier.is_some_and(|m| !(0.0..=10.0).contains(&m)) {
                return Err(InvalidTrashConfig("score_multiplier must be between 0 and 10"));
            }
        }
        if self.quality_profiles.iter().filter(|profile| profile.compact).count() > 1 {
            return Err(InvalidTrashConfig("only one quality profile per app can be the compact one"));
        }
        for format in &self.custom_formats {
            if !hex(&format.trash_id, 32) {
                return Err(InvalidTrashConfig("every custom format trash_id must be 32 lowercase hex digits"));
            }
            if format.profiles.iter().any(|id| !profiles.contains(id.as_str())) {
                return Err(InvalidTrashConfig("a custom format names a profile this app does not sync"));
            }
            if format.score.is_some() && format.adjust_score.is_some() {
                return Err(InvalidTrashConfig("a custom format takes score or adjust_score, not both"));
            }
        }
        if let Some(definition) = &self.quality_definition {
            if definition.kind.trim().is_empty() {
                return Err(InvalidTrashConfig("the quality definition type must be a guide size table (movie, series, anime, …)"));
            }
            if definition.preferred_ratio.is_some_and(|ratio| !(0.0..=1.0).contains(&ratio)) {
                return Err(InvalidTrashConfig("preferred_ratio must be between 0 and 1"));
            }
        }
        if let Some(language) = &self.language {
            if self.source != Source::Trash {
                return Err(InvalidTrashConfig("the language preset uses TRaSH's language formats: it needs source trash"));
            }
            language.validate()?;
        }
        Ok(())
    }
}
