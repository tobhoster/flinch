//! The Jellyfin/Emby reader against a scripted fake server over a real socket,
//! and the join from its rows to library targets.

use super::map::parse_utc;
use super::{evidence, JellyfinClient, JellyfinItem, JellyfinRead, ServerKind, UserItems};
use crate::card::LibraryKind;
use crate::fit::plays::Viewer;
use crate::ids::ExternalIds;
use crate::plex::WatchTarget;
use crate::watch::WatchSource;
use rstest::rstest;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::JoinHandle;

/// Serves `script` bodies in order, one connection each, and returns the
/// request heads it saw.
fn fake_server(script: Vec<&'static str>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the fake server");
    let base = format!("http://{}", listener.local_addr().expect("fake address"));
    let handle = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for body in script {
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
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .expect("the answer is sent");
        }
        seen
    });
    (base, handle)
}

const USERS: &str = r#"[{"Id":"u1","Name":"ann"},{"Id":"u2","Name":"bo"}]"#;
const ANN_PAGE_1: &str = r#"{"Items":[{"Id":"m1","Type":"Movie","ProviderIds":{"Tmdb":"603"},"UserData":{"Played":true,"PlayCount":1,"LastPlayedDate":"2024-01-02T03:04:05.0000000Z"}}],"TotalRecordCount":2}"#;
const ANN_PAGE_2: &str = r#"{"Items":[{"Id":"m2","Type":"Movie","ProviderIds":{"Imdb":"tt0001"}}],"TotalRecordCount":2}"#;
const BO_PAGE: &str =
    r#"{"Items":[{"Id":"m1","Type":"Movie","ProviderIds":{"Tmdb":"603"},"UserData":{"Played":false}}],"TotalRecordCount":1}"#;

#[rstest]
#[case::jellyfin(ServerKind::Jellyfin, "authorization: MediaBrowser Token=\"secret\"", "GET /Items?userId=u1&")]
#[case::emby(ServerKind::Emby, "x-emby-token: secret", "GET /Users/u1/Items?")]
#[tokio::test]
async fn every_user_is_read_to_the_servers_total(#[case] kind: ServerKind, #[case] auth: &str, #[case] items_path: &str) {
    let (base, server) = fake_server(vec![USERS, ANN_PAGE_1, ANN_PAGE_2, BO_PAGE]);
    let read = JellyfinClient::new(&base, "secret", kind).expect("client").read().await.expect("read");
    let seen = server.join().expect("fake server");
    assert!(read.complete, "{:?}", read.problems);
    assert_eq!(read.users.iter().map(|user| user.items.len()).collect::<Vec<_>>(), vec![2, 1]);
    assert!(seen[0].starts_with("GET /Users "), "{}", seen[0]);
    assert!(seen[1].starts_with(items_path), "{}", seen[1]);
    assert!(seen[2].contains("StartIndex=1"), "the second page starts after the first: {}", seen[2]);
    assert!(seen.iter().all(|head| head.to_ascii_lowercase().contains(&auth.to_ascii_lowercase())), "key header on every request");
}

#[tokio::test]
async fn a_listing_that_stops_short_of_its_total_leaves_the_read_incomplete() {
    let short = r#"{"Items":[],"TotalRecordCount":5}"#;
    let (base, server) = fake_server(vec![USERS, ANN_PAGE_1, short, BO_PAGE]);
    let read = JellyfinClient::new(&base, "secret", ServerKind::Jellyfin).expect("client").read().await.expect("read");
    server.join().expect("fake server");
    assert!(!read.complete, "ann's second page never came: the total was not reached");
    assert_eq!(read.users.len(), 1, "bo was still read");
    assert!(read.problems[0].starts_with("user ann:"), "{:?}", read.problems);
    assert!(read.problems.iter().all(|problem| !problem.contains("secret") && !problem.contains("127.0.0.1")), "{:?}", read.problems);
}

fn movie(id: &str, tmdb: u32) -> WatchTarget {
    target(id, LibraryKind::Movie, None, ExternalIds { tmdb: Some(tmdb), ..ExternalIds::default() }, None)
}

fn season(id: &str, tvdb: u32, number: u32, files: u32) -> WatchTarget {
    target(id, LibraryKind::Season, Some(number), ExternalIds { tvdb: Some(tvdb), ..ExternalIds::default() }, Some(files))
}

fn target(id: &str, kind: LibraryKind, season_index: Option<u32>, external: ExternalIds, files: Option<u32>) -> WatchTarget {
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
        added_epoch: None,
        on_disk: true,
    }
}

fn items(json: &str) -> Vec<JellyfinItem> {
    serde_json::from_str(json).expect("item fixture")
}

fn read(complete: bool, users: Vec<(&str, &str)>) -> JellyfinRead {
    let users = users.into_iter().map(|(user, json)| UserItems { user_id: user.into(), name: user.into(), items: items(json) }).collect();
    JellyfinRead { users, complete, problems: Vec::new() }
}

const SHOW: &str = r#"{"Id":"s","Type":"Series","ProviderIds":{"Tvdb":"70"}}"#;

#[test]
fn a_movie_one_user_finished_is_watched_with_that_users_dated_play() {
    let targets = [movie("radarr-1", 603), movie("radarr-2", 604)];
    let ann = r#"[{"Id":"m1","Type":"Movie","ProviderIds":{"Tmdb":"603"},"UserData":{"Played":true,"PlayCount":1,"LastPlayedDate":"2024-01-01T00:00:00Z"}},
                  {"Id":"m2","Type":"Movie","ProviderIds":{"Tmdb":"604"}}]"#;
    let bo = r#"[{"Id":"m1","Type":"Movie","ProviderIds":{"tmdb":"603"},"UserData":{"Played":false}}]"#;
    let found = evidence(&targets, &read(true, vec![("u1", ann), ("u2", bo)]));
    let watched = &found.entries["radarr-1"];
    assert_eq!((watched.progress, watched.last_watched_epoch, watched.source), (1.0, Some(1_704_067_200), WatchSource::Jellyfin));
    let plays = &found.plays["radarr-1"].item;
    assert_eq!(plays.len(), 1);
    assert_eq!(plays[0].viewer, Some(Viewer::JellyfinUser("u1".into())));
    let unplayed = &found.entries["radarr-2"];
    assert_eq!((unplayed.progress, unplayed.last_watched_epoch), (0.0, None), "every user read: nobody played it");
}

#[test]
fn an_incomplete_read_claims_no_watch_state_but_keeps_its_plays() {
    let targets = [movie("radarr-1", 603), movie("radarr-2", 604)];
    let ann = r#"[{"Id":"m1","Type":"Movie","ProviderIds":{"Tmdb":"603"},"UserData":{"Played":true,"LastPlayedDate":"2024-01-01T00:00:00Z"}},
                  {"Id":"m2","Type":"Movie","ProviderIds":{"Tmdb":"604"}}]"#;
    let found = evidence(&targets, &read(false, vec![("u1", ann)]));
    assert!(found.entries.is_empty(), "a skipped user may have played radarr-2: no zero, and no state at all");
    assert_eq!(found.plays["radarr-1"].item.len(), 1, "a play read is a play");
}

#[test]
fn an_id_two_targets_share_joins_neither() {
    let targets = [movie("radarr-1", 603), movie("radarr-9", 603)];
    let ann = r#"[{"Id":"m1","Type":"Movie","ProviderIds":{"Tmdb":"603"},"UserData":{"Played":true}}]"#;
    let found = evidence(&targets, &read(true, vec![("u1", ann)]));
    assert_eq!(found.resolved, 0);
    assert!(found.entries.is_empty());
}

#[rstest]
#[case::both_finished_by_someone(2, Some(1.0))]
#[case::count_disagrees_with_sonarr(3, None)]
fn a_season_joins_by_series_id_and_number_only_when_its_episode_count_agrees(#[case] files: u32, #[case] progress: Option<f32>) {
    let targets = [season("sonarr-7-s1", 70, 1, files)];
    let ann = format!(
        r#"[{SHOW},{{"Id":"e1","Type":"Episode","SeriesId":"s","ParentIndexNumber":1,"IndexNumber":1,"UserData":{{"Played":true,"LastPlayedDate":"2024-01-01T00:00:00Z"}}}},
            {{"Id":"e2","Type":"Episode","SeriesId":"s","ParentIndexNumber":1,"IndexNumber":2}},
            {{"Id":"e3","Type":"Episode","SeriesId":"s","ParentIndexNumber":1,"IndexNumber":3,"LocationType":"Virtual"}},
            {{"Id":"e9","Type":"Episode","SeriesId":"s","ParentIndexNumber":2,"IndexNumber":1,"UserData":{{"Played":true,"LastPlayedDate":"2024-02-01T00:00:00Z"}}}}]"#
    );
    let bo = format!(
        r#"[{SHOW},{{"Id":"e2","Type":"Episode","SeriesId":"s","ParentIndexNumber":1,"IndexNumber":2,"UserData":{{"Played":true,"LastPlayedDate":"2024-01-05T00:00:00Z"}}}}]"#
    );
    let found = evidence(&targets, &read(true, vec![("u1", &ann), ("u2", &bo)]));
    assert_eq!(found.entries.get("sonarr-7-s1").map(|entry| entry.progress), progress, "the virtual episode is not a file");
    if progress.is_some() {
        assert_eq!(found.plays["sonarr-7-s1"].item.len(), 2);
        assert_eq!(found.plays["sonarr-7-s1"].audience.len(), 3, "season 2's play speaks for the show");
    }
}

#[rstest]
#[case::utc("2024-01-01T00:00:00.0000000Z", Some(1_704_067_200))]
#[case::offset("2024-01-01T02:00:00+02:00", Some(1_704_067_200))]
#[case::no_zone("2024-01-01T00:00:00", Some(1_704_067_200))]
#[case::leap_day("2024-02-29T12:00:00Z", Some(1_709_208_000))]
#[case::garbage("yesterday", None)]
#[case::bad_month("2024-13-01T00:00:00Z", None)]
fn last_played_dates_read_as_epoch_seconds(#[case] text: &str, #[case] epoch: Option<u64>) {
    assert_eq!(parse_utc(text), epoch);
}

#[rstest]
#[case::one_season_id("x1", "x1", Some("x1"))]
#[case::episodes_disagree("x1", "x2", None)]
fn each_card_keeps_the_servers_own_item_id_for_the_shelf(#[case] first: &str, #[case] second: &str, #[case] season_id: Option<&str>) {
    let targets = [movie("radarr-1", 603), season("sonarr-7-s1", 70, 1, 2)];
    let ann = format!(
        r#"[{SHOW},{{"Id":"m1","Type":"Movie","ProviderIds":{{"Tmdb":"603"}}}},
            {{"Id":"e1","Type":"Episode","SeriesId":"s","SeasonId":"{first}","ParentIndexNumber":1,"IndexNumber":1}},
            {{"Id":"e2","Type":"Episode","SeriesId":"s","SeasonId":"{second}","ParentIndexNumber":1,"IndexNumber":2}}]"#
    );
    let found = evidence(&targets, &read(true, vec![("u1", &ann)]));
    assert_eq!(found.item_ids.get("radarr-1").map(String::as_str), Some("m1"));
    assert_eq!(found.item_ids.get("sonarr-7-s1").map(String::as_str), season_id, "never a guessed season");
}
