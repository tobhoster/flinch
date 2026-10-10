//! The Jellyfin/Emby Leaving Soon shelf and last look against a scripted fake
//! server over a real socket: every write is read back, a dry run sends none,
//! and a redirect is an error that carries neither the key nor the address.

use super::super::{last_look, JellyfinClient, JellyfinError, ServerKind};
use super::JellyfinCollections;
use crate::card::LibraryKind;
use crate::executor::recheck::{judge, Recheck};
use rstest::rstest;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::JoinHandle;

/// Serves `(status, body)` in order, one connection each, and returns the
/// request heads it saw.
fn fake_server(script: Vec<(u16, &'static str)>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the fake server");
    let base = format!("http://{}", listener.local_addr().expect("fake address"));
    let handle = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for (status, body) in script {
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
            let extra = if status == 302 { "Location: http://elsewhere.invalid/\r\n" } else { "" };
            write!(
                stream,
                "HTTP/1.1 {status} X\r\n{extra}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .expect("the answer is sent");
        }
        seen
    });
    (base, handle)
}

const USERS: &str =
    r#"[{"Id":"u0","Name":"kid","Policy":{"IsAdministrator":false}},{"Id":"u1","Name":"ann","Policy":{"IsAdministrator":true}}]"#;
const NONE: &str = r#"{"Items":[],"TotalRecordCount":0}"#;
const SHELF: &str = r#"{"Items":[{"Id":"c1","Type":"BoxSet","Name":"Leaving Soon"},{"Id":"c9","Type":"BoxSet","Name":"Leaving Soon 2"}],"TotalRecordCount":2}"#;
const MOVIE_AND_SEASON: &str = r#"{"Items":[{"Id":"m1","Type":"Movie"},{"Id":"s1","Type":"Season"}],"TotalRecordCount":2}"#;
const MOVIE_ONLY: &str = r#"{"Items":[{"Id":"m1","Type":"Movie"}],"TotalRecordCount":1}"#;

fn seed() -> Vec<String> {
    vec!["m1".into(), "s1".into()]
}

#[rstest]
#[case::jellyfin(ServerKind::Jellyfin, "GET /Items?userId=u1&")]
#[case::emby(ServerKind::Emby, "GET /Users/u1/Items?")]
#[tokio::test]
async fn a_missing_shelf_is_created_with_its_items_and_read_back(#[case] kind: ServerKind, #[case] listing: &str) {
    let (base, server) = fake_server(vec![(200, USERS), (200, NONE), (200, r#"{"Id":"c1"}"#), (200, SHELF), (200, MOVIE_AND_SEASON)]);
    let client = JellyfinClient::new(&base, "secret", kind).expect("client");
    let shelf = JellyfinCollections::new(&client, false).ensure("Leaving Soon", &seed()).await.expect("ensure");
    let seen = server.join().expect("fake server");
    assert_eq!(shelf, Some(("c1".to_string(), true)));
    assert!(seen[1].starts_with(listing) && seen[1].contains("IncludeItemTypes=BoxSet"), "found through the administrator: {}", seen[1]);
    assert!(seen[2].starts_with("POST /Collections?Name=Leaving+Soon&Ids=m1%2Cs1 "), "{}", seen[2]);
    assert!(seen[4].contains("ParentId=c1"), "members read back: {}", seen[4]);
}

#[tokio::test]
async fn an_existing_shelf_gains_only_new_members_and_each_is_read_back() {
    let script = vec![(200, USERS), (200, SHELF), (200, NONE), (204, ""), (200, MOVIE_AND_SEASON)];
    let (base, server) = fake_server(script);
    let client = JellyfinClient::new(&base, "secret", ServerKind::Jellyfin).expect("client");
    let shelf = JellyfinCollections::new(&client, false).ensure("Leaving Soon", &seed()).await.expect("ensure");
    let seen = server.join().expect("fake server");
    assert_eq!(shelf, Some(("c1".to_string(), false)), "the exact name, not 'Leaving Soon 2'");
    assert!(seen[3].starts_with("POST /Collections/c1/Items?Ids=m1%2Cs1 "), "{}", seen[3]);
}

/// The fail-closed path for seasons: a server that answers 204 but drops the
/// Season member fails the read-back, so the season is never recorded as shelved.
#[tokio::test]
async fn a_season_the_server_drops_fails_the_read_back() {
    let (base, server) = fake_server(vec![(200, USERS), (200, NONE), (204, ""), (200, MOVIE_ONLY)]);
    let client = JellyfinClient::new(&base, "secret", ServerKind::Jellyfin).expect("client");
    let error = JellyfinCollections::new(&client, false).add("c1", &seed()).await.expect_err("s1 is missing");
    server.join().expect("fake server");
    assert!(matches!(&error, JellyfinError::NotApplied { detail, .. } if detail == "not members: s1"), "{error}");
}

#[tokio::test]
async fn a_member_is_removed_and_read_back_gone() {
    let (base, server) = fake_server(vec![(204, ""), (200, USERS), (200, MOVIE_ONLY)]);
    let client = JellyfinClient::new(&base, "secret", ServerKind::Jellyfin).expect("client");
    JellyfinCollections::new(&client, false).remove("c1", "s1").await.expect("removed");
    let seen = server.join().expect("fake server");
    assert!(seen[0].starts_with("DELETE /Collections/c1/Items?Ids=s1 "), "{}", seen[0]);
}

#[tokio::test]
async fn a_dry_run_reads_but_sends_no_write() {
    let (base, server) = fake_server(vec![(200, USERS), (200, NONE)]);
    let client = JellyfinClient::new(&base, "secret", ServerKind::Jellyfin).expect("client");
    let collections = JellyfinCollections::new(&client, true);
    assert_eq!(collections.ensure("Leaving Soon", &seed()).await.expect("dry ensure"), None);
    collections.remove("c1", "m1").await.expect("dry remove");
    let seen = server.join().expect("fake server");
    assert!(seen.iter().all(|head| head.starts_with("GET ")), "{seen:?}");
}

#[tokio::test]
async fn a_redirect_is_an_error_without_the_key_or_address() {
    let (base, server) = fake_server(vec![(302, "")]);
    let client = JellyfinClient::new(&base, "secret", ServerKind::Jellyfin).expect("client");
    let error = JellyfinCollections::new(&client, false).remove("c1", "m1").await.expect_err("not followed");
    server.join().expect("fake server");
    let text = error.to_string();
    assert!(matches!(error, JellyfinError::Status { status: 302, .. }), "{text}");
    assert!(!text.contains("secret") && !text.contains("127.0.0.1"), "{text}");
}

const TWO_USERS: &str = r#"[{"Id":"u1","Name":"ann"},{"Id":"u2","Name":"bo"}]"#;

/// Announced at 1_704_067_200 (2024-01-01); the shelf holds a movie.
#[rstest]
#[case::nobody_since(
    r#"{"Items":[{"Id":"m1","Type":"Movie","UserData":{"Played":true,"LastPlayedDate":"2023-06-01T00:00:00Z"}}]}"#,
    Recheck::Clear
)]
#[case::played_on_the_shelf(
    r#"{"Items":[{"Id":"m1","Type":"Movie","UserData":{"Played":true,"LastPlayedDate":"2024-01-03T00:00:00Z"}}]}"#,
    Recheck::PlayedSince(1_704_240_000)
)]
#[case::partway(r#"{"Items":[{"Id":"m1","Type":"Movie","UserData":{"PlaybackPositionTicks":600000000}}]}"#, Recheck::InProgress)]
#[case::gone(r#"{"Items":[]}"#, Recheck::Gone)]
#[tokio::test]
async fn the_last_look_reads_every_user(#[case] bo: &'static str, #[case] verdict: Recheck) {
    let (base, server) = fake_server(vec![(200, TWO_USERS), (200, r#"{"Items":[]}"#), (200, bo)]);
    let client = JellyfinClient::new(&base, "secret", ServerKind::Jellyfin).expect("client");
    let watch = last_look(&client, "m1", LibraryKind::Movie).await.expect("look");
    let seen = server.join().expect("fake server");
    assert_eq!(judge(1_704_067_200, watch), verdict);
    assert!(seen[2].starts_with("GET /Items?userId=u2&") && seen[2].contains("Ids=m1"), "{}", seen[2]);
}

#[tokio::test]
async fn a_season_is_looked_at_through_its_episodes() {
    let episodes = r#"{"Items":[{"Id":"e1","Type":"Episode","UserData":{"Played":true,"LastPlayedDate":"2024-01-03T00:00:00Z"}}]}"#;
    let (base, server) = fake_server(vec![(200, TWO_USERS), (200, episodes), (200, r#"{"Items":[]}"#)]);
    let client = JellyfinClient::new(&base, "secret", ServerKind::Emby).expect("client");
    let watch = last_look(&client, "s1", LibraryKind::Season).await.expect("look");
    let seen = server.join().expect("fake server");
    assert_eq!(judge(1_704_067_200, watch), Recheck::PlayedSince(1_704_240_000));
    assert!(seen[1].starts_with("GET /Users/u1/Items?") && seen[1].contains("ParentId=s1"), "{}", seen[1]);
}
