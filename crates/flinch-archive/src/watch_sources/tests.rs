//! The Tracearr and Trakt readers against scripted fake servers over a real
//! socket, and the catalogue-id join and absence rule.

use super::join::coverage_start;
use super::{evidence, Played, SourceKind, SourcePlay, SourceRead, TracearrClient, TraktClient};
use crate::card::LibraryKind;
use crate::fit::plays::Viewer;
use crate::ids::ExternalIds;
use crate::plex::WatchTarget;
use crate::watch::WatchSource;
use rstest::rstest;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::JoinHandle;

const DAY: u64 = 86_400;

/// Serves `(extra headers, body)` answers in order, one connection each, and
/// returns the request heads it saw.
fn fake_server(script: Vec<(&'static str, &'static str)>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the fake server");
    let base = format!("http://{}", listener.local_addr().expect("fake address"));
    let handle = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for (headers, body) in script {
            let (mut stream, _) = listener.accept().expect("the client connects");
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).expect("the request arrives");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            seen.push(String::from_utf8_lossy(&request).to_string());
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .expect("the answer is sent");
        }
        seen
    });
    (base, handle)
}

const USERS_1: &str = r#"{"data":[{"id":"u1","username":"ann"}],"meta":{"nextCursor":"c1","pageSize":100}}"#;
const USERS_2: &str = r#"{"data":[{"id":"u2","username":"bo"}],"meta":{"nextCursor":null,"pageSize":100}}"#;
const ANN_1: &str =
    r#"{"data":[{"started_at":"2024-01-02T00:00:00.000Z","watched":true,"media_type":"movie","tmdb_id":603}],"meta":{"nextCursor":"h1"}}"#;
const ANN_2: &str = r#"{"data":[{"started_at":"2024-01-01T00:00:00.000Z","watched":false,"percent_complete":40.0,"media_type":"episode","season_number":1,"episode_number":2,"tvdb_id":999,"show_media_id":"s1"}],"meta":{"nextCursor":null}}"#;
const SHOW_S1: &str = r#"{"id":"s1","media_type":"show","tvdb_id":70,"tmdb_id":null,"imdb_id":null}"#;
const BO: &str = r#"{"data":[{"started_at":"2024-01-03T00:00:00.000Z","watched":true,"media_type":"episode","season_number":1,"episode_number":1,"show_media_id":"s1"}],"meta":{"nextCursor":null}}"#;

#[tokio::test]
async fn tracearr_reads_every_user_to_the_end_of_their_history() {
    let (base, server) = fake_server(vec![("", USERS_1), ("", USERS_2), ("", ANN_1), ("", ANN_2), ("", SHOW_S1), ("", BO)]);
    let read = TracearrClient::new(&base, "trr_pub_secret").expect("client").read().await.expect("read");
    let seen = server.join().expect("fake server");
    assert!(read.complete, "{:?}", read.problems);
    assert_eq!(read.accounts, 2);
    assert!(seen[0].starts_with("GET /api/v2/public/users?include_removed=true&pageSize=100 "), "{}", seen[0]);
    assert!(seen[1].contains("cursor=c1"), "the next page follows the cursor: {}", seen[1]);
    assert!(seen[2].starts_with("GET /api/v2/public/history?user_id=u1&"), "{}", seen[2]);
    assert!(seen[3].contains("cursor=h1"), "{}", seen[3]);
    assert!(seen[4].starts_with("GET /api/v2/public/media/s1 "), "an episode's show ids come from its show: {}", seen[4]);
    assert!(seen[5].starts_with("GET /api/v2/public/history?user_id=u2&"), "the show is looked up once: {}", seen[5]);
    assert!(seen.iter().all(|head| head.to_ascii_lowercase().contains("authorization: bearer trr_pub_secret")));
    let show = ExternalIds { tvdb: Some(70), ..ExternalIds::default() };
    assert_eq!(read.plays[1].played, Played::Episode { show, season: 1, episode: Some(2) }, "the show's ids, not the episode's");
    assert_eq!(read.plays[1].fraction, 0.4);
    assert_eq!(read.plays[2].viewer, Viewer::TracearrUser("u2".into()));
}

#[tokio::test]
async fn a_tracearr_cursor_that_repeats_leaves_the_read_incomplete() {
    let looping = r#"{"data":[],"meta":{"nextCursor":"same"}}"#;
    let (base, server) = fake_server(vec![("", USERS_2), ("", looping), ("", looping)]);
    let read = TracearrClient::new(&base, "trr_pub_secret").expect("client").read().await.expect("read");
    server.join().expect("fake server");
    assert!(!read.complete, "a loop is not the end of the history");
    assert_eq!(read.accounts, 0);
    assert!(read.problems[0].starts_with("user bo:"), "{:?}", read.problems);
    assert!(read.problems.iter().all(|problem| !problem.contains("secret") && !problem.contains("127.0.0.1")), "{:?}", read.problems);
}

const TRAKT_1: &str = r#"[{"id":1,"watched_at":"2024-01-02T00:00:00.000Z","action":"scrobble","type":"movie","movie":{"ids":{"trakt":4,"imdb":"tt0468569","tmdb":155}}}]"#;
const TRAKT_2: &str = r#"[{"id":2,"watched_at":"2024-01-01T00:00:00.000Z","action":"watch","type":"episode","episode":{"season":2,"number":1,"ids":{"tvdb":797571}},"show":{"ids":{"tvdb":84912,"tmdb":8592}}}]"#;

#[tokio::test]
async fn trakt_pages_to_its_page_count_with_every_required_header() {
    let headers = "X-Pagination-Page-Count: 2\r\nX-Pagination-Item-Count: 2\r\n";
    let (base, server) = fake_server(vec![(headers, TRAKT_1), (headers, TRAKT_2)]);
    let read = TraktClient::new(&base, "access", "client").expect("client").read("ann").await.expect("read");
    let seen = server.join().expect("fake server");
    assert!(read.complete);
    assert!(seen[0].starts_with("GET /sync/history?page=1&limit=100 "), "{}", seen[0]);
    assert!(seen[1].starts_with("GET /sync/history?page=2&limit=100 "), "{}", seen[1]);
    for head in seen.iter().map(|head| head.to_ascii_lowercase()) {
        for header in ["authorization: bearer access", "trakt-api-key: client", "trakt-api-version: 2", "user-agent: flinch/"] {
            assert!(head.contains(header), "{header} missing from {head}");
        }
    }
    let show = ExternalIds { tvdb: Some(84912), tmdb: Some(8592), imdb: None };
    assert_eq!(read.plays[1].played, Played::Episode { show, season: 2, episode: Some(1) });
    assert_eq!(read.plays[0].viewer, Viewer::TraktUser("ann".into()));
}

#[rstest]
#[case::fewer_items_than_announced("X-Pagination-Page-Count: 1\r\nX-Pagination-Item-Count: 5\r\n")]
#[case::no_pagination_headers("")]
#[tokio::test]
async fn a_trakt_history_short_of_its_announced_size_is_an_error(#[case] headers: &'static str) {
    let (base, server) = fake_server(vec![(headers, TRAKT_1)]);
    let error = TraktClient::new(&base, "access", "client").expect("client").read("ann").await.expect_err("refused");
    server.join().expect("fake server");
    let text = error.to_string();
    assert!(text.starts_with("trakt history:") && !text.contains("access") && !text.contains("127.0.0.1"), "{text}");
}

fn target(id: &str, kind: LibraryKind, season_index: Option<u32>, external: ExternalIds, added_epoch: Option<u64>) -> WatchTarget {
    let files = season_index.map(|_| 2);
    WatchTarget {
        id: id.into(),
        kind,
        title: "anything".into(),
        year: None,
        show_title: None,
        season_index,
        episodes_total: files,
        episode_files: files,
        episodes_on_disk: files.map(|files| (1..=files).collect()),
        external,
        added_epoch,
        on_disk: true,
    }
}

fn tmdb(id: u32) -> ExternalIds {
    ExternalIds { tmdb: Some(id), ..ExternalIds::default() }
}

fn tvdb(id: u32) -> ExternalIds {
    ExternalIds { tvdb: Some(id), ..ExternalIds::default() }
}

fn play(epoch: u64, played: Played) -> SourcePlay {
    SourcePlay { epoch, viewer: Viewer::TraktUser("ann".into()), played, fraction: 1.0 }
}

fn read(plays: Vec<SourcePlay>, complete: bool) -> SourceRead {
    let epochs = plays.iter().map(|play| play.epoch).collect();
    SourceRead { plays, epochs, accounts: 1, complete, ..SourceRead::default() }
}

#[test]
fn plays_join_movies_by_their_ids_and_episodes_by_their_shows_ids_and_season() {
    let targets = vec![
        target("radarr-1", LibraryKind::Movie, None, tmdb(603), None),
        target("sonarr-7-s1", LibraryKind::Season, Some(1), tvdb(70), None),
        target("sonarr-7-s2", LibraryKind::Season, Some(2), tvdb(70), None),
    ];
    let plays = vec![
        play(100, Played::Movie(tmdb(603))),
        play(200, Played::Episode { show: tvdb(70), season: 1, episode: Some(1) }),
        play(300, Played::Episode { show: tvdb(70), season: 1, episode: Some(2) }),
        // Season 2 holds two episodes: an episode 9 is a different ordering.
        play(400, Played::Episode { show: tvdb(70), season: 2, episode: Some(9) }),
    ];
    let found = evidence(&targets, &read(plays, true), SourceKind::Trakt, 0, 500);
    assert_eq!(found.entries["radarr-1"].progress, 1.0);
    assert_eq!(found.entries["radarr-1"].source, WatchSource::Trakt);
    let season = &found.entries["sonarr-7-s1"];
    assert_eq!((season.progress, season.last_watched_epoch), (1.0, Some(300)), "both episodes finished");
    assert!(!found.entries.contains_key("sonarr-7-s2"), "an episode past the season's count joins nothing");
    assert_eq!(found.plays["sonarr-7-s2"].audience.len(), 3, "every play of the show speaks for its audience");
    assert_eq!(found.joined, 2);
    assert_eq!(found.never_played, 0, "trakt never claims absence");
}

#[test]
fn an_id_two_targets_share_joins_neither() {
    let targets =
        vec![target("radarr-1", LibraryKind::Movie, None, tmdb(603), None), target("radarr-2", LibraryKind::Movie, None, tmdb(603), None)];
    let found = evidence(&targets, &read(vec![play(100, Played::Movie(tmdb(603)))], true), SourceKind::Trakt, 0, 500);
    assert!(found.entries.is_empty() && found.plays.is_empty());
}

/// A record recording daily from 400 days ago until yesterday.
fn steady(now: u64) -> Vec<SourcePlay> {
    (1..=400).map(|days| play(now - days * DAY, Played::Movie(tmdb(1)))).collect()
}

#[rstest]
#[case::complete_record(true, 0, SourceKind::Tracearr, true)]
#[case::incomplete_record(false, 0, SourceKind::Tracearr, false)]
// History kept only 60 days: an arrival 100 days ago predates it.
#[case::outside_retention(true, 60, SourceKind::Tracearr, false)]
#[case::trakt_proves_plays_only(true, 0, SourceKind::Trakt, false)]
fn tracearr_claims_never_played_only_on_a_complete_record_that_saw_the_arrival(
    #[case] complete: bool,
    #[case] retention_days: u32,
    #[case] kind: SourceKind,
    #[case] claimed: bool,
) {
    let now = 1_000 * DAY;
    let targets = vec![target("radarr-2", LibraryKind::Movie, None, tmdb(2), Some(now - 100 * DAY))];
    let found = evidence(&targets, &read(steady(now), complete), kind, retention_days, now);
    assert_eq!(found.entries.get("radarr-2").map(|entry| entry.source), claimed.then_some(WatchSource::TracearrAbsence));
}

#[rstest]
#[case::recording(1, Some(600))]
// Nothing for 90 days: the record is not running, its silence proves nothing.
#[case::stopped(90, None)]
fn coverage_begins_at_the_latest_unbroken_run(#[case] last_days_ago: u64, #[case] start_days: Option<u64>) {
    let now = 1_000 * DAY;
    // An old run, a 100-day gap, then a steady run from day 600.
    let mut epochs: Vec<u64> = (100..=300).map(|day| day * DAY).collect();
    epochs.extend((600..=1_000 - last_days_ago).map(|day| day * DAY));
    assert_eq!(coverage_start(&epochs, 0, now), start_days.map(|day| day * DAY));
}
