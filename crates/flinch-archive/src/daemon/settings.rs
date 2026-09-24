//! Operator settings: `settings.json` on the shared volume, written by the web
//! UI and re-read by the daemon every cycle.

/// Operator-adjustable settings, re-read every cycle so the UI can change them
/// without a redeploy (the Maintainerr-style settings surface).
///
/// Every field defaults individually (`#[serde(default)]` on the struct, fed by
/// the one `Default` impl): a missing or new field must never reset the
/// operator's other choices — least of all an opt-out of never-played reclaim.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RuntimeSettings {
    /// Seconds between scheduled runs.
    pub interval_s: u64,
    /// Consecutive candidate appearances required before scheduling.
    pub grace_runs: u32,
    pub max_items: usize,
    pub max_gib: u64,
    /// Hand candidates to Maintainerr rather than only listing them.
    pub enforce: bool,
    pub collection_movie: String,
    pub collection_season: String,
    /// Maintainerr collection that announces evictions nobody finished: shown
    /// in Plex, deleting only after its window, so the household can still
    /// claim an item by playing it. One title for both kinds, each bound to its
    /// own library. Blank sends them straight to the delete collections.
    pub collection_leaving: String,
    /// Minimum calibrated P(safe) before an item may be scheduled. Thresholds
    /// are meaningless until the score is calibrated; this is where the Laya
    /// lesson lands.
    pub score_floor: f32,
    /// Temperature applied to the raw logit. >1 softens an overconfident model.
    pub score_temperature: f32,
    /// Reclaim items nobody ever played when the calibrated score clears the
    /// floor. Off by default: this is the capability Maintainerr cannot express,
    /// and deleting on the strength of absent evidence is the operator's call.
    pub unwatched_reclaim_enabled: bool,
    pub unwatched_reclaim_floor: f32,
    pub unwatched_reclaim_dwell_days: f32,
    /// Plex base URL + token; empty means "no watch source", guard stays closed.
    pub plex_url: String,
    pub plex_token: String,
    /// The storage ceiling as a fraction (0.80 = 80%). Crossing it starts
    /// eviction on that volume; below it nothing is deleted.
    pub capacity_ceiling: f32,
    /// Where a latched eviction stops (0.75 = 75%). Below the ceiling so one
    /// finished download does not trigger one more delete.
    pub capacity_release: f32,
    /// While evicting, arm the calibrated never-played rule as an extra
    /// candidate source (still gated by its own floor and dwell). Off: the
    /// volume may stay over budget, and the status says so instead of guessing.
    pub capacity_arm_never_played: bool,
    /// Radarr/Sonarr tag label that makes an item untouchable (a hard guard,
    /// like a favorite). Empty disables it.
    pub keep_tag: String,
}

impl Default for RuntimeSettings {
    fn default() -> Self {
        Self {
            interval_s: 3600,
            grace_runs: 2,
            max_items: 10,
            max_gib: 50,
            enforce: false,
            collection_movie: "Watched Movies Cleanup".to_string(),
            collection_season: "Watched Seasons Cleanup".to_string(),
            collection_leaving: "Leaving Soon".to_string(),
            score_floor: 0.75,
            score_temperature: 1.6,
            unwatched_reclaim_enabled: false,
            unwatched_reclaim_floor: 0.75,
            unwatched_reclaim_dwell_days: 90.0,
            plex_url: String::new(),
            plex_token: String::new(),
            capacity_ceiling: 0.80,
            capacity_release: 0.75,
            capacity_arm_never_played: true,
            keep_tag: "flinch-keep".to_string(),
        }
    }
}

impl RuntimeSettings {
    /// The Maintainerr collections the operator named. Each library type has
    /// its own delete collection: a movie handed to a season collection is an
    /// invalid target, not a deletion. A blank title hands nothing of that kind
    /// (the sync reports the collection as missing).
    pub fn collection_titles(&self) -> crate::maintainerr::CollectionTitles {
        crate::maintainerr::CollectionTitles {
            movie: self.collection_movie.clone(),
            season: self.collection_season.clone(),
            leaving: self.collection_leaving.clone(),
        }
    }
}

/// Why settings could not be loaded. A missing file is not an error (first
/// run: defaults); anything else is, so the caller can keep the last good
/// settings instead of silently reverting every choice to its default.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("settings unreadable: {0}")]
    Io(#[from] std::io::Error),
    #[error("settings malformed: {0}")]
    Parse(#[from] serde_json::Error),
}

pub fn read_settings(path: &std::path::Path) -> Result<RuntimeSettings, SettingsError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(serde_json::from_str(&text)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(RuntimeSettings::default()),
        Err(error) => Err(error.into()),
    }
}

pub fn write_settings(path: &std::path::Path, settings: &RuntimeSettings) -> std::io::Result<()> {
    crate::persist::replace(path, &serde_json::to_vec_pretty(settings)?)
}
