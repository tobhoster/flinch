//! FLINCH's built-in presets: what FLINCH's Recyclarr companion config used
//! to ask of Recyclarr, now synced by FLINCH itself. Each buys disk:
//!
//! 1. A compact profile per app, for the quality advice to downgrade into
//!    ([`super::compact_profile_id`]).
//! 2. No endless upgrades: `until_score` the guide sets at 10000 is beyond
//!    any WEB release (a strong WEB 2160p release scores about 5.2k in
//!    Radarr, 2.3k in Sonarr), and a `min_upgrade_format_score` of 1 lets a
//!    +5 repack re-download a 20 GB file.
//! 3. No Remux-1080p stop-over in the Radarr ladder: a movie without a 4K WEB
//!    release would grab a 20–40 GB remux, then upgrade to WEB 2160p anyway.
//!    The quality stays in the profile, disabled, so the order stays safe.
//! 4. `preferred_ratio` 0.2: when every other comparison ties, the smaller
//!    release wins. It moves only the last tie-breaker, so it rejects nothing.

use super::config::{InstanceConfig, ProfileConfig, QualityConfig, QualityDefinitionConfig, UpgradeConfig};

fn profile(trash_id: &str, upgrade: Option<(&str, i32)>, min_upgrade: i32, compact: bool) -> ProfileConfig {
    ProfileConfig {
        trash_id: trash_id.to_string(),
        name: None,
        reset_unmatched_scores: true,
        upgrade: upgrade.map(|(until_quality, until_score)| UpgradeConfig {
            allowed: true,
            until_quality: Some(until_quality.to_string()),
            until_score: Some(until_score),
        }),
        min_upgrade_format_score: Some(min_upgrade),
        qualities: Vec::new(),
        compact,
        score_multiplier: None,
    }
}

fn rung(name: &str, qualities: &[&str], enabled: bool) -> QualityConfig {
    QualityConfig { name: name.to_string(), qualities: qualities.iter().map(|q| q.to_string()).collect(), enabled }
}

/// Radarr: Remux 2160p (Combined) capped at WEB 2160p, and WEB 1080p as the
/// compact profile.
pub fn radarr() -> InstanceConfig {
    let mut original = profile("d1d310673359205736b4b84acd5ea8c8", Some(("WEB 2160p", 5000)), 500, false);
    original.qualities = vec![
        rung("WEB 2160p", &["WEBDL-2160p", "WEBRip-2160p"], true),
        rung("Remux-1080p", &[], false),
        rung("Bluray-1080p", &[], true),
        rung("WEB 1080p", &["WEBDL-1080p", "WEBRip-1080p"], true),
    ];
    InstanceConfig {
        quality_profiles: vec![original, profile("e8c5acb741363a0dbda67d3978f4912f", None, 500, true)],
        custom_formats: Vec::new(),
        quality_definition: Some(QualityDefinitionConfig { kind: "movie".to_string(), preferred_ratio: Some(0.2) }),
        ..InstanceConfig::default()
    }
}

/// Sonarr: WEB-2160p (Combined), and WEB-1080p as the compact profile, both
/// with a reachable upgrade ceiling.
pub fn sonarr() -> InstanceConfig {
    InstanceConfig {
        quality_profiles: vec![
            profile("c4cadd6b35b95f62c3d47a408e53e2f7", Some(("WEB 2160p", 2000)), 250, false),
            profile("72dae194fc92bf828f32cde7744e51a1", Some(("WEB 1080p", 2000)), 250, true),
        ],
        custom_formats: Vec::new(),
        quality_definition: Some(QualityDefinitionConfig { kind: "series".to_string(), preferred_ratio: Some(0.2) }),
        ..InstanceConfig::default()
    }
}
