//! The native executor's clients against scripted fakes over a real socket:
//! each delete lands on the documented endpoint, is read back, undoes itself
//! when it fails halfway, and a dry run sends reads only.

use super::radarr::Radarr;
use super::recheck::{judge, PlexWatch, Recheck};
use super::seerr::{Cleared, Seerr};
use super::sonarr::Sonarr;
use super::{DeleteMode, Evicted, ExecutorError, RestoreTarget};
use rstest::rstest;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::JoinHandle;

/// One request as the fake saw it: `METHOD /path?query` and the body.
type Seen = (String, String);

fn read_request(stream: &mut std::net::TcpStream) -> (String, String) {
    let mut request = Vec::new();
    let mut buffer = [0u8; 8192];
    let head_end = loop {
        if let Some(at) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            break at + 4;
        }
        let read = stream.read(&mut buffer).expect("the request arrives");
        if read == 0 {
            break request.len();
        }
        request.extend_from_slice(&buffer[..read]);
    };
    let head = String::from_utf8_lossy(&request[..head_end]).to_string();
    let length = head
        .lines()
        .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|n| n.trim().parse::<usize>().unwrap_or(0)))
        .unwrap_or(0);
    while request.len() < head_end + length {
        let read = stream.read(&mut buffer).expect("the body arrives");
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
    }
    (head, String::from_utf8_lossy(&request[head_end..]).to_string())
}

/// Serves `script` in order, one connection each, checking the API key.
fn fake(script: Vec<(u16, &'static str)>) -> (String, JoinHandle<Vec<Seen>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the fake app");
    let base = format!("http://{}", listener.local_addr().expect("fake address"));
    let handle = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for (status, body) in script {
            let (mut stream, _) = listener.accept().expect("the client connects");
            let (head, sent) = read_request(&mut stream);
            assert!(head.to_ascii_lowercase().contains("x-api-key: secret"), "key header missing: {head}");
            let line = head.lines().next().unwrap_or_default().trim_end_matches(" HTTP/1.1").to_string();
            seen.push((line, sent));
            write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .expect("the answer is sent");
        }
        seen
    });
    (base, handle)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().expect("client")
}

const MOVIE: &str =
    r#"{"id":7,"monitored":true,"hasFile":true,"movieFile":{"id":5},"tmdbId":603,"qualityProfileId":4,"rootFolderPath":"/movies"}"#;
const MOVIE_GONE_FILE: &str = r#"{"id":7,"monitored":false,"hasFile":false,"tmdbId":603}"#;

#[tokio::test]
async fn a_movie_file_is_unmonitored_then_deleted_and_read_back() {
    let (base, server) = fake(vec![(200, MOVIE), (202, "[]"), (200, ""), (200, MOVIE_GONE_FILE)]);
    let http = client();
    let evicted = Radarr::new(&http, &base, "secret", false).evict(7, DeleteMode::FileAndUnmonitor, false).await.expect("deleted");
    let target = RestoreTarget::Radarr {
        radarr_id: 7,
        tmdb_id: Some(603),
        mode: DeleteMode::FileAndUnmonitor,
        quality_profile_id: Some(4),
        root_folder_path: Some("/movies".into()),
    };
    assert_eq!(evicted, Evicted::Done(target));
    let seen = server.join().expect("fake");
    let lines: Vec<&str> = seen.iter().map(|(line, _)| line.as_str()).collect();
    assert_eq!(lines, ["GET /api/v3/movie/7", "PUT /api/v3/movie/editor", "DELETE /api/v3/moviefile/5", "GET /api/v3/movie/7"]);
    assert_eq!(seen[1].1, r#"{"monitored":false,"movieIds":[7]}"#);
}

#[tokio::test]
async fn a_failed_file_delete_monitors_the_movie_again_and_reports_nothing_done() {
    let (base, server) = fake(vec![(200, MOVIE), (202, "[]"), (500, "database is locked"), (202, "[]")]);
    let http = client();
    let result = Radarr::new(&http, &base, "secret", false).evict(7, DeleteMode::FileAndUnmonitor, false).await;
    assert!(matches!(&result, Err(ExecutorError::Http { status: 500, detail, .. }) if detail == "database is locked"), "{result:?}");
    let seen = server.join().expect("fake");
    assert_eq!(seen[3], ("PUT /api/v3/movie/editor".to_string(), r#"{"monitored":true,"movieIds":[7]}"#.to_string()));
}

#[tokio::test]
async fn a_delete_the_read_back_does_not_show_is_not_applied() {
    let (base, server) = fake(vec![(200, MOVIE), (202, "[]"), (200, ""), (200, MOVIE)]);
    let http = client();
    let result = Radarr::new(&http, &base, "secret", false).evict(7, DeleteMode::FileAndUnmonitor, false).await;
    server.join().expect("fake");
    assert!(matches!(result, Err(ExecutorError::NotApplied { .. })), "{result:?}");
}

#[tokio::test]
async fn removing_the_entry_asks_for_the_files_and_the_exclusion_and_checks_it_gone() {
    let (base, server) = fake(vec![(200, MOVIE), (200, ""), (404, "")]);
    let http = client();
    let evicted = Radarr::new(&http, &base, "secret", false).evict(7, DeleteMode::RemoveEntry, true).await.expect("removed");
    assert!(matches!(evicted, Evicted::Done(RestoreTarget::Radarr { mode: DeleteMode::RemoveEntry, .. })));
    let seen = server.join().expect("fake");
    assert_eq!(seen[1].0, "DELETE /api/v3/movie/7?deleteFiles=true&addImportExclusion=true");
}

const SERIES: &str = r#"{"id":3,"title":"Show","seasons":[{"seasonNumber":1,"monitored":true},{"seasonNumber":2,"monitored":true}]}"#;
const SERIES_AFTER: &str = r#"{"id":3,"seasons":[{"seasonNumber":1,"monitored":false},{"seasonNumber":2,"monitored":true}]}"#;
const FILES: &str = r#"[{"id":11,"seasonNumber":1},{"id":12,"seasonNumber":1},{"id":21,"seasonNumber":2}]"#;
const FILES_AFTER: &str = r#"[{"id":21,"seasonNumber":2}]"#;

#[tokio::test]
async fn a_season_is_unmonitored_and_only_its_episode_files_are_deleted() {
    let (base, server) = fake(vec![(200, SERIES), (200, FILES), (202, SERIES_AFTER), (200, ""), (200, FILES_AFTER), (200, SERIES_AFTER)]);
    let http = client();
    let evicted = Sonarr::new(&http, &base, "secret", false).evict(3, 1).await.expect("deleted");
    assert_eq!(evicted, Evicted::Done(RestoreTarget::Sonarr { series_id: 3, season: 1 }));
    let seen = server.join().expect("fake");
    assert_eq!(seen[2].0, "PUT /api/v3/series/3");
    let put: serde_json::Value = serde_json::from_str(&seen[2].1).expect("series body");
    assert_eq!(put["seasons"][0]["monitored"], false);
    assert_eq!(put["seasons"][1]["monitored"], true, "other seasons keep their flag");
    assert_eq!(put["title"], "Show", "the whole resource goes back");
    assert_eq!(seen[3], ("DELETE /api/v3/episodefile/bulk".to_string(), r#"{"episodeFileIds":[11,12]}"#.to_string()));
}

#[tokio::test]
async fn a_dry_run_reads_but_sends_no_write() {
    // Only the reads are scripted: a write would find no answer and fail.
    let (radarr_base, radarr) = fake(vec![(200, MOVIE)]);
    let (sonarr_base, sonarr) = fake(vec![(200, SERIES), (200, FILES)]);
    let (seerr_base, seerr) = fake(vec![(200, r#"{"mediaInfo":{"id":9,"requests":[]}}"#)]);
    let http = client();
    let movie = Radarr::new(&http, &radarr_base, "secret", true).evict(7, DeleteMode::FileAndUnmonitor, false).await.expect("printed");
    let season = Sonarr::new(&http, &sonarr_base, "secret", true).evict(3, 1).await.expect("printed");
    let cleared = Seerr::new(&http, &seerr_base, "secret", true).clear_movie(603).await.expect("printed");
    assert_eq!((movie, season, cleared), (Evicted::Simulated, Evicted::Simulated, Cleared::Simulated));
    for server in [radarr, sonarr, seerr] {
        assert!(server.join().expect("fake").iter().all(|(line, _)| line.starts_with("GET ")), "a dry run sent a write");
    }
}

#[tokio::test]
async fn a_season_request_shared_with_other_seasons_is_kept_and_its_own_deleted() {
    let show = r#"{"mediaInfo":{"id":9,"requests":[{"id":1,"seasons":[{"seasonNumber":1}]},{"id":2,"seasons":[{"seasonNumber":1},{"seasonNumber":2}]}]}}"#;
    let after = r#"{"mediaInfo":{"id":9,"requests":[{"id":2,"seasons":[{"seasonNumber":1},{"seasonNumber":2}]}]}}"#;
    let (base, server) = fake(vec![(200, show), (204, ""), (200, after)]);
    let http = client();
    let cleared = Seerr::new(&http, &base, "secret", false).clear_season(1399, 1).await.expect("cleared");
    assert_eq!(cleared, Cleared::Requests(1));
    let seen = server.join().expect("fake");
    assert_eq!(seen[1].0, "DELETE /api/v1/request/1");
}

#[tokio::test]
async fn a_redirect_is_an_error_not_followed() {
    let (base, server) = fake(vec![(302, "")]);
    let http = client();
    let result = Radarr::new(&http, &base, "secret", false).movie(7).await;
    server.join().expect("fake");
    assert!(matches!(result, Err(ExecutorError::Http { status: 302, .. })), "{result:?}");
}

#[rstest]
#[case::untouched(Some(PlexWatch { last_viewed_at: Some(50), view_offset_ms: 0 }), Recheck::Clear)]
#[case::never_played(Some(PlexWatch::default()), Recheck::Clear)]
#[case::played_since(Some(PlexWatch { last_viewed_at: Some(150), view_offset_ms: 0 }), Recheck::PlayedSince(150))]
#[case::partway_now(Some(PlexWatch { last_viewed_at: Some(50), view_offset_ms: 60_000 }), Recheck::InProgress)]
#[case::gone_from_plex(None, Recheck::Gone)]
fn the_last_look_before_a_delete(#[case] watch: Option<PlexWatch>, #[case] expected: Recheck) {
    assert_eq!(judge(100, watch), expected);
}
