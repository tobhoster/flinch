//! The PCD source: the fixture database replayed into the guide model,
//! fetched from a fake GitHub only after its license is read, and synced into
//! the fake Radarr through the same diff and apply as TRaSH-Guides.

use super::fake::{self, Request};
use super::radarr::{self, State, KEY};
use crate::capacity::App;
use crate::trash::config::{InstanceConfig, ProfileConfig, Source};
use crate::trash::diff::{Action, Kind, Owned};
use crate::trash::guide::{AppGuide, GuideSource};
use crate::trash::pcd::{self, PcdError, LANGUAGE_REJECT};
use crate::trash::{sync_app, AppSync, ArrClient, Selection};
use serde_json::json;
use std::path::PathBuf;

const DATABASE: &str = "Dictionarry-Hub/database";
const SCHEMA: &str = "Dictionarry-Hub/schema";
const COMMIT: &str = "faeeeaea5f1c87de6577222412f7544be7d04899";
const SCHEMA_COMMIT: &str = "e1c2bd73d7003f254ad135eeefbbed7b47f095b1";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/trash/fixtures/pcd")
}

/// Every op of the fixture, schema first, each repo in its op order.
fn ops() -> Vec<(String, String)> {
    let mut all = Vec::new();
    for repo in ["schema", "database"] {
        let mut files: Vec<(u64, String)> = std::fs::read_dir(fixtures().join(repo).join("ops"))
            .expect("an ops dir")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .map(|name| (name.split('.').next().and_then(|n| n.parse().ok()).unwrap_or(u64::MAX), name))
            .collect();
        files.sort();
        for (_, name) in files {
            all.push((name.clone(), std::fs::read_to_string(fixtures().join(repo).join("ops").join(&name)).expect("an op")));
        }
    }
    all
}

fn replayed() -> (AppGuide, AppGuide) {
    pcd::replay(&ops()).expect("the fixture replays")
}

#[test]
fn a_pcd_reads_into_the_guide_model_with_each_apps_own_names_and_values() {
    let (radarr, sonarr) = replayed();
    let profile_id = pcd::id("profile", "1080p Compact");

    let profile = &radarr.profiles[&profile_id];
    let ladder: Vec<(&str, bool, Vec<&str>)> =
        profile.items.iter().map(|i| (i.name.as_str(), i.allowed, i.items.iter().map(String::as_str).collect())).collect();
    assert_eq!(
        ladder,
        [
            ("1080p Compact", true, vec!["WEBDL-1080p", "WEBRip-1080p", "Bluray-1080p"]),
            ("720p Quality", true, vec!["WEBDL-720p", "Bluray-720p"]),
            ("Remux-1080p", false, vec![]),
        ],
        "best first by position"
    );
    assert_eq!((profile.cutoff.as_str(), profile.min_format_score, profile.cutoff_format_score), ("1080p Compact", 20000, 888_888));
    assert_eq!(profile.trash_score_set.as_deref(), Some(profile_id.as_str()), "a PCD scores per profile");
    let scored = |guide: &AppGuide, name: &str| {
        guide.custom_formats.get(&pcd::id("cf", name)).and_then(|f| f.trash_scores.get(&profile_id)).copied()
    };
    assert_eq!(scored(&radarr, "FLUX"), Some(150), "op 10 runs after op 2");
    assert_eq!((scored(&radarr, "x265"), scored(&sonarr, "x265")), (Some(50), Some(75)), "an app's own score wins over 'all'");
    assert_eq!(scored(&radarr, "Language: Must Original"), Some(LANGUAGE_REJECT));

    let web = &radarr.custom_formats[&pcd::id("cf", "1080p WEB-DL")];
    let source = web.specifications.iter().find(|s| s.implementation == "SourceSpecification").expect("a source condition");
    assert_eq!(source.fields["value"], json!(7), "Radarr's WEBDL");
    let sonarr_source = sonarr.custom_formats[&pcd::id("cf", "1080p WEB-DL")]
        .specifications
        .iter()
        .find(|s| s.implementation == "SourceSpecification")
        .map(|s| s.fields["value"].clone());
    assert_eq!(sonarr_source, Some(json!(3)), "Sonarr's Web");
    let language = &radarr.custom_formats[&pcd::id("cf", "Language: Must Original")].specifications[0];
    assert_eq!((language.negate, language.fields["value"].clone()), (true, json!(-2)), "matches a release without the original language");
    assert!(radarr.custom_formats[&pcd::id("cf", "x265")].include_when_renaming);

    assert!(radarr.skipped[&pcd::id("cf", "Freeleech")].contains("indexer_flag"), "{:?}", radarr.skipped);
    assert!(radarr.skipped[&pcd::id("cf", "Season Pack")].contains("no condition for radarr"));
    let pack = &sonarr.custom_formats[&pcd::id("cf", "Season Pack")].specifications[0];
    assert_eq!((pack.implementation.as_str(), pack.fields["value"].clone()), ("ReleaseTypeSpecification", json!(3)));

    assert_eq!(radarr.sizes[0].kind, "default");
    assert_eq!(
        radarr.sizes[0].qualities.iter().map(|q| (q.quality.as_str(), q.max)).collect::<Vec<_>>(),
        [("Bluray-1080p", Some(200.0)), ("WEBDL-1080p", Some(100.0))]
    );
    assert_eq!(sonarr.sizes[0].qualities[0].quality, "Bluray-1080p Remux", "Sonarr's own name for Remux-1080p");
}

#[test]
fn a_pcd_op_cannot_open_a_file() {
    let mut all = ops();
    all.push(("99.attach.sql".to_string(), "ATTACH DATABASE '/tmp/flinch-pcd-escape.db' AS escape;".to_string()));
    assert!(matches!(pcd::replay(&all), Err(PcdError::Sql { op, .. }) if op == "99.attach.sql"));
    assert!(!std::path::Path::new("/tmp/flinch-pcd-escape.db").exists());
}

/// GitHub for both repositories from `fixtures/pcd`; `license` replaces the
/// manifest's.
fn github(license: Option<&'static str>) -> fake::Server {
    fake::serve(move |request: &Request| {
        for (repo, dir, commit) in [(DATABASE, "database", COMMIT), (SCHEMA, "schema", SCHEMA_COMMIT)] {
            if request.path == format!("/repos/{repo}/contents/ops?ref={commit}") {
                let entries: Vec<serde_json::Value> = std::fs::read_dir(fixtures().join(dir).join("ops"))
                    .expect("ops")
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .map(|name| json!({ "name": name, "path": format!("ops/{name}"), "type": "file" }))
                    .collect();
                return (200, serde_json::Value::Array(entries).to_string());
            }
            if let Some(path) = request.path.strip_prefix(&format!("/{repo}/{commit}/")) {
                if path == "pcd.json" {
                    let mut manifest: serde_json::Value =
                        serde_json::from_str(&std::fs::read_to_string(fixtures().join(dir).join(path)).expect("pcd.json")).expect("JSON");
                    manifest["license"] = license.map_or(serde_json::Value::Null, |l| json!(l));
                    return (200, manifest.to_string());
                }
                return std::fs::read_to_string(fixtures().join(dir).join(path)).map_or((404, String::new()), |body| (200, body));
            }
        }
        (404, String::new())
    })
}

fn sources(server: &fake::Server) -> (GuideSource, GuideSource) {
    let at = |repo: &str, commit: &str| GuideSource {
        api: server.base.clone(),
        raw: server.base.clone(),
        repo: repo.to_string(),
        commit: commit.to_string(),
    };
    (at(DATABASE, COMMIT), at(SCHEMA, SCHEMA_COMMIT))
}

#[tokio::test]
async fn the_license_is_read_first_and_an_unlicensed_database_is_not_fetched() {
    let server = github(None);
    let (database, schema) = sources(&server);
    let result = pcd::fetch(&reqwest::Client::new(), &database, &schema, "key".to_string()).await;
    assert!(matches!(result, Err(PcdError::Unlicensed { .. })), "{result:?}");
    assert_eq!(
        server.requests().iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
        [format!("/{DATABASE}/{COMMIT}/pcd.json")],
        "nothing but the manifest"
    );
}

#[tokio::test]
async fn a_licensed_database_arrives_whole_and_is_cached_for_its_pins() {
    let server = github(Some("MIT"));
    let (database, schema) = sources(&server);
    let read = pcd::fetch(&reqwest::Client::new(), &database, &schema, format!("{COMMIT}-{SCHEMA_COMMIT}")).await.expect("the PCD arrives");
    assert_eq!(read.license.as_deref(), Some("MIT"));
    assert_eq!((read.radarr.clone(), read.sonarr.clone()), replayed());

    let dir = std::env::temp_dir().join(format!("flinch-trash-pcd-{}", std::process::id()));
    pcd::write_cache(&dir, &read).expect("cached");
    let cached = pcd::read_cache(&dir, &crate::trash::config::PcdConfig::default());
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(cached.as_ref(), Some(&read), "the default pins are the fixture's");
}

#[tokio::test]
async fn a_pcd_profile_syncs_through_the_same_diff_and_apply() {
    let server = radarr::serve(State::fixtures());
    let http = reqwest::Client::new();
    let client = ArrClient::new(&http, App::Radarr, &server.base, KEY);
    let (guide, _) = replayed();
    let profile = ProfileConfig {
        trash_id: pcd::id("profile", "1080p Compact"),
        name: None,
        reset_unmatched_scores: true,
        upgrade: None,
        min_upgrade_format_score: None,
        qualities: Vec::new(),
        compact: true,
        score_multiplier: None,
    };
    let config = InstanceConfig { source: Source::Pcd, quality_profiles: vec![profile], ..InstanceConfig::default() };
    let sync = |owned| AppSync {
        client: &client,
        config: &config,
        guide: &guide,
        delete_unmanaged: false,
        delete_unused_profiles: false,
        owned,
        usage: None,
        outlook: None,
        keep_profile: None,
    };

    let (preview, _) = sync_app(sync(Owned::default()), None).await;
    assert_eq!(preview.source, Source::Pcd);
    assert!(
        preview.problems.iter().any(|p| p.contains("Freeleech") && p.contains("indexer_flag")),
        "the skipped format says why: {:?}",
        preview.problems
    );
    let created: std::collections::BTreeSet<&str> =
        preview.changes.iter().filter(|c| c.kind == Kind::CustomFormat && c.action == Action::Create).map(|c| c.name.as_str()).collect();
    assert_eq!(
        created,
        ["1080p WEB-DL", "FLUX", "Language: Must Original", "x265"].into(),
        "the formats the profile scores, Freeleech left out"
    );

    let (state, outcome) = sync_app(sync(Owned::default()), Some((Selection::All, false))).await;
    assert!(outcome.expect("an outcome").failed.is_empty());
    assert!(state.changes.is_empty(), "read back as asked: {:?}", state.changes);
    assert!(state.compact_profile_id.is_some());
}
