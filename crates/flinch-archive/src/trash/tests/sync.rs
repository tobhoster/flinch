//! Preview and apply against a fake Radarr that keeps what it is sent.

use super::radarr::{self, State, KEY};
use crate::capacity::App;
use crate::trash::config::{CustomFormatConfig, InstanceConfig, ProfileConfig, QualityDefinitionConfig};
use crate::trash::diff::{Action, Kind, Owned};
use crate::trash::guide::AppGuide;
use crate::trash::{sync_app, AppState, AppSync, ArrClient, Selection};
use rstest::rstest;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::PathBuf;

const WEB_1080P: &str = "e8c5acb741363a0dbda67d3978f4912f";
const REPACK_PROPER: &str = "e7718d7a3ce595f289bfee26adc178f5";

/// The checked-in Radarr guide files, parsed as a fetch would.
pub(super) fn guide() -> AppGuide {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/trash/fixtures/guide/docs/json/radarr");
    let files = |sub: &str| -> Vec<Vec<u8>> {
        std::fs::read_dir(dir.join(sub)).expect("a guide dir").flatten().map(|e| std::fs::read(e.path()).expect("a guide file")).collect()
    };
    let mut guide = AppGuide::default();
    for bytes in files("cf") {
        let format: crate::trash::guide::GuideCustomFormat = serde_json::from_slice(&bytes).expect("a guide format");
        guide.custom_formats.insert(format.trash_id.clone(), format);
    }
    for bytes in files("quality-profiles") {
        let profile: crate::trash::guide::GuideProfile = serde_json::from_slice(&bytes).expect("a guide profile");
        guide.profiles.insert(profile.trash_id.clone(), profile);
    }
    guide.sizes = files("quality-size").iter().map(|bytes| serde_json::from_slice(bytes).expect("a size table")).collect();
    guide
}

/// The compact profile with the preset's upgrade floor, and the movie sizes
/// tie-broken toward smaller files.
pub(super) fn compact(custom_formats: Vec<CustomFormatConfig>) -> InstanceConfig {
    InstanceConfig {
        quality_profiles: vec![ProfileConfig {
            trash_id: WEB_1080P.to_string(),
            name: None,
            reset_unmatched_scores: true,
            upgrade: None,
            min_upgrade_format_score: Some(500),
            qualities: Vec::new(),
            compact: true,
            score_multiplier: None,
        }],
        custom_formats,
        quality_definition: Some(QualityDefinitionConfig { kind: "movie".to_string(), preferred_ratio: Some(0.2) }),
        ..InstanceConfig::default()
    }
}

async fn run(
    server: &super::fake::Server,
    config: &InstanceConfig,
    owned: Owned,
    delete_unmanaged: bool,
    apply: Option<(Selection<'_>, bool)>,
) -> (AppState, Option<crate::trash::AppOutcome>) {
    let http = reqwest::Client::new();
    let client = ArrClient::new(&http, App::Radarr, &server.base, KEY);
    let guide = guide();
    let sync = AppSync {
        client: &client,
        config,
        guide: &guide,
        delete_unmanaged,
        delete_unused_profiles: false,
        owned,
        usage: None,
        outlook: None,
        keep_profile: None,
    };
    sync_app(sync, apply).await
}

fn writes(server: &super::fake::Server) -> Vec<String> {
    server.requests().into_iter().filter(|r| r.method != "GET").map(|r| format!("{} {}", r.method, r.path)).collect()
}

#[rstest]
#[case::operators_formats_kept(false, None)]
#[case::operators_formats_offered_for_deletion_when_opted_in(true, Some("radarr:cf-delete:1"))]
#[tokio::test]
async fn the_preview_writes_nothing_adopts_by_name_and_offers_only_what_it_may_delete(
    #[case] delete_unmanaged: bool,
    #[case] delete: Option<&str>,
) {
    let server = radarr::serve(State::fixtures());
    let (state, outcome) = run(&server, &compact(Vec::new()), Owned::default(), delete_unmanaged, None).await;

    assert!(outcome.is_none() && writes(&server).is_empty(), "a preview sends no write");
    let creates: BTreeSet<&str> =
        state.changes.iter().filter(|c| c.kind == Kind::CustomFormat && c.action == Action::Create).map(|c| c.name.as_str()).collect();
    assert_eq!(creates, BTreeSet::from(["Repack2", "Repack3", "WEB Tier 01", "WEB Tier 02", "WEB Tier 03"]));
    let adopted = state.changes.iter().find(|c| c.id == format!("radarr:cf:{REPACK_PROPER}")).expect("Repack/Proper adopted by name");
    assert_eq!((adopted.action, adopted.arr_id), (Action::Update, Some(2)), "the operator's same-named format is updated, not duplicated");
    let profile = state.changes.iter().find(|c| c.kind == Kind::QualityProfile).expect("the compact profile");
    assert_eq!(profile.action, Action::Create);
    assert_eq!(profile.requires.len(), 5, "a new profile waits for the formats it scores");
    let sizes = state.changes.iter().find(|c| c.kind == Kind::QualityDefinition).expect("the size table");
    let webdl = sizes.fields.iter().find(|f| f.field == "WEBDL-1080p").expect("WEBDL-1080p resized");
    assert_eq!(
        webdl.to.as_deref(),
        Some("min 12.5 · preferred 409.8 · max 2000"),
        "Recyclarr's preferred_ratio: 12.5 + (1999 − 12.5) × 0.2"
    );
    let deletes: Vec<&str> = state.changes.iter().filter(|c| c.action == Action::Delete).map(|c| c.id.as_str()).collect();
    assert_eq!(deletes, delete.into_iter().collect::<Vec<_>>());
    assert_eq!(state.compact_profile_id, None, "no compact profile exists yet");
}

fn created_profile(server: &super::fake::Server) -> Value {
    let body = server
        .requests()
        .into_iter()
        .find(|r| r.method == "POST" && r.path == "/api/v3/qualityprofile")
        .expect("the profile is created")
        .body;
    serde_json::from_str(&body).expect("a JSON profile")
}

#[tokio::test]
async fn applying_everything_leaves_nothing_pending_when_read_back() {
    let server = radarr::serve(State::fixtures());
    let (state, outcome) = run(&server, &compact(Vec::new()), Owned::default(), false, Some((Selection::All, false))).await;
    let outcome = outcome.expect("an apply outcome");

    assert!(outcome.failed.is_empty() && outcome.unverified.is_empty(), "{outcome:?}");
    assert_eq!(outcome.applied.len(), 8, "5 formats created, 1 updated, the profile, the sizes");
    assert!(state.changes.is_empty(), "the read-back finds nothing left to change: {:?}", state.changes);
    assert_eq!(state.owned.custom_formats.len(), 5, "only what FLINCH created is FLINCH's");
    assert!(state.compact_profile_id.is_some() && state.compact_profile_id == state.owned.profiles.get(WEB_1080P).copied());

    let profile = created_profile(&server);
    let items = profile["items"].as_array().expect("items");
    let top = items.last().expect("a best rung");
    assert_eq!(
        (top["name"].as_str(), top["id"].as_i64(), top["allowed"].as_bool()),
        (Some("WEB 1080p"), Some(1002), Some(true)),
        "best last, and Radarr's own group id reused"
    );
    assert_eq!(profile["cutoff"], 1002);
    assert_eq!(profile["language"]["name"], "Original");
    let score =
        |name: &str| profile["formatItems"].as_array().and_then(|f| f.iter().find(|i| i["name"] == name)).map(|i| i["score"].clone());
    assert_eq!(score("WEB Tier 01"), Some(1700.into()));
    assert_eq!(score("My Own"), Some(0.into()), "an unasked-for format is reset in a FLINCH profile");
    let operators = server.requests().into_iter().filter(|r| r.path == "/api/v3/qualityprofile/1").count();
    assert_eq!(operators, 0, "the operator's own profile is never written");
}

#[tokio::test]
async fn a_score_override_wins_over_the_guide() {
    let server = radarr::serve(State::fixtures());
    let config = compact(vec![CustomFormatConfig {
        trash_id: REPACK_PROPER.to_string(),
        score: Some(-50),
        adjust_score: None,
        profiles: Vec::new(),
    }]);
    let (state, _) = run(&server, &config, Owned::default(), false, Some((Selection::All, false))).await;

    assert!(state.changes.is_empty());
    let repack = created_profile(&server)["formatItems"].as_array().and_then(|f| f.iter().find(|i| i["format"] == 2).cloned());
    assert_eq!(repack.map(|item| item["score"].clone()), Some((-50).into()));
}

#[tokio::test]
async fn a_dry_run_sends_nothing_and_records_every_request_it_would_send() {
    let server = radarr::serve(State::fixtures());
    let (state, outcome) = run(&server, &compact(Vec::new()), Owned::default(), false, Some((Selection::All, true))).await;
    let outcome = outcome.expect("an apply outcome");

    assert!(writes(&server).is_empty(), "{:?}", writes(&server));
    assert_eq!(outcome.printed.len(), 8);
    assert!(outcome.printed.iter().any(|line| line.starts_with("would POST /api/v3/qualityprofile")));
    assert_eq!(state.owned, Owned::default(), "nothing was created, so nothing is FLINCH's");
    assert_eq!(state.changes.len(), 8, "every change still pending");
}

#[tokio::test]
async fn only_the_selected_change_is_written() {
    let server = radarr::serve(State::fixtures());
    let selected = BTreeSet::from(["radarr:sizes:movie".to_string()]);
    let (state, outcome) = run(&server, &compact(Vec::new()), Owned::default(), false, Some((Selection::Only(&selected), false))).await;

    assert_eq!(writes(&server), ["PUT /api/v3/qualitydefinition/update"]);
    assert_eq!(outcome.expect("an outcome").applied, ["radarr:sizes:movie"]);
    assert_eq!(state.changes.len(), 7, "the rest waits for the operator");
}

#[tokio::test]
async fn formats_flinch_created_are_offered_for_deletion_once_unused_and_the_operators_never() {
    let server = radarr::serve(State::fixtures());
    let (synced, _) = run(&server, &compact(Vec::new()), Owned::default(), false, Some((Selection::All, false))).await;
    let nothing = InstanceConfig::default();
    let (state, _) = run(&server, &nothing, synced.owned.clone(), false, None).await;

    let deleted: BTreeSet<u32> = state.changes.iter().filter(|c| c.action == Action::Delete).filter_map(|c| c.arr_id).collect();
    assert_eq!(
        deleted,
        synced.owned.custom_formats.values().copied().collect(),
        "its own five, not My Own (1) nor the adopted Repack/Proper (2)"
    );
}

#[tokio::test]
async fn an_unreadable_app_keeps_what_flinch_knew_it_created() {
    let server = radarr::serve(State { down: true, ..State::fixtures() });
    let owned = Owned { custom_formats: [(REPACK_PROPER.to_string(), 7)].into(), profiles: Default::default() };
    let selected = BTreeSet::from(["radarr:sizes:movie".to_string()]);
    let (state, outcome) = run(&server, &compact(Vec::new()), owned.clone(), false, Some((Selection::Only(&selected), false))).await;

    assert!(state.error.as_deref().is_some_and(|e| e.contains("HTTP 500") && e.contains("database is locked")), "{:?}", state.error);
    assert_eq!(state.owned, owned);
    assert_eq!(outcome.expect("an outcome").failed.len(), 1, "the selected change is reported, not dropped");
}

/// The Recyclarr companion config's intent, now the Radarr preset: upgrades
/// stop at WEB 2160p and a reachable score, Remux-1080p stays in the ladder
/// but disabled, and WEB 1080p is the compact profile.
#[tokio::test]
async fn the_radarr_preset_stops_upgrades_at_web_2160p_and_skips_the_remux_stop_over() {
    let server = radarr::serve(State::fixtures());
    let (state, outcome) = run(&server, &crate::trash::presets::radarr(), Owned::default(), false, Some((Selection::All, false))).await;

    assert!(outcome.expect("an outcome").failed.is_empty());
    assert!(state.changes.is_empty(), "{:?}", state.changes);
    let body = server
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST" && r.path == "/api/v3/qualityprofile")
        .map(|r| serde_json::from_str::<Value>(&r.body).expect("a JSON profile"))
        .find(|p| p["name"] == "Remux 2160p (Combined)")
        .expect("the original-quality profile is created");
    let ladder: Vec<(String, bool)> = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .rev()
        .map(|item| (item["name"].as_str().or(item["quality"]["name"].as_str()).unwrap_or_default().to_string(), item["allowed"] == true))
        .collect();
    assert_eq!(
        ladder[..4],
        [
            ("WEB 2160p".to_string(), true),
            ("Remux-1080p".to_string(), false),
            ("Bluray-1080p".to_string(), true),
            ("WEB 1080p".to_string(), true)
        ],
        "best first"
    );
    assert!(ladder.contains(&("Remux-2160p".to_string(), false)), "a quality the ladder leaves out stays, disabled");
    assert_eq!(
        (body["cutoff"].as_i64(), body["cutoffFormatScore"].as_i64(), body["minUpgradeFormatScore"].as_i64()),
        (Some(1003), Some(5000), Some(500))
    );
    assert!(state.compact_profile_id.is_some() && state.compact_profile_id == state.owned.profiles.get(WEB_1080P).copied());
}
