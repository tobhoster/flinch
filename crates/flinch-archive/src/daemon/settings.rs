//! Operator settings: `settings.json` on the shared volume, written by the web
//! UI and re-read by the daemon every cycle.

/// Operator-adjustable settings, re-read every cycle so the UI can change them
/// without a redeploy (the Maintainerr-style settings surface).
///
/// Every field defaults individually (`#[serde(default)]` on the struct, fed by
/// the one `Default` impl): a missing or new field must never reset the
/// operator's other choices. A field this version dropped is ignored, so an
/// older file still loads; a dropped `enforce: true` reads as a dry run.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RuntimeSettings {
    /// Seconds between scheduled runs.
    pub interval_s: u64,
    /// Consecutive plans an item must appear in before it is handed over.
    pub grace_runs: u32,
    pub max_items: usize,
    pub max_gib: u64,
    pub collection_movie: String,
    pub collection_season: String,
    /// Maintainerr collection that announces evictions nobody finished: shown
    /// in Plex, deleting only after its window, so the household can still
    /// claim an item by playing it. One title for both kinds, each bound to its
    /// own library. Blank holds never-played reclaim off (see
    /// [`super::NeverPlayedHold`]).
    pub collection_leaving: String,
    /// Let items nobody ever played be candidates. Off by default: deleting on
    /// the strength of absent evidence is the operator's call.
    pub unwatched_reclaim_enabled: bool,
    /// Plex base URL + token; empty means "no watch source", guard stays closed.
    pub plex_url: String,
    pub plex_token: String,
    /// Radarr/Sonarr tag label that pins an item (like a favorite). Empty
    /// disables it.
    pub keep_tag: String,
    pub capacity: crate::capacity::CapacityConfig,
    pub planner: crate::plan::PlannerConfig,
}

impl Default for RuntimeSettings {
    fn default() -> Self {
        Self {
            interval_s: 3600,
            grace_runs: 2,
            max_items: 10,
            max_gib: 50,
            collection_movie: "Watched Movies Cleanup".to_string(),
            collection_season: "Watched Seasons Cleanup".to_string(),
            collection_leaving: "Leaving Soon".to_string(),
            unwatched_reclaim_enabled: false,
            plex_url: String::new(),
            plex_token: String::new(),
            keep_tag: "flinch-keep".to_string(),
            capacity: crate::capacity::CapacityConfig::default(),
            planner: crate::plan::PlannerConfig::default(),
        }
    }
}

impl RuntimeSettings {
    /// The Maintainerr collections the operator named. Each library type has
    /// its own delete collection: a movie handed to a season collection is an
    /// invalid target, not a deletion. A blank title hands nothing of that kind
    /// (the sync reports the title as missing).
    pub fn collection_titles(&self) -> crate::maintainerr::CollectionTitles {
        crate::maintainerr::CollectionTitles {
            movie: self.collection_movie.clone(),
            season: self.collection_season.clone(),
            leaving: self.collection_leaving.clone(),
        }
    }

    /// The bounds the Settings page enforces, held here too so a hand-edited
    /// file or a scripted PUT cannot hand the daemon a value the page would
    /// refuse. `NaN` fails every range check, so non-finite floats are refused
    /// by the same comparisons.
    pub fn validate(&self) -> Result<(), SettingsError> {
        if self.interval_s < 300 {
            return Err(SettingsError::Invalid("the scan interval (interval_s) must be at least 300 seconds"));
        }
        if !(1..=20).contains(&self.grace_runs) {
            return Err(SettingsError::Invalid("grace runs (grace_runs) must be between 1 and 20"));
        }
        self.capacity.validate().map_err(|error| SettingsError::Invalid(error.0))?;
        self.planner.validate().map_err(|error| SettingsError::Invalid(error.0))
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
    /// Parsed, but outside the bounds the Settings page allows. Carries the
    /// field and its range in plain words: the UI shows it as it is.
    #[error("settings invalid: {0}")]
    Invalid(&'static str),
}

/// A file that parses but fails [`RuntimeSettings::validate`] is refused like
/// an unparseable one, so the daemon keeps its last good settings instead of
/// running with a value the page would never have saved.
pub fn read_settings(path: &std::path::Path) -> Result<RuntimeSettings, SettingsError> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let settings: RuntimeSettings = serde_json::from_str(&text)?;
            settings.validate()?;
            Ok(settings)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(RuntimeSettings::default()),
        Err(error) => Err(error.into()),
    }
}

pub fn write_settings(path: &std::path::Path, settings: &RuntimeSettings) -> std::io::Result<()> {
    crate::persist::replace(path, &serde_json::to_vec_pretty(settings)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// A real deployment's settings.json from before the forecaster (its Plex
    /// host made generic): it must still load after the upgrade, as a dry run,
    /// or the daemon parks on its last good (or default) settings.
    const LIVE: &str = r#"{"interval_s":300,"grace_runs":2,"max_items":10,"max_gib":50,"enforce":true,
        "collection_movie":"Watched Movies Cleanup","collection_season":"Watched Seasons Cleanup","collection_leaving":"Leaving Soon",
        "score_floor":0.75,"score_temperature":1.6,"unwatched_reclaim_enabled":true,"unwatched_reclaim_floor":0.75,
        "unwatched_reclaim_dwell_days":90.0,"plex_url":"plex.media.svc.cluster.local:32400","plex_token":"<set>",
        "capacity_ceiling":0.8,"capacity_release":0.75,"capacity_arm_never_played":true,"keep_tag":"flinch-keep",
        "taste_url":"","taste_model":"","taste_daily_budget":200}"#;

    fn live() -> RuntimeSettings {
        serde_json::from_str(LIVE).unwrap()
    }

    #[test]
    fn the_owners_pre_forecaster_settings_load_as_a_valid_dry_run() {
        let settings = live();
        assert!(settings.validate().is_ok());
        assert!(settings.planner.dry_run, "a dropped enforce: true never hands anything over");
        assert!(settings.unwatched_reclaim_enabled, "surviving choices are kept");
        assert!(RuntimeSettings::default().validate().is_ok());
    }

    #[test]
    fn a_partial_nested_object_defaults_the_rest() {
        let settings: RuntimeSettings =
            serde_json::from_str(r#"{"capacity":{"target_utilization":0.7},"planner":{"dry_run":false}}"#).unwrap();
        assert_eq!(settings.capacity.target_utilization, 0.7);
        assert_eq!(settings.capacity.emergency_utilization, 0.95);
        assert!(!settings.planner.dry_run);
        assert_eq!(settings.planner.grace_period_days, 30);
    }

    #[rstest]
    #[case::interval_below_five_minutes(|s: &mut RuntimeSettings| s.interval_s = 299)]
    #[case::no_grace(|s: &mut RuntimeSettings| s.grace_runs = 0)]
    #[case::grace_above_twenty(|s: &mut RuntimeSettings| s.grace_runs = 21)]
    #[case::target_above_emergency(|s: &mut RuntimeSettings| s.capacity.target_utilization = 0.96)]
    #[case::zero_quantum(|s: &mut RuntimeSettings| s.planner.quantum_mb = 0)]
    #[case::negative_weight(|s: &mut RuntimeSettings| { s.planner.user_weights.insert("ann".into(), -1.0); })]
    fn a_value_the_settings_page_would_refuse_is_invalid(#[case] break_it: fn(&mut RuntimeSettings)) {
        let mut settings = live();
        break_it(&mut settings);
        assert!(matches!(settings.validate(), Err(SettingsError::Invalid(_))));
    }

    #[test]
    fn a_file_that_parses_but_is_out_of_range_is_refused_not_used() {
        let path = std::env::temp_dir().join(format!("flinch-settings-{}-invalid.json", std::process::id()));
        std::fs::write(&path, r#"{"capacity": {"target_utilization": 0.97}}"#).unwrap();
        let read = read_settings(&path);
        std::fs::remove_file(&path).ok();
        assert!(matches!(read, Err(SettingsError::Invalid(_))));
    }
}
