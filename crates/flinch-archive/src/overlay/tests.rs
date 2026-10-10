//! Poster badges: the bookkeeping that guarantees a way back, the restore
//! against a scripted fake Plex, the drawing, and a dry run that sends nothing.

use super::*;
use rstest::rstest;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::JoinHandle;

const OCT_23: u64 = 1_792_713_600; // 2026-10-23 UTC

fn record(original: &str, badge: &str) -> Overlaid {
    Overlaid { original: original.into(), badge: badge.into(), applied_at: 1 }
}

#[test]
fn the_badge_reads_the_leave_date() {
    assert_eq!(badge_text(OCT_23), "LEAVES OCT 23");
}

#[rstest]
#[case::new_member_badged(&[], &[("7", OCT_23)], true, &[("7", "LEAVES OCT 23")], &[])]
#[case::already_badged(&[("7", "LEAVES OCT 23")], &[("7", OCT_23)], true, &[], &[])]
#[case::left_the_shelf(&[("7", "LEAVES OCT 23")], &[], true, &[], &["7"])]
#[case::turned_off(&[("7", "LEAVES OCT 23")], &[("7", OCT_23)], false, &[], &["7"])]
fn plan_badges_the_shelf_and_restores_the_rest(
    #[case] recorded: &[(&str, &str)],
    #[case] shelf: &[(&str, u64)],
    #[case] enabled: bool,
    #[case] apply: &[(&str, &str)],
    #[case] restore: &[&str],
) {
    let state =
        OverlayState { posters: recorded.iter().map(|(key, badge)| (key.to_string(), record("metadata://posters/a", badge))).collect() };
    let shelf: Vec<(String, u64)> = shelf.iter().map(|(key, until)| (key.to_string(), *until)).collect();
    let planned = plan(&state, &shelf, enabled);
    assert_eq!(planned.apply, apply.iter().map(|(key, text)| (key.to_string(), text.to_string())).collect::<Vec<_>>());
    assert_eq!(planned.restore, restore.iter().map(|key| key.to_string()).collect::<Vec<_>>());
}

#[test]
fn an_unreadable_record_is_an_error_not_a_fresh_start() {
    let dir = std::env::temp_dir().join(format!("flinch-overlays-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let _ = std::fs::remove_file(OverlayState::path(&dir));
    assert_eq!(OverlayState::read(&dir).expect("missing is empty"), OverlayState::default());
    std::fs::write(OverlayState::path(&dir), "{not json").expect("write");
    assert!(OverlayState::read(&dir).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

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

const BADGED: &str = r#"{"MediaContainer":{"Metadata":[{"ratingKey":"metadata://posters/a","selected":false},{"ratingKey":"upload://posters/b","selected":true}]}}"#;
const ORIGINAL: &str = r#"{"MediaContainer":{"Metadata":[{"ratingKey":"metadata://posters/a","selected":true},{"ratingKey":"upload://posters/b","selected":false}]}}"#;

#[tokio::test]
async fn restore_selects_the_original_reads_it_back_and_forgets_it() {
    let (base, server) = fake_plex(vec![(200, BADGED), (200, ""), (200, ORIGINAL)]);
    let http = reqwest::Client::new();
    let plex = PlexCollections::new(&http, &base, "owner-token", false);
    let mut state = OverlayState { posters: [("7".to_string(), record("metadata://posters/a", "LEAVES OCT 23"))].into() };
    restore(&plex, &mut state, "7").await.expect("restored");
    assert!(state.posters.is_empty());
    let seen = server.join().expect("fake");
    assert_eq!(seen[1], "PUT /library/metadata/7/poster?url=metadata%3A%2F%2Fposters%2Fa HTTP/1.1");
}

#[tokio::test]
async fn an_item_gone_from_plex_is_forgotten_without_a_write() {
    let (base, server) = fake_plex(vec![(404, "")]);
    let http = reqwest::Client::new();
    let plex = PlexCollections::new(&http, &base, "owner-token", false);
    let mut state = OverlayState { posters: [("7".to_string(), record("metadata://posters/a", "LEAVES OCT 23"))].into() };
    restore(&plex, &mut state, "7").await.expect("gone counts as restored");
    assert!(state.posters.is_empty());
    assert_eq!(server.join().expect("fake").len(), 1);
}

#[tokio::test]
async fn a_dry_run_sends_nothing_and_keeps_the_record() {
    // Nothing listens on port 9 (discard): any request would fail the call.
    let http = reqwest::Client::new();
    let plex = PlexCollections::new(&http, "http://127.0.0.1:9", "owner-token", true);
    let mut state = OverlayState { posters: [("7".to_string(), record("metadata://posters/a", "LEAVES OCT 23"))].into() };
    apply(&plex, &mut state, "8", "LEAVES OCT 23", 5).await.expect("printed only");
    restore(&plex, &mut state, "7").await.expect("printed only");
    assert_eq!(state.posters.len(), 1);
    assert!(!state.posters.contains_key("8"));
}

#[test]
fn the_band_is_drawn_across_the_bottom() {
    let mut poster = Vec::new();
    image::RgbImage::from_pixel(200, 300, image::Rgb([0, 0, 0]))
        .write_to(&mut std::io::Cursor::new(&mut poster), image::ImageFormat::Png)
        .expect("png");
    let drawn = image::load_from_memory(&badge::draw(&poster, "LEAVES OCT 23").expect("drawn")).expect("jpeg").to_rgb8();
    assert_eq!(drawn.dimensions(), (200, 300));
    assert!(drawn.get_pixel(2, 298).0[0] > 120, "red band at the bottom");
    assert!(drawn.get_pixel(2, 2).0[0] < 30, "the top is untouched");
}
