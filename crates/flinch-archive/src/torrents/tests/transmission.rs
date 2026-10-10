//! Transmission over a real socket: the session-id handshake, the dialect its
//! 409 names, basic authentication, and a confirmed removal.

use super::super::{ClientConfig, ClientKind, Holding, TorrentError, Torrents, TorrentsConfig, UNBOUNDED_RATIO};
use super::fake::{client, reply, serve};

const HASH: &str = "54eddd830a5b58480a6143d616a97e3a6c23c439";

fn config(url: String, username: &str) -> TorrentsConfig {
    TorrentsConfig {
        clients: vec![ClientConfig { kind: ClientKind::Transmission, url, username: username.into(), password_env: None }],
        ..TorrentsConfig::default()
    }
}

fn conflict(version: Option<&str>) -> super::fake::Reply {
    let answer = reply(409, "").with("X-Transmission-Session-Id", "sess-1");
    match version {
        Some(version) => answer.with("X-Transmission-Rpc-Version", version),
        None => answer,
    }
}

#[tokio::test]
async fn an_older_server_is_asked_in_the_bespoke_dialect_after_the_handshake() {
    let listing = format!(
        r#"{{"result":"success","arguments":{{"torrents":[
            {{"hashString":"{HASH}","name":"Show S01","uploadRatio":0.4,"secondsSeeding":7200,"isFinished":false,"percentDone":1,"downloadDir":"/dl/tv"}},
            {{"hashString":"bb","name":"Movie","uploadRatio":-2,"secondsSeeding":5,"isFinished":true,"percentDone":1,"downloadDir":"/dl"}}
        ]}}}}"#
    );
    let (base, server) = serve(vec![conflict(None), reply(200, &listing)]);
    let listings = Torrents::new(&client(), &config(base, "tr"), false).list().await;
    let seen = server.join().expect("fake");

    assert_eq!(seen[0].line, "POST /transmission/rpc HTTP/1.1", "a server root gets the default RPC path");
    assert!(seen[1].head.contains("x-transmission-session-id: sess-1"), "{}", seen[1].head);
    assert!(seen[1].head.contains("authorization: basic"), "{}", seen[1].head);
    let body: serde_json::Value = serde_json::from_str(&seen[1].body).expect("json request");
    assert_eq!(body["method"], "torrent-get");
    assert!(body["arguments"]["fields"].as_array().expect("fields").iter().any(|field| field == "hashString"));
    let torrents = listings.into_iter().next().expect("one client").expect("listed");
    assert_eq!(torrents[0].hash, HASH);
    assert_eq!((torrents[0].ratio, torrents[0].seeding_secs, torrents[0].limit_reached), (0.4, 7200, false));
    assert_eq!(torrents[0].content_path, "/dl/tv/Show S01");
    assert!(torrents[1].limit_reached, "isFinished: its own seed limit is reached");
    assert_eq!(torrents[1].ratio, UNBOUNDED_RATIO, "-2 is Transmission's infinite ratio");
}

#[tokio::test]
async fn rpc_six_is_spoken_as_json_rpc_with_snake_case_keys() {
    let listing = format!(
        r#"{{"jsonrpc":"2.0","result":{{"torrents":[{{"hash_string":"{HASH}","name":"M","upload_ratio":1.5,"seconds_seeding":60,"is_finished":false,"percent_done":1,"download_dir":"/dl"}}]}},"id":1}}"#
    );
    let (base, server) = serve(vec![conflict(Some("6.0.1")), reply(200, &listing)]);
    let listings = Torrents::new(&client(), &config(format!("{base}/transmission/rpc"), ""), false).list().await;
    let seen = server.join().expect("fake");

    let body: serde_json::Value = serde_json::from_str(&seen[1].body).expect("json request");
    assert_eq!((body["jsonrpc"].as_str(), body["method"].as_str()), (Some("2.0"), Some("torrent_get")));
    assert!(body["params"]["fields"].as_array().expect("fields").iter().any(|field| field == "hash_string"));
    assert!(!seen[1].head.contains("authorization:"), "no credentials configured, none sent");
    let torrents = listings.into_iter().next().expect("one client").expect("listed");
    assert_eq!((torrents[0].hash.as_str(), torrents[0].ratio), (HASH, 1.5));
}

#[tokio::test]
async fn a_failed_call_names_the_servers_answer() {
    let (base, server) = serve(vec![conflict(None), reply(200, r#"{"result":"method name not recognized","arguments":{}}"#)]);
    let listings = Torrents::new(&client(), &config(base, ""), false).list().await;
    server.join().expect("fake");
    assert!(
        matches!(&listings[0], Err(TorrentError::Parse { detail, .. }) if detail.contains("method name not recognized")),
        "{:?}",
        listings[0]
    );
}

#[tokio::test]
async fn a_removal_deletes_local_data_and_is_read_back() {
    let (base, server) = serve(vec![
        conflict(None),
        reply(200, r#"{"result":"success","arguments":{}}"#),
        reply(200, r#"{"result":"success","arguments":{"torrents":[]}}"#),
    ]);
    let holding = Holding {
        hash: HASH.into(),
        client: ClientKind::Transmission,
        client_index: 0,
        ratio: 1.0,
        seeding_secs: 0,
        meets_goal: true,
        hardlinked_paths: Vec::new(),
        links_verified: true,
        cards: vec!["sonarr-1-s1".into()],
    };
    let removed = Torrents::new(&client(), &config(base, ""), false).remove(&holding, true).await;
    let seen = server.join().expect("fake");
    assert!(removed.is_ok(), "{removed:?}");
    let body: serde_json::Value = serde_json::from_str(&seen[1].body).expect("json request");
    assert_eq!(body["method"], "torrent-remove");
    assert_eq!(body["arguments"]["ids"][0], HASH);
    assert_eq!(body["arguments"]["delete-local-data"], true);
    let read_back: serde_json::Value = serde_json::from_str(&seen[2].body).expect("json request");
    assert_eq!(read_back["arguments"]["ids"][0], HASH);
}

#[tokio::test]
async fn files_join_the_download_dir() {
    let answer = format!(
        r#"{{"result":"success","arguments":{{"torrents":[{{"hashString":"{HASH}","downloadDir":"/dl/tv","files":[{{"name":"Show/e1.mkv","length":1}}]}}]}}}}"#
    );
    let (base, server) = serve(vec![conflict(None), reply(200, &answer)]);
    let files = Torrents::new(&client(), &config(base, ""), false).files(0, HASH).await.expect("files");
    server.join().expect("fake");
    assert_eq!(files, ["/dl/tv/Show/e1.mkv"]);
}
