//! Delivery over a real socket: each event reaches a channel once, within its
//! hourly budget, in that channel's documented shape, and no failure names the
//! URL that carries the channel's secret.

use super::{
    answer_test, ChannelConfig, ChannelKind, Digest, DiskLine, Event, EventKind, Notifier, NotifyConfig, Stage, TestRequest, TestResult,
    TopCandidate, TEST_REQUEST_FILE, TEST_RESULT_FILE,
};
use rstest::rstest;
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::thread::JoinHandle;

mod household;

const NOW: u64 = 1_790_000_000;
const UI: &str = "https://flinch.example.com/";

/// One request the fake server took: path, headers (lower-cased), JSON body.
struct Taken {
    path: String,
    headers: String,
    body: Value,
}

/// A server that answers `statuses` in turn, one connection each, then hands
/// its listener back so a test can prove nothing else connected.
fn serve(statuses: Vec<u16>) -> (String, JoinHandle<(Vec<Taken>, TcpListener)>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the fake channel");
    let base = format!("http://{}", listener.local_addr().expect("fake address"));
    let thread = std::thread::spawn(move || {
        let mut taken = Vec::new();
        for status in statuses {
            let (mut stream, _) = listener.accept().expect("the client connects");
            let mut raw = Vec::new();
            let mut chunk = [0u8; 4096];
            let head_end = loop {
                let read = stream.read(&mut chunk).expect("the request arrives");
                raw.extend_from_slice(&chunk[..read]);
                if let Some(end) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let head = String::from_utf8_lossy(&raw[..head_end]).to_lowercase();
            let length: usize =
                head.lines().find_map(|line| line.strip_prefix("content-length:")).and_then(|value| value.trim().parse().ok()).unwrap_or(0);
            while raw.len() < head_end + length {
                let read = stream.read(&mut chunk).expect("the body arrives");
                raw.extend_from_slice(&chunk[..read]);
            }
            let path = head.split_whitespace().nth(1).unwrap_or_default().to_string();
            let body = serde_json::from_slice(&raw[head_end..head_end + length]).unwrap_or(Value::Null);
            write!(stream, "HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").expect("the answer is sent");
            taken.push(Taken { path, headers: head, body });
        }
        (taken, listener)
    });
    (base, thread)
}

fn finished(thread: JoinHandle<(Vec<Taken>, TcpListener)>) -> Vec<Taken> {
    let (taken, listener) = thread.join().expect("the fake channel finishes");
    listener.set_nonblocking(true).expect("a listener that can be polled");
    assert!(listener.accept().is_err(), "no request beyond the ones answered");
    taken
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().expect("client")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flinch-notify-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn config(kind: ChannelKind, url: String, max_per_hour: u32) -> NotifyConfig {
    let channel = ChannelConfig { name: "home".to_string(), kind, url, ..ChannelConfig::default() };
    NotifyConfig { channels: vec![channel], ui_url: UI.to_string(), max_per_hour, ..NotifyConfig::default() }
}

fn leaving(id: &str, handed_at: u64) -> Event {
    Event::LeavingSoon {
        id: id.to_string(),
        title: format!("Title {id}"),
        bytes: 4 << 30,
        handed_at,
        leaves_at: Some(NOW + 7 * 86_400),
        requesters: Vec::new(),
        poster: None,
        keep_url: None,
    }
}

fn problem() -> Event {
    Event::Problem { key: "plex".to_string(), message: "Plex unreadable".to_string(), since: NOW - 7_200 }
}

#[tokio::test]
async fn an_event_reaches_a_channel_once_and_a_new_hand_over_is_told_again() {
    let dir = scratch("dedupe");
    let (base, server) = serve(vec![204, 204]);
    let config = config(ChannelKind::Discord, format!("{base}/hook"), 12);
    let http = client();
    let notifier = Notifier::new(&http, &config, &dir);

    let first = notifier.send_at(&[leaving("radarr-1", NOW)], NOW).await;
    let again = notifier.send_at(&[leaving("radarr-1", NOW)], NOW + 3_600).await;
    let rehanded = notifier.send_at(&[leaving("radarr-1", NOW + 86_400)], NOW + 86_400).await;
    let taken = finished(server);

    assert_eq!((first.messages, again.messages, rehanded.messages), (1, 0, 1));
    assert_eq!(taken.len(), 2);
}

#[tokio::test]
async fn past_the_hourly_budget_events_wait_for_a_later_cycle_instead_of_being_dropped() {
    let dir = scratch("budget");
    let (base, server) = serve(vec![204, 204]);
    let config = config(ChannelKind::Apprise, format!("{base}/notify/flinch"), 1);
    let http = client();
    let notifier = Notifier::new(&http, &config, &dir);
    let events = [problem(), leaving("radarr-1", NOW), leaving("radarr-2", NOW)];

    let first = notifier.send_at(&events, NOW).await;
    let same_hour = notifier.send_at(&events, NOW + 600).await;
    let next_hour = notifier.send_at(&events, NOW + 3_600).await;
    let taken = finished(server);

    assert_eq!((first.messages, first.deferred), (1, 2), "the problem goes first, both Leaving Soon titles wait");
    assert_eq!((same_hour.messages, same_hour.deferred), (0, 2));
    assert_eq!((next_hour.messages, next_hour.deferred), (1, 0));
    assert_eq!(taken[0].body["type"], "failure");
    assert_eq!(taken[1].body["type"], "warning");
}

#[tokio::test]
async fn a_failed_post_is_retried_next_cycle_and_its_error_never_names_the_url() {
    let dir = scratch("failure");
    let (base, server) = serve(vec![500, 204]);
    let config = config(ChannelKind::Discord, format!("{base}/api/webhooks/1/SECRET-TOKEN"), 12);
    let http = client();
    let notifier = Notifier::new(&http, &config, &dir);

    let failed = notifier.send_at(&[problem()], NOW).await;
    let retried = notifier.send_at(&[problem()], NOW + 60).await;
    finished(server);

    assert_eq!(failed.messages, 0);
    assert_eq!(failed.failures.len(), 1);
    assert!(failed.failures[0].contains("HTTP 500"), "{:?}", failed.failures);
    assert!(!failed.failures[0].contains("SECRET"), "{:?}", failed.failures);
    assert_eq!(retried.messages, 1);
}

#[tokio::test]
async fn a_redirect_is_a_failure_and_is_never_followed() {
    let dir = scratch("redirect");
    let elsewhere = TcpListener::bind("127.0.0.1:0").expect("bind the redirect target");
    elsewhere.set_nonblocking(true).expect("a target that can be polled");
    let (base, server) = serve(vec![302]);
    let config = config(ChannelKind::Webhook, format!("{base}/hook"), 12);
    let http = client();

    let report = Notifier::new(&http, &config, &dir).send_at(&[problem()], NOW).await;
    finished(server);

    assert!(report.failures[0].contains("HTTP 302"), "{:?}", report.failures);
    assert!(elsewhere.accept().is_err());
}

#[rstest]
#[case::discord(ChannelKind::Discord, "/api/webhooks/1/abc")]
#[case::ntfy(ChannelKind::Ntfy, "/flinch")]
#[case::apprise(ChannelKind::Apprise, "/notify/flinch")]
#[case::webhook(ChannelKind::Webhook, "/hook")]
#[tokio::test]
async fn each_channel_gets_its_documented_shape_with_the_keep_link(#[case] kind: ChannelKind, #[case] path: &str) {
    let dir = scratch(&format!("shape-{kind:?}"));
    let (base, server) = serve(vec![200]);
    let config = config(kind, format!("{base}{path}"), 12);
    let http = client();

    let report = Notifier::new(&http, &config, &dir).send_at(&[leaving("sonarr-7-s2", NOW)], NOW).await;
    let taken = finished(server);

    assert_eq!(report.messages, 1, "{:?}", report.failures);
    let keep = "https://flinch.example.com/?item=sonarr-7-s2";
    let body = &taken[0].body;
    match kind {
        ChannelKind::Discord => {
            assert_eq!(taken[0].path, path);
            assert_eq!(body["allowed_mentions"]["parse"], serde_json::json!([]));
            assert_eq!(body["embeds"][0]["title"], "Leaving soon: 1 title(s)");
            assert!(body["embeds"][0]["description"].as_str().unwrap_or_default().contains(keep));
        }
        ChannelKind::Ntfy => {
            assert_eq!(taken[0].path, "/", "ntfy takes JSON at its root");
            assert_eq!(body["topic"], "flinch");
            assert_eq!(body["actions"][0]["action"], "view");
            assert_eq!(body["actions"][0]["url"], keep);
            assert_eq!(body["markdown"], true);
        }
        ChannelKind::Apprise => {
            assert_eq!(taken[0].path, path);
            assert_eq!(body["format"], "markdown");
            assert!(body["body"].as_str().unwrap_or_default().contains(keep));
        }
        ChannelKind::Webhook => {
            assert_eq!(body["source"], "flinch");
            assert_eq!(body["events"][0]["event"], "leaving_soon");
            assert_eq!(body["events"][0]["keep_url"], keep);
        }
    }
}

#[tokio::test]
async fn a_channel_gets_only_the_kinds_it_subscribed_to_and_its_bearer_token() {
    let dir = scratch("subscribed");
    let (base, server) = serve(vec![200]);
    let mut config = config(ChannelKind::Webhook, format!("{base}/hook"), 12);
    config.channels[0].events = vec![EventKind::Digest];
    // PATH is set in every test environment; its value stands in for a token.
    config.channels[0].token_env = "PATH".to_string();
    let digest = Event::Digest(Digest {
        day: NOW / 86_400,
        dry_run: true,
        disks: vec![DiskLine {
            volume: "/media".to_string(),
            used_bytes: 80,
            capacity_bytes: 100,
            projected_used_bytes: 95,
            window_days: 30,
            target_reclaim_bytes: 10,
            emergency: false,
        }],
        freed_items: 2,
        freed_bytes: 3 << 30,
        top: vec![TopCandidate { id: "radarr-1".to_string(), title: "Old".to_string(), bytes: 1 << 30, regret: 0.1 }],
    });
    let http = client();

    let report = Notifier::new(&http, &config, &dir).send_at(&[problem(), digest], NOW).await;
    let taken = finished(server);

    assert_eq!(report.messages, 1);
    assert_eq!(taken[0].body["events"].as_array().map(Vec::len), Some(1));
    assert_eq!(taken[0].body["events"][0]["event"], "digest");
    assert!(taken[0].headers.contains("authorization: bearer "));
}

#[test]
fn a_problem_is_told_on_its_third_cycle_and_a_failed_cycle_neither_counts_nor_breaks_the_run() {
    let dir = scratch("streaks");
    let http = client();
    let config = config(ChannelKind::Webhook, "http://127.0.0.1:9/hook".to_string(), 12);
    let notifier = Notifier::new(&http, &config, &dir);
    let plex = [("plex".to_string(), "Plex unreadable".to_string())];
    let failed = [("cycle".to_string(), "cycle failed".to_string())];

    let one = notifier.problems(&plex, true, NOW).expect("streaks");
    let two = notifier.problems(&failed, false, NOW + 1).expect("streaks");
    let three = notifier.problems(&plex, true, NOW + 2).expect("streaks");
    let four = notifier.problems(&plex, true, NOW + 3).expect("streaks");
    let five = notifier.problems(&plex, true, NOW + 4).expect("streaks");
    let cleared = notifier.problems(&[], true, NOW + 5).expect("streaks");
    let back = notifier.problems(&plex, true, NOW + 6).expect("streaks");

    assert!(one.is_empty() && two.is_empty() && three.is_empty(), "the failed cycle neither counts nor breaks the run");
    assert_eq!(four, vec![Event::Problem { key: "plex".to_string(), message: "Plex unreadable".to_string(), since: NOW }]);
    assert_eq!(five, four, "still persisting: the same key, deduplicated at send");
    assert!(cleared.is_empty() && back.is_empty(), "a cleared problem starts its run over");
}

#[tokio::test]
async fn a_test_request_posts_to_every_channel_and_the_answer_is_written_for_the_page() {
    let dir = scratch("test-send");
    let (base, server) = serve(vec![204]);
    let mut settings = crate::daemon::RuntimeSettings {
        notify: config(ChannelKind::Ntfy, format!("{base}/flinch"), 1),
        ..crate::daemon::RuntimeSettings::default()
    };
    settings.notify.channels.push(ChannelConfig {
        name: "unset".to_string(),
        kind: ChannelKind::Discord,
        url_env: "FLINCH_TEST_SURELY_UNSET_URL".to_string(),
        ..ChannelConfig::default()
    });
    crate::daemon::write_settings(&dir.join("settings.json"), &settings).expect("settings");
    let request = TestRequest { id: "abc".to_string(), requested_at: NOW };
    std::fs::write(dir.join(TEST_REQUEST_FILE), serde_json::to_vec(&request).expect("request")).expect("request file");
    let http = client();

    let answered = answer_test(&http, &dir).await;
    let taken = finished(server);
    let again = answer_test(&http, &dir).await;

    let written: TestResult = serde_json::from_slice(&std::fs::read(dir.join(TEST_RESULT_FILE)).expect("result file")).expect("result");
    assert_eq!(answered.as_ref(), Some(&written));
    assert_eq!(written.id, "abc");
    assert!(written.channels[0].ok, "{:?}", written.channels);
    assert_eq!(written.channels[1].detail, "environment variable FLINCH_TEST_SURELY_UNSET_URL is not set");
    assert_eq!(taken[0].body["title"], "FLINCH test notification");
    assert_eq!(taken[0].body["topic"], "flinch");
    assert!(again.is_none(), "a request is answered once");
}

#[rstest]
#[case::both_urls(r#"{"name":"a","kind":"webhook","url":"http://x/h","url_env":"X","events":["digest"]}"#)]
#[case::neither_url(r#"{"name":"a","kind":"webhook","events":["digest"]}"#)]
#[case::bad_env_name(r#"{"name":"a","kind":"webhook","url_env":"x-y","events":["digest"]}"#)]
#[case::ntfy_without_topic(r#"{"name":"a","kind":"ntfy","url":"https://ntfy.sh/","events":["digest"]}"#)]
#[case::token_on_discord(r#"{"name":"a","kind":"discord","url_env":"D","token_env":"T","events":["digest"]}"#)]
#[case::no_events(r#"{"name":"a","kind":"webhook","url":"http://x/h","events":[]}"#)]
fn a_channel_the_page_would_refuse_is_refused(#[case] channel: &str) {
    let config: NotifyConfig = serde_json::from_str(&format!(r#"{{"channels":[{channel}]}}"#)).expect("parses");
    assert!(config.validate().is_err());
}

#[test]
fn two_channels_with_one_name_are_refused() {
    let channel = ChannelConfig { name: "a".to_string(), url: "http://x/h".to_string(), ..ChannelConfig::default() };
    let config = NotifyConfig { channels: vec![channel.clone(), channel], ..NotifyConfig::default() };
    assert!(config.validate().is_err());
}

#[rstest]
#[case(0, "1970-01-01")]
#[case(19_723, "2024-01-01")]
#[case(20_735, "2026-10-09")]
fn utc_days_read_as_dates(#[case] day: u64, #[case] date: &str) {
    assert_eq!(super::utc_date(day), date);
}

#[test]
fn a_deletion_and_its_hand_over_are_told_apart() {
    let deleted = |stage| Event::Deleted { id: "radarr-1".to_string(), title: "T".to_string(), bytes: 1, handed_at: NOW, stage };
    assert_ne!(deleted(Stage::Handed).key(), deleted(Stage::Gone).key());
}
