//! Reading the guide from a fake GitHub serving the checked-in guide files.

use super::fake::{self, Request};
use crate::trash::guide::{self, GuideError, GuideSource};
use crate::trash::GUIDE_COMMIT;
use std::path::PathBuf;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/trash/fixtures/guide")
}

/// The contents API for a directory, and raw files, from `fixtures/guide`;
/// `broken` answers 500 instead.
fn github(broken: Option<&'static str>) -> fake::Server {
    fake::serve(move |request: &Request| {
        let contents = "/repos/TRaSH-Guides/Guides/contents/";
        let raw = format!("/TRaSH-Guides/Guides/{GUIDE_COMMIT}/");
        if let Some(rest) = request.path.strip_prefix(contents) {
            let Some(dir) = rest.strip_suffix(&format!("?ref={GUIDE_COMMIT}")) else { return (404, String::new()) };
            let mut entries: Vec<serde_json::Value> = std::fs::read_dir(fixtures().join(dir))
                .map(|entries| entries.flatten().collect::<Vec<_>>())
                .unwrap_or_default()
                .into_iter()
                .map(|entry| {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    serde_json::json!({ "name": name, "path": format!("{dir}/{name}"), "type": "file" })
                })
                .collect();
            entries.push(serde_json::json!({ "name": "nested", "path": format!("{dir}/nested"), "type": "dir" }));
            return (200, serde_json::Value::Array(entries).to_string());
        }
        match request.path.strip_prefix(&raw) {
            Some(path) if broken.is_some_and(|b| path.ends_with(b)) => (500, String::new()),
            Some(path) => std::fs::read_to_string(fixtures().join(path)).map_or((404, String::new()), |body| (200, body)),
            None => (404, String::new()),
        }
    })
}

fn source(server: &fake::Server) -> GuideSource {
    GuideSource {
        api: server.base.clone(),
        raw: server.base.clone(),
        repo: "TRaSH-Guides/Guides".to_string(),
        commit: GUIDE_COMMIT.to_string(),
    }
}

#[tokio::test]
async fn the_guide_at_the_pinned_commit_is_read_whole_and_cached_for_that_commit_only() {
    let server = github(None);
    let http = reqwest::Client::new();
    let read = guide::fetch(&http, &source(&server)).await.expect("the guide arrives");

    let web = &read.radarr.profiles["e8c5acb741363a0dbda67d3978f4912f"];
    assert_eq!(web.name, "WEB 1080p");
    assert_eq!(web.items[0].items, ["WEBRip-1080p", "WEBDL-1080p"], "best first, as the guide lists it");
    assert_eq!(read.radarr.custom_formats.len(), 7, "the six formats and the language format");
    assert_eq!(read.radarr.custom_formats["c20f169ef63c5f40c2def54abaf4438e"].trash_scores["default"], 1700);
    assert_eq!(read.sonarr.profiles["72dae194fc92bf828f32cde7744e51a1"].name, "WEB-1080p");
    assert_eq!(read.radarr.sizes[0].kind, "movie");
    assert!(
        server.requests().iter().all(|r| r.header("user-agent") == Some("flinch-trash-sync")),
        "GitHub refuses a client that does not name itself"
    );

    let dir = std::env::temp_dir().join(format!("flinch-trash-guide-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a cache dir");
    let older = guide::Guide { commit: "0".repeat(40), ..guide::Guide::default() };
    guide::write_cache(&dir, &older).expect("an older pin cached");
    guide::write_cache(&dir, &read).expect("cached");
    let cached = guide::read_cache(&dir, GUIDE_COMMIT);
    let stale = guide::read_cache(&dir, &"0".repeat(40));
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(cached.as_ref(), Some(&read));
    assert_eq!(stale, None, "a moved pin drops the old copy");
}

#[tokio::test]
async fn one_file_that_does_not_arrive_fails_the_whole_guide() {
    let server = github(Some("repack3.json"));
    let result = guide::fetch(&reqwest::Client::new(), &source(&server)).await;
    assert!(
        matches!(&result, Err(GuideError::Http { what, status: 500 }) if what.ends_with("repack3.json")),
        "a partial guide would preview a profile without some of its formats: {result:?}"
    );
}
