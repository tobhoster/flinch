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

    /// The bounds the Settings page enforces, held here too so a hand-edited
    /// file or a scripted PUT cannot hand the daemon a value the page would
    /// refuse. `NaN` fails every range check below, so non-finite floats are
    /// refused by the same comparisons.
    pub fn validate(&self) -> Result<(), SettingsError> {
        let within = |value: f32, low: f32, high: f32| value.is_finite() && (low..=high).contains(&value);
        if !within(self.capacity_ceiling, 0.01, 1.0) {
            return Err(SettingsError::Invalid("the storage ceiling (capacity_ceiling) must be between 1% and 100%"));
        }
        if !within(self.capacity_release, 0.01, 0.99) || self.capacity_release > self.capacity_ceiling {
            return Err(SettingsError::Invalid(
                "the release mark (capacity_release) must be between 1% and 99% and not above the ceiling",
            ));
        }
        if self.interval_s < 300 {
            return Err(SettingsError::Invalid("the scan interval (interval_s) must be at least 300 seconds"));
        }
        if !(1..=20).contains(&self.grace_runs) {
            return Err(SettingsError::Invalid("grace runs (grace_runs) must be between 1 and 20"));
        }
        if !within(self.score_floor, 0.0, 1.0) {
            return Err(SettingsError::Invalid("the score floor (score_floor) must be between 0 and 1"));
        }
        if !within(self.unwatched_reclaim_floor, 0.0, 1.0) {
            return Err(SettingsError::Invalid("the never-played floor (unwatched_reclaim_floor) must be between 0 and 1"));
        }
        if !within(self.score_temperature, 0.1, f32::MAX) {
            return Err(SettingsError::Invalid("the temperature (score_temperature) must be at least 0.1"));
        }
        if !within(self.unwatched_reclaim_dwell_days, 0.0, f32::MAX) {
            return Err(SettingsError::Invalid("days on disk (unwatched_reclaim_dwell_days) must be 0 or more"));
        }
        Ok(())
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

    /// A real deployment's settings.json (its Plex host made generic): a new
    /// rule that refuses it would park the daemon on its last good (or
    /// default) settings after an upgrade.
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
    fn the_owners_live_settings_and_the_defaults_are_valid() {
        assert!(live().validate().is_ok());
        assert!(RuntimeSettings::default().validate().is_ok());
    }

    #[rstest]
    #[case::ceiling_zero(|s: &mut RuntimeSettings| s.capacity_ceiling = 0.0)]
    #[case::ceiling_above_one(|s: &mut RuntimeSettings| s.capacity_ceiling = 1.01)]
    #[case::ceiling_nan(|s: &mut RuntimeSettings| s.capacity_ceiling = f32::NAN)]
    #[case::release_zero(|s: &mut RuntimeSettings| s.capacity_release = 0.0)]
    #[case::release_full(|s: &mut RuntimeSettings| { s.capacity_ceiling = 1.0; s.capacity_release = 1.0 })]
    #[case::release_above_ceiling(|s: &mut RuntimeSettings| s.capacity_release = 0.85)]
    #[case::release_infinite(|s: &mut RuntimeSettings| s.capacity_release = f32::INFINITY)]
    #[case::interval_below_five_minutes(|s: &mut RuntimeSettings| s.interval_s = 299)]
    #[case::no_grace(|s: &mut RuntimeSettings| s.grace_runs = 0)]
    #[case::grace_above_twenty(|s: &mut RuntimeSettings| s.grace_runs = 21)]
    #[case::score_floor_negative(|s: &mut RuntimeSettings| s.score_floor = -0.01)]
    #[case::score_floor_above_one(|s: &mut RuntimeSettings| s.score_floor = 1.01)]
    #[case::never_played_floor_above_one(|s: &mut RuntimeSettings| s.unwatched_reclaim_floor = 1.5)]
    #[case::never_played_floor_nan(|s: &mut RuntimeSettings| s.unwatched_reclaim_floor = f32::NAN)]
    #[case::temperature_too_low(|s: &mut RuntimeSettings| s.score_temperature = 0.09)]
    #[case::temperature_infinite(|s: &mut RuntimeSettings| s.score_temperature = f32::INFINITY)]
    #[case::dwell_negative(|s: &mut RuntimeSettings| s.unwatched_reclaim_dwell_days = -1.0)]
    #[case::dwell_infinite(|s: &mut RuntimeSettings| s.unwatched_reclaim_dwell_days = f32::INFINITY)]
    fn a_value_the_settings_page_would_refuse_is_invalid(#[case] break_it: fn(&mut RuntimeSettings)) {
        let mut settings = live();
        break_it(&mut settings);
        assert!(matches!(settings.validate(), Err(SettingsError::Invalid(_))));
    }

    #[rstest]
    #[case::lowest(|s: &mut RuntimeSettings| { s.capacity_ceiling = 0.01; s.capacity_release = 0.01; s.interval_s = 300; s.grace_runs = 1 })]
    #[case::highest(|s: &mut RuntimeSettings| { s.capacity_ceiling = 1.0; s.capacity_release = 0.99; s.grace_runs = 20 })]
    #[case::release_at_the_ceiling(|s: &mut RuntimeSettings| s.capacity_release = s.capacity_ceiling)]
    #[case::floors_and_dwell_at_their_edges(|s: &mut RuntimeSettings| {
        s.score_floor = 1.0; s.unwatched_reclaim_floor = 0.0; s.score_temperature = 0.1; s.unwatched_reclaim_dwell_days = 0.0
    })]
    fn the_edges_the_settings_page_allows_are_valid(#[case] edge: fn(&mut RuntimeSettings)) {
        let mut settings = live();
        edge(&mut settings);
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn a_file_that_parses_but_is_out_of_range_is_refused_not_used() {
        let path = std::env::temp_dir().join(format!("flinch-settings-{}-invalid.json", std::process::id()));
        std::fs::write(&path, r#"{"capacity_ceiling": 0.7, "capacity_release": 0.9}"#).unwrap();
        let read = read_settings(&path);
        std::fs::remove_file(&path).ok();
        assert!(matches!(read, Err(SettingsError::Invalid(_))));
    }
}
