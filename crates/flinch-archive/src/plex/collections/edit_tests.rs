//! Ordering and removal against a scripted fake Plex: the documented calls,
//! read back, and nothing sent in a dry run.

use super::super::PlexCollections;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::JoinHandle;

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
            seen.push(String::from_utf8_lossy(&request).lines().next().unwrap_or_default().to_string());
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

const REVERSED: &str = r#"{"MediaContainer":{"Metadata":[{"ratingKey":"11"},{"ratingKey":"10"}]}}"#;
const ORDERED: &str = r#"{"MediaContainer":{"Metadata":[{"ratingKey":"10"},{"ratingKey":"11"}]}}"#;
const RELEASE_SORT: &str = r#"{"MediaContainer":{"Metadata":[{"ratingKey":"77","collectionSort":"0"}]}}"#;
const EMPTY: &str = r#"{"MediaContainer":{"size":0}}"#;

fn wanted() -> Vec<String> {
    vec!["10".into(), "11".into()]
}

#[tokio::test]
async fn ordering_switches_to_a_custom_sort_then_moves_and_reads_back() {
    let (base, server) = fake_plex(vec![(200, REVERSED), (200, RELEASE_SORT), (200, ""), (200, ""), (200, ""), (200, ORDERED)]);
    let http = reqwest::Client::new();
    let moved = PlexCollections::new(&http, &base, "owner-token", false).order("77", &wanted()).await.expect("ordered");
    assert_eq!(moved, 2);
    let seen = server.join().expect("fake");
    assert_eq!(
        seen[2..5],
        [
            "PUT /library/collections/77/prefs?collectionSort=2 HTTP/1.1",
            "PUT /library/collections/77/items/10/move HTTP/1.1",
            "PUT /library/collections/77/items/11/move?after=10 HTTP/1.1",
        ]
    );
}

#[tokio::test]
async fn an_order_plex_ignored_is_not_applied() {
    let custom = r#"{"MediaContainer":{"Metadata":[{"ratingKey":"77","collectionSort":2}]}}"#;
    let (base, _server) = fake_plex(vec![(200, REVERSED), (200, custom), (200, ""), (200, ""), (200, REVERSED)]);
    let http = reqwest::Client::new();
    let result = PlexCollections::new(&http, &base, "owner-token", false).order("77", &wanted()).await;
    assert!(matches!(result, Err(super::CollectionError::NotApplied { .. })), "{result:?}");
}

#[tokio::test]
async fn a_dry_run_reads_but_sends_no_write() {
    let (base, server) = fake_plex(vec![(200, REVERSED), (200, RELEASE_SORT), (200, EMPTY)]);
    let http = reqwest::Client::new();
    let plex = PlexCollections::new(&http, &base, "owner-token", true);
    assert_eq!(plex.order("77", &wanted()).await.expect("printed"), 2);
    assert!(plex.delete_if_empty("78").await.expect("printed"));
    assert!(server.join().expect("fake").iter().all(|line| line.starts_with("GET ")));
}

#[tokio::test]
async fn only_an_empty_collection_is_deleted_and_read_back_gone() {
    let (base, server) = fake_plex(vec![(200, ORDERED), (200, EMPTY), (200, ""), (404, "")]);
    let http = reqwest::Client::new();
    let plex = PlexCollections::new(&http, &base, "owner-token", false);
    assert!(!plex.delete_if_empty("77").await.expect("kept"));
    assert!(plex.delete_if_empty("78").await.expect("deleted"));
    assert_eq!(server.join().expect("fake")[2], "DELETE /library/collections/78 HTTP/1.1");
}
