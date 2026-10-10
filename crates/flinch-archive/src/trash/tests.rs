//! Behaviour of the TRaSH sync at its seams: a fake GitHub serving guide
//! files, and a fake Radarr that keeps what it is sent and refuses what Radarr
//! refuses. `fixtures/guide` holds verbatim TRaSH-Guides files at
//! [`super::GUIDE_COMMIT`]; `fixtures/radarr` is shaped by Radarr's OpenAPI
//! spec and its default quality list. `fixtures/pcd` is a cut-down Profilarr
//! Compliant Database: the schema's own ops and database ops written in the
//! Dictionarry database's style.

mod fake;
mod guide;
mod impact;
mod pcd;
mod radarr;
mod scores;
mod sync;

use super::config::{CustomFormatConfig, Source, TrashConfig};
use super::language::{LanguagePreset, Preferred};
use rstest::rstest;

#[rstest]
#[case::schedule_of_zero(|c: &mut TrashConfig| c.schedule_hours = 0)]
#[case::short_commit(|c: &mut TrashConfig| c.guide_commit = "d6a23d61".to_string())]
#[case::uppercase_trash_id(|c: &mut TrashConfig| c.instances.radarr.quality_profiles[0].trash_id.make_ascii_uppercase())]
#[case::two_compact_profiles(|c: &mut TrashConfig| c.instances.sonarr.quality_profiles.iter_mut().for_each(|p| p.compact = true))]
#[case::upgrade_floor_of_zero(|c: &mut TrashConfig| c.instances.radarr.quality_profiles[0].min_upgrade_format_score = Some(0))]
#[case::ratio_above_one(|c: &mut TrashConfig| { c.instances.sonarr.quality_definition.iter_mut().for_each(|d| d.preferred_ratio = Some(1.5)); })]
#[case::ratio_not_a_number(|c: &mut TrashConfig| { c.instances.sonarr.quality_definition.iter_mut().for_each(|d| d.preferred_ratio = Some(f64::NAN)); })]
#[case::format_for_a_profile_not_synced(|c: &mut TrashConfig| c.instances.radarr.custom_formats.push(CustomFormatConfig {
    trash_id: "e7718d7a3ce595f289bfee26adc178f5".to_string(),
    score: Some(0),
    adjust_score: None,
    profiles: vec!["72dae194fc92bf828f32cde7744e51a1".to_string()],
}))]
#[case::score_and_adjustment_together(|c: &mut TrashConfig| c.instances.radarr.custom_formats.push(CustomFormatConfig {
    trash_id: "e7718d7a3ce595f289bfee26adc178f5".to_string(),
    score: Some(0),
    adjust_score: Some(10),
    profiles: Vec::new(),
}))]
#[case::negative_multiplier(|c: &mut TrashConfig| c.instances.radarr.quality_profiles[0].score_multiplier = Some(-1.0))]
#[case::language_preset_on_a_pcd(|c: &mut TrashConfig| {
    c.instances.radarr.source = Source::Pcd;
    c.instances.radarr.language = Some(LanguagePreset { prefer: Preferred::English, fallback: false, fallback_penalty: 1000 });
})]
#[case::fallback_penalty_at_the_reject_score(|c: &mut TrashConfig| {
    c.instances.sonarr.language = Some(LanguagePreset { prefer: Preferred::Original, fallback: true, fallback_penalty: 10_000 });
})]
#[case::pcd_commit_short(|c: &mut TrashConfig| c.pcd.commit = "faeeeaea".to_string())]
#[case::pcd_repository_escaping(|c: &mut TrashConfig| c.pcd.repository = "../evil".to_string())]
#[case::ladder_with_nothing_enabled(|c: &mut TrashConfig| c.instances.radarr.quality_profiles[0].qualities.iter_mut().for_each(|q| q.enabled = false))]
fn a_value_the_page_would_refuse_is_invalid(#[case] break_it: fn(&mut TrashConfig)) {
    let mut config = TrashConfig::default();
    assert_eq!(config.validate(), Ok(()), "the presets themselves are valid");
    break_it(&mut config);
    assert!(config.validate().is_err());
}
