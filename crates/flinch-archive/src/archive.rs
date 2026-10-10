//! The archive tier: move an item to an archive root folder on another disk
//! instead of deleting it, so it stays playable from slower or bigger storage.
//!
//! - **Opt-in.** Off by default; each instance archives only when its root is
//!   set (the default instances' here, each extra one's on its own entry,
//!   [`crate::arr::instances`]).
//! - **The forecast decides how much fits.** An archive root must be a root
//!   folder of its *arr on a disk FLINCH measures: that disk's own forecast
//!   gives the headroom ([`headroom`]) the planner may fill, so archiving never
//!   pushes the archive disk past its target. A root FLINCH cannot measure
//!   archives nothing (fail closed) and is reported.
//! - **The planner picks.** [`crate::plan::generate_plan`] gives each movie and
//!   each whole series a move decision next to eviction (see
//!   [`crate::plan::knapsack::Moves`]); a move's regret is near zero, so the
//!   archive fills before anything is deleted, with the items whose deletion
//!   would hurt most.
//! - **The daemon acts.** Moves are FLINCH's own writes through the *arrs'
//!   editor (`PUT /api/v3/movie/editor`, `PUT /api/v3/series/editor` with
//!   `moveFiles: true`), whichever executor deletes; a dry run prints them.

use crate::capacity::{App, CapacityConfig, CapacityForecast, LibraryVolumes, VolumeForecast};
use crate::plan::ArchiveDestination;
use serde::{Deserialize, Serialize};

/// `settings.json` `archive`. Every field defaults individually.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ArchiveConfig {
    pub enabled: bool,
    /// Radarr root folder movies move to; blank: movies are never archived.
    pub radarr_root: String,
    /// Sonarr root folder series move to; blank: series are never archived.
    pub sonarr_root: String,
    /// Moves sent per cycle at most: each copies a whole movie or series.
    pub max_moves_per_run: usize,
}

impl Default for ArchiveConfig {
    fn default() -> Self {
        Self { enabled: false, radarr_root: "/archive/movies".to_string(), sonarr_root: "/archive/tv".to_string(), max_moves_per_run: 2 }
    }
}

/// An [`ArchiveConfig`] outside its bounds; the message names the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidArchiveConfig(pub &'static str);

/// An absolute path as either *arr may run: `/…`, `C:\…` or `\\server\…`.
fn absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/');
    (path.starts_with('/') || path.starts_with("\\\\") || drive) && !path.chars().any(char::is_control)
}

impl ArchiveConfig {
    pub fn validate(&self) -> Result<(), InvalidArchiveConfig> {
        let roots = [self.radarr_root.trim(), self.sonarr_root.trim()];
        if roots.iter().any(|root| !root.is_empty() && !absolute(root)) {
            return Err(InvalidArchiveConfig("archive roots must be absolute paths as the *arrs see them"));
        }
        if self.enabled && roots.iter().all(|root| root.is_empty()) {
            return Err(InvalidArchiveConfig("the archive needs a Radarr or a Sonarr root folder"));
        }
        if !(1..=50).contains(&self.max_moves_per_run) {
            return Err(InvalidArchiveConfig("archive moves per run must be 1 to 50"));
        }
        Ok(())
    }

    /// The default instances' roots, trimmed, for the apps that archive, as
    /// (app, instance, root) like [`destinations`] takes them.
    pub fn roots(&self) -> impl Iterator<Item = (App, &'static str, &str)> {
        [(App::Radarr, self.radarr_root.trim()), (App::Sonarr, self.sonarr_root.trim())]
            .into_iter()
            .filter(|(_, root)| self.enabled && !root.is_empty())
            .map(|(app, root)| (app, crate::ids::DEFAULT_INSTANCE, root))
    }
}

/// Bytes a volume can take while its projection stays under the target with
/// its buffer: `θ·capacity − buffer − projected`, never below 0. A volume
/// that must free bytes itself has none.
pub fn headroom(forecast: &CapacityForecast, config: &CapacityConfig) -> u64 {
    let safe = config.target_utilization * forecast.max_capacity_bytes as f64;
    let room = safe - config.headroom_buffer_bytes as f64 - forecast.projected_used_bytes as f64;
    if forecast.target_reclaim_bytes > 0 || !room.is_finite() {
        return 0;
    }
    room.max(0.0).floor() as u64
}

/// This run's destinations, one per (app, instance, root) given, and the
/// roots that cannot be one (on no disk the instance measures, or with no
/// forecast), named for the operator.
pub fn destinations<'i, 'r>(
    roots: impl IntoIterator<Item = (App, &'i str, &'r str)>,
    library: &LibraryVolumes,
    forecasts: &[VolumeForecast],
    capacity: &CapacityConfig,
) -> (Vec<ArchiveDestination>, Vec<String>) {
    let mut found = Vec::new();
    let mut unresolved = Vec::new();
    for (app, instance, root) in roots {
        let forecast =
            library.volume_of(app, instance, root).and_then(|volume| forecasts.iter().find(|forecast| forecast.volume == volume));
        match forecast {
            Some(forecast) => found.push(ArchiveDestination {
                app,
                instance: instance.to_string(),
                root: root.trim_end_matches(['/', '\\']).to_string(),
                volume: forecast.volume.clone(),
                headroom_bytes: headroom(&forecast.forecast, capacity),
            }),
            None => unresolved
                .push(format!("{}:{root} is not a root folder on a disk FLINCH measures", crate::ids::instance_key(app, instance))),
        }
    }
    (found, unresolved)
}

/// Whether an *arr path lies under `root` (either separator).
pub fn under_root(path: &str, root: &str) -> bool {
    let root = root.trim_end_matches(['/', '\\']);
    path.strip_prefix(root).is_some_and(|rest| rest.starts_with(['/', '\\']))
}

/// One move this cycle, for status.json.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedItem {
    pub id: String,
    pub title: String,
    pub bytes: u64,
    pub root: String,
    /// The path the *arr reads back; `None` in a dry run.
    pub path: Option<String>,
}

/// The archive tier this cycle (status.json `archive`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ArchiveStatus {
    pub dry_run: bool,
    pub destinations: Vec<ArchiveDestination>,
    /// Roots that cannot take anything this cycle, and why.
    pub unresolved: Vec<String>,
    /// Moves in this cycle's plan.
    pub planned: usize,
    pub planned_bytes: u64,
    /// Planned moves still earning their grace streak, or held by the cap.
    pub waiting: usize,
    /// Moves sent and read back at the new root (printed, in a dry run).
    pub moved: Vec<ArchivedItem>,
    /// Moves that failed, with the *arr's reason.
    pub failed: Vec<String>,
}

#[cfg(test)]
mod tests;
