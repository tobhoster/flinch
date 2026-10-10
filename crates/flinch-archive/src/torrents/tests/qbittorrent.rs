//! qBittorrent over a real socket: the login cookie travels back, limits read
//! as the effective ones, and a removal is posted and read back.

use super::super::{ClientConfig, ClientKind, Holding, TorrentError, Torrents, TorrentsConfig, UNBOUNDED_RATIO};
use super::fake::{client, reply, serve};

const HASH: &str = "8c212779b4abde7c6bc608063a0d008b7e40ce32";

fn config(base: &str, password_env: Option<&str>) -> TorrentsConfig {
    TorrentsConfig {
        clients: vec![ClientConfig {
            kind: ClientKind::Qbittorrent,
            url: base.to_string(),
            username: "admin".into(),
            password_env: password_env.map(Into::into),
        }],
        ..TorrentsConfig::default()
    }
}

fn holding() -> Holding {
    Holding {
        hash: HASH.into(),
        client: ClientKind::Qbittorrent,
        client_index: 0,
        ratio: 2.0,
        seeding_secs: 0,
        meets_goal: true,
        hardlinked_paths: Vec::new(),
        links_verified: true,
        cards: vec!["radarr-1".into()],
    }
}

const LOGGED_IN: &str = "Ok.";

#[tokio::test]
async fn the_login_cookie_is_sent_back_and_effective_limits_decide() {
    std::env::set_var("FLINCH_TEST_QBIT_PW_A", "s3cret");
    let listing = r#"[
        {"hash":"8C212779B4ABDE7C6BC608063A0D008B7E40CE32","name":"Movie","ratio":2.1,"seeding_time":600,"max_ratio":2.0,"max_seeding_time":-1,"progress":1,"save_path":"/dl","content_path":"/dl/Movie"},
        {"hash":"bb","name":"Show","ratio":0.3,"seeding_time":3600,"max_ratio":-1,"max_seeding_time":60,"progress":1,"save_path":"/dl","content_path":"/dl/Show"},
        {"hash":"cc","name":"Rare","ratio":-1,"seeding_time":10,"max_ratio":-1,"max_seeding_time":-1,"progress":0.5,"save_path":"/dl/","content_path":""}
    ]"#;
    let (base, server) =
        serve(vec![reply(200, LOGGED_IN).with("Set-Cookie", "QBT_SID_8080=abc123; HttpOnly; path=/"), reply(200, listing)]);
    let listings = Torrents::new(&client(), &config(&base, Some("FLINCH_TEST_QBIT_PW_A")), false).list().await;
    let seen = server.join().expect("fake");

    assert_eq!(seen[0].line, "POST /api/v2/auth/login HTTP/1.1");
    assert_eq!(seen[0].body, "username=admin&password=s3cret");
    assert!(seen[0].head.contains(&format!("referer: {base}")), "{}", seen[0].head);
    assert_eq!(seen[1].line, "GET /api/v2/torrents/info HTTP/1.1");
    assert!(seen[1].head.contains("cookie: qbt_sid_8080=abc123"), "{}", seen[1].head);
    let torrents = listings.into_iter().next().expect("one client").expect("listed");
    assert_eq!(torrents[0].hash, HASH, "hashes are lower-cased");
    assert!(torrents[0].limit_reached, "ratio past the effective max_ratio");
    assert!(torrents[1].limit_reached, "an hour against a 60-minute max_seeding_time");
    assert!(!torrents[2].limit_reached && !torrents[2].complete);
    assert_eq!(torrents[2].ratio, UNBOUNDED_RATIO, "-1 is qBittorrent's ratio above 9999");
    assert_eq!(torrents[2].content_path, "/dl/Rare", "an old server without content_path joins save_path and name");
}

#[rstest::rstest]
#[case::old_server_says_fails(reply(200, "Fails."))]
#[case::new_server_says_unauthorized(reply(401, ""))]
#[tokio::test]
async fn a_refused_login_is_a_login_error(#[case] answer: super::fake::Reply) {
    std::env::set_var("FLINCH_TEST_QBIT_PW_B", "wrong");
    let (base, server) = serve(vec![answer]);
    let listings = Torrents::new(&client(), &config(&base, Some("FLINCH_TEST_QBIT_PW_B")), false).list().await;
    server.join().expect("fake");
    assert!(matches!(listings[0], Err(TorrentError::Login { client: ClientKind::Qbittorrent })), "{:?}", listings[0]);
}

#[tokio::test]
async fn a_lapsed_session_logs_in_once_more_and_retries() {
    std::env::set_var("FLINCH_TEST_QBIT_PW_C", "pw");
    let (base, server) = serve(vec![
        reply(200, LOGGED_IN).with("Set-Cookie", "SID=old"),
        reply(403, "Forbidden"),
        reply(200, LOGGED_IN).with("Set-Cookie", "SID=new"),
        reply(200, "[]"),
    ]);
    let listings = Torrents::new(&client(), &config(&base, Some("FLINCH_TEST_QBIT_PW_C")), false).list().await;
    let seen = server.join().expect("fake");
    assert!(matches!(&listings[0], Ok(torrents) if torrents.is_empty()), "{:?}", listings[0]);
    assert!(seen[3].head.contains("cookie: sid=new"), "{}", seen[3].head);
}

#[tokio::test]
async fn an_unset_password_variable_sends_nothing() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let listings = Torrents::new(&client(), &config(&base, Some("FLINCH_TEST_QBIT_UNSET")), false).list().await;
    assert!(matches!(&listings[0], Err(TorrentError::MissingSecret { var, .. }) if var == "FLINCH_TEST_QBIT_UNSET"));
    listener.set_nonblocking(true).expect("pollable");
    assert!(listener.accept().is_err(), "no request may leave without its password");
}

#[tokio::test]
async fn a_removal_is_posted_and_confirmed_by_reading_back() {
    // No credentials: a client whitelisted for the daemon's address.
    let (base, server) = serve(vec![reply(200, ""), reply(200, "[]")]);
    let mut config = config(&base, None);
    config.clients[0].username.clear();
    let removed = Torrents::new(&client(), &config, false).remove(&holding(), true).await;
    let seen = server.join().expect("fake");
    assert!(removed.is_ok(), "{removed:?}");
    assert_eq!(seen[0].line, "POST /api/v2/torrents/delete HTTP/1.1");
    assert_eq!(seen[0].body, format!("hashes={HASH}&deleteFiles=true"));
    assert_eq!(seen[1].line, format!("GET /api/v2/torrents/info?hashes={HASH} HTTP/1.1"));
}

#[tokio::test]
async fn a_torrent_still_listed_after_removal_is_not_applied() {
    let still = format!(r#"[{{"hash":"{HASH}","name":"Movie","ratio":2,"progress":1}}]"#);
    let (base, server) = serve(vec![reply(200, ""), reply(200, &still), reply(200, &still), reply(200, &still)]);
    let mut config = config(&base, None);
    config.clients[0].username.clear();
    let removed = Torrents::new(&client(), &config, false).remove(&holding(), false).await;
    server.join().expect("fake");
    assert!(matches!(removed, Err(TorrentError::NotApplied { .. })), "{removed:?}");
}

#[tokio::test]
async fn a_dry_run_removal_sends_nothing() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let removed = Torrents::new(&client(), &config(&base, None), true).remove(&holding(), true).await;
    assert!(removed.is_ok());
    listener.set_nonblocking(true).expect("pollable");
    assert!(listener.accept().is_err(), "a dry run never reaches the client");
}

#[tokio::test]
async fn files_are_absolute_under_the_save_path() {
    let info = format!(r#"[{{"hash":"{HASH}","name":"Show","save_path":"/dl/tv/","progress":1}}]"#);
    let files = r#"[{"index":0,"name":"Show/S01E01.mkv","size":1},{"index":1,"name":"Show/S01E02.mkv","size":1}]"#;
    let (base, server) = serve(vec![reply(200, &info), reply(200, files)]);
    let mut config = config(&base, None);
    config.clients[0].username.clear();
    let listed = Torrents::new(&client(), &config, false).files(0, HASH).await.expect("files");
    let seen = server.join().expect("fake");
    assert_eq!(listed, ["/dl/tv/Show/S01E01.mkv", "/dl/tv/Show/S01E02.mkv"]);
    assert_eq!(seen[1].line, format!("GET /api/v2/torrents/files?hash={HASH} HTTP/1.1"));
}
