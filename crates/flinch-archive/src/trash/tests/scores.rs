//! Relative scores (Recyclarr #208) and the language preset, applied to the
//! fake Radarr and read from the profile it was sent.

use super::radarr::{self, State, KEY};
use super::sync::{compact, guide};
use crate::capacity::App;
use crate::trash::config::{CustomFormatConfig, InstanceConfig};
use crate::trash::diff::Owned;
use crate::trash::language::{LanguagePreset, Preferred};
use crate::trash::{sync_app, AppSync, ArrClient, Selection};
use rstest::rstest;
use serde_json::Value;

const REPACK_PROPER: &str = "e7718d7a3ce595f289bfee26adc178f5";

/// Apply everything for `config`; the created profile and what is left pending.
async fn created(config: &InstanceConfig) -> (Value, usize) {
    let server = radarr::serve(State::fixtures());
    let http = reqwest::Client::new();
    let client = ArrClient::new(&http, App::Radarr, &server.base, KEY);
    let guide = guide();
    let sync = AppSync {
        client: &client,
        config,
        guide: &guide,
        delete_unmanaged: false,
        delete_unused_profiles: false,
        owned: Owned::default(),
        usage: None,
        outlook: None,
        keep_profile: None,
    };
    let (state, _) = sync_app(sync, Some((Selection::All, false))).await;
    let body = server
        .requests()
        .into_iter()
        .find(|r| r.method == "POST" && r.path == "/api/v3/qualityprofile")
        .expect("the profile is created")
        .body;
    (serde_json::from_str(&body).expect("a JSON profile"), state.changes.len())
}

fn score(profile: &Value, name: &str) -> Option<i64> {
    profile["formatItems"].as_array()?.iter().find(|item| item["name"] == name)?["score"].as_i64()
}

#[rstest]
#[case::the_guides_scores(None, None, None, 1700, 5)]
#[case::halved_and_rounded(Some(0.5), None, None, 850, 3)]
#[case::moved_by_an_adjustment(None, None, Some(20), 1700, 25)]
#[case::doubled_then_adjusted(Some(2.0), None, Some(-3), 3400, 7)]
#[case::an_absolute_override_is_not_scaled(Some(2.0), Some(-50), None, 3400, -50)]
#[tokio::test]
async fn a_profile_multiplier_scales_the_guide_and_an_adjustment_moves_it(
    #[case] multiplier: Option<f64>,
    #[case] absolute: Option<i32>,
    #[case] adjust: Option<i32>,
    #[case] tier_01: i64,
    #[case] repack: i64,
) {
    let wanted = (absolute.is_some() || adjust.is_some()).then(|| CustomFormatConfig {
        trash_id: REPACK_PROPER.to_string(),
        score: absolute,
        adjust_score: adjust,
        profiles: Vec::new(),
    });
    let mut config = compact(wanted.into_iter().collect());
    config.quality_profiles[0].score_multiplier = multiplier;
    assert_eq!(config.quality_profiles[0].trash_id.len(), 32);

    let (profile, pending) = created(&config).await;

    assert_eq!(pending, 0, "read back as asked");
    assert_eq!(score(&profile, "WEB Tier 01"), Some(tier_01));
    assert_eq!(score(&profile, "Repack/Proper"), Some(repack));
}

#[rstest]
#[case::english_only(false, -10_000, 0)]
#[case::english_with_fallback(true, -1000, -1000)]
#[tokio::test]
async fn the_language_preset_scores_trash_language_formats_and_keeps_a_fallback_reachable(
    #[case] fallback: bool,
    #[case] not_english: i64,
    #[case] minimum: i64,
) {
    let mut config = compact(Vec::new());
    config.language = Some(LanguagePreset { prefer: Preferred::English, fallback, fallback_penalty: 1000 });

    let (profile, pending) = created(&config).await;

    assert_eq!(pending, 0, "read back as asked");
    assert_eq!(score(&profile, "Language: Not English"), Some(not_english));
    assert_eq!(profile["minFormatScore"].as_i64(), Some(minimum), "the guide's minimum is 0; a fallback lowers it by the penalty");
    assert_eq!(profile["language"]["name"], "Any", "a profile language of Original would refuse the fallback");
    let tier_01 = score(&profile, "WEB Tier 01").unwrap_or_default();
    assert!(tier_01 + not_english >= minimum || !fallback, "a WEB Tier 01 release in another language is still taken with a fallback");
    assert!(not_english < minimum || fallback, "without a fallback another language is refused");
}
