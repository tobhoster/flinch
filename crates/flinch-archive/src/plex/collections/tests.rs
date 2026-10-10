//! The collections client against a scripted fake Plex over a real socket:
//! each write must land on the documented endpoint and be read back.

use super::{CollectionError, PlexCollections};
use crate::card::LibraryKind;
use rstest::rstest;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::JoinHandle;

/// Serves `script` bodies in order, one connection each, and returns the
/// request lines it saw.
fn fake_plex(script: Vec<(u16, &'static str)>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the fake Plex");
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
            let text = String::from_utf8_lossy(&request);
            seen.push(text.lines().next().unwrap_or_default().to_string());
            assert!(text.contains("x-plex-token: owner-token"), "token header missing: {text}");
            write!(
                stream,
                "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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

const ROOT: &str = r#"{"MediaContainer":{"machineIdentifier":"abc123"}}"#;
const NONE: &str = r#"{"MediaContainer":{"size":0}}"#;
const LISTED: &str =
    r#"{"MediaContainer":{"Metadata":[{"ratingKey":"9","title":"Leaving Soon","smart":"1"},{"ratingKey":"77","title":"Leaving Soon"}]}}"#;
const MEMBERS: &str = r#"{"MediaContainer":{"totalSize":2,"Metadata":[{"ratingKey":"10"},{"ratingKey":"11"}]}}"#;

#[tokio::test]
async fn find_takes_the_regular_collection_and_skips_a_smart_namesake() {
    let (base, server) = fake_plex(vec![(200, LISTED)]);
    let http = client();
    let found = PlexCollections::new(&http, &base, "owner-token", false).find(1, "Leaving Soon").await.expect("listed");
    assert_eq!(found.as_deref(), Some("77"));
    assert_eq!(server.join().expect("fake"), ["GET /library/sections/1/all?type=18 HTTP/1.1"]);
}

#[tokio::test]
async fn ensure_creates_with_the_seed_through_a_server_uri_and_reads_it_back() {
    let created = r#"{"MediaContainer":{"Metadata":[{"ratingKey":77,"title":"Leaving Soon"}]}}"#;
    let (base, server) = fake_plex(vec![(200, NONE), (200, ROOT), (200, ""), (200, created), (200, MEMBERS)]);
    let http = client();
    let seed = ["10".to_string(), "11".to_string()];
    let key = PlexCollections::new(&http, &base, "owner-token", false)
        .ensure(1, "Leaving Soon", LibraryKind::Season, &seed)
        .await
        .expect("created");
    assert_eq!(key.as_deref(), Some("77"));
    let seen = server.join().expect("fake");
    assert_eq!(
        seen[2],
        "POST /library/collections?uri=server%3A%2F%2Fabc123%2Fcom.plexapp.plugins.library%2Flibrary%2Fmetadata%2F10%2C11&type=3&title=Leaving+Soon&smart=0&sectionId=1 HTTP/1.1"
    );
}

#[rstest]
#[case::all_listed(MEMBERS, true)]
#[case::one_ignored(r#"{"MediaContainer":{"Metadata":[{"ratingKey":"10"}]}}"#, false)]
#[tokio::test]
async fn an_add_counts_only_once_the_read_back_lists_every_item(#[case] after: &'static str, #[case] applied: bool) {
    let (base, server) = fake_plex(vec![(200, ROOT), (200, ""), (200, after)]);
    let http = client();
    let result = PlexCollections::new(&http, &base, "owner-token", false).add("77", &["10".to_string(), "11".to_string()]).await;
    assert_eq!(result.is_ok(), applied, "{result:?}");
    if !applied {
        assert!(matches!(result, Err(CollectionError::NotApplied { .. })));
    }
    assert!(server.join().expect("fake")[1].starts_with("PUT /library/collections/77/items?uri=server%3A%2F%2Fabc123"));
}

#[tokio::test]
async fn a_remove_still_listed_is_not_applied() {
    let (base, server) = fake_plex(vec![(200, ""), (200, MEMBERS)]);
    let http = client();
    let result = PlexCollections::new(&http, &base, "owner-token", false).remove("77", "11").await;
    assert!(matches!(result, Err(CollectionError::NotApplied { .. })), "{result:?}");
    assert_eq!(server.join().expect("fake")[0], "DELETE /library/collections/77/items/11 HTTP/1.1");
}

#[tokio::test]
async fn promoting_an_unmanaged_collection_posts_it_then_reads_the_hub_back() {
    let hub = r#"{"MediaContainer":{"Hub":[{"identifier":"custom.collection.1.77","promotedToRecommended":true,"promotedToOwnHome":1,"promotedToSharedHome":"1"}]}}"#;
    let (base, server) = fake_plex(vec![(200, NONE), (200, ""), (200, hub)]);
    let http = client();
    PlexCollections::new(&http, &base, "owner-token", false).promote_home("77", 1).await.expect("promoted");
    let seen = server.join().expect("fake");
    assert_eq!(
        seen[1],
        "POST /hubs/sections/1/manage?promotedToRecommended=1&promotedToOwnHome=1&promotedToSharedHome=1&metadataItemId=77 HTTP/1.1"
    );
}

#[tokio::test]
async fn a_dry_run_sends_no_write() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).expect("pollable");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let http = client();
    let plex = PlexCollections::new(&http, &base, "owner-token", true);
    plex.add("77", &["10".to_string()]).await.expect("printed");
    plex.remove("77", "10").await.expect("printed");
    assert!(listener.accept().is_err(), "a dry run must not connect");
}

#[tokio::test]
async fn a_redirect_is_an_error_not_followed() {
    let (base, server) = fake_plex(vec![(302, "")]);
    let http = client();
    let result = PlexCollections::new(&http, &base, "owner-token", false).members("77").await;
    server.join().expect("fake");
    assert!(matches!(result, Err(CollectionError::Http { status: 302, .. })), "{result:?}");
}

#[rstest]
#[case::read_back(r#"{"MediaContainer":{"Metadata":[{"ratingKey":"77","summary":"Leaves Oct 23"}]}}"#, true)]
#[case::ignored(r#"{"MediaContainer":{"Metadata":[{"ratingKey":"77","summary":"old"}]}}"#, false)]
#[tokio::test]
async fn a_summary_is_written_locked_through_the_section_and_read_back(#[case] after: &'static str, #[case] applied: bool) {
    let before = r#"{"MediaContainer":{"Metadata":[{"ratingKey":"77","summary":"old"}]}}"#;
    let (base, server) = fake_plex(vec![(200, before), (200, ""), (200, after)]);
    let http = client();
    let result = PlexCollections::new(&http, &base, "owner-token", false).edit_summary(3, "77", "Leaves Oct 23").await;
    let seen = server.join().expect("fake");
    assert_eq!(seen[0], "GET /library/metadata/77 HTTP/1.1");
    assert_eq!(seen[1], "PUT /library/sections/3/all?type=18&id=77&summary.value=Leaves+Oct+23&summary.locked=1 HTTP/1.1");
    assert_eq!(result.is_ok(), applied, "{result:?}");
}

#[tokio::test]
async fn an_unchanged_summary_is_not_written_again() {
    let (base, server) = fake_plex(vec![(200, r#"{"MediaContainer":{"Metadata":[{"ratingKey":"77","summary":"Leaves Oct 23\n"}]}}"#)]);
    let http = client();
    PlexCollections::new(&http, &base, "owner-token", false).edit_summary(3, "77", "Leaves Oct 23").await.expect("nothing to do");
    assert_eq!(server.join().expect("fake").len(), 1, "one read, no write");
}
