use super::*;
use rstest::rstest;
use serde_json::json;

fn answer() -> serde_json::Value {
    json!({"id": 550, "results": {
        "DE": {"link": "https://www.themoviedb.org/movie/550/watch?locale=DE",
               "flatrate": [{"provider_id": 337, "provider_name": "Disney Plus", "display_priority": 1},
                            {"provider_id": "x"},
                            {"provider_id": 8, "provider_name": "Netflix", "display_priority": 2}],
               "rent": [{"provider_id": 2, "provider_name": "Apple TV"}]},
        "US": {"buy": [{"provider_id": 2, "provider_name": "Apple TV"}]}}})
}

fn looked(at: u64, region: &str, ids: &[(u32, &str)]) -> Looked {
    Looked {
        looked_at: at,
        region: region.into(),
        flatrate: ids.iter().map(|(id, name)| Provider { id: *id, name: (*name).into() }).collect(),
    }
}

#[rstest]
#[case::flatrate_in_order_malformed_skipped("DE", Some(vec![337, 8]))]
#[case::rent_or_buy_only_streams_nowhere("US", Some(vec![]))]
#[case::region_absent_streams_nowhere("FR", Some(vec![]))]
fn flatrate_providers_of_the_region(#[case] region: &str, #[case] ids: Option<Vec<u32>>) {
    let parsed = parse_flatrate(&answer(), region).map(|providers| providers.iter().map(|p| p.id).collect::<Vec<_>>());
    assert_eq!(parsed, ids);
}

#[test]
fn an_answer_without_results_is_unreadable() {
    assert_eq!(parse_flatrate(&json!({"status_message": "Invalid API key"}), "DE"), None);
}

#[test]
fn due_puts_unseen_first_then_stale_oldest_and_honours_the_region() {
    let now = 100 * 86_400;
    let mut cache = StreamingCache::default();
    cache.insert(Title::Movie(1), looked(now - STREAMING_TTL_SECS - 10, "DE", &[]));
    cache.insert(Title::Movie(2), looked(now - STREAMING_TTL_SECS - 99, "DE", &[]));
    cache.insert(Title::Movie(3), looked(now - 60, "DE", &[]));
    cache.insert(Title::Tv(4), looked(now - 60, "US", &[]));
    let titles = [Title::Movie(1), Title::Movie(2), Title::Movie(3), Title::Tv(4), Title::Tv(5), Title::Tv(5)];
    assert_eq!(cache.due(&titles, "DE", now), vec![Title::Tv(4), Title::Tv(5), Title::Movie(2), Title::Movie(1)]);
}

#[test]
fn due_is_capped_per_cycle() {
    let titles: Vec<Title> = (1..=100).map(Title::Movie).collect();
    assert_eq!(StreamingCache::default().due(&titles, "DE", 0).len(), LOOKUP_BUDGET);
}

#[test]
fn streams_names_the_first_subscribed_provider_of_the_region_only() {
    let mut cache = StreamingCache::default();
    cache.insert(Title::Movie(1), looked(0, "DE", &[(337, "Disney Plus"), (8, "Netflix")]));
    cache.insert(Title::Tv(2), looked(0, "DE", &[(9, "Prime Video")]));
    cache.insert(Title::Tv(3), looked(0, "US", &[(8, "Netflix")]));
    let streams = cache.streams("DE", &[8]);
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[&Title::Movie(1)].note(), "streams on Netflix (DE)");
    cache.retain(&[Title::Tv(2)]);
    assert_eq!(cache.known("DE"), 1);
}

#[rstest]
#[case::off_by_default(StreamingConfig::default(), true)]
#[case::on(StreamingConfig { enabled: true, region: "DE".into(), provider_ids: vec![8], ..StreamingConfig::default() }, true)]
#[case::lowercase_region(StreamingConfig { enabled: true, region: "de".into(), provider_ids: vec![8], ..StreamingConfig::default() }, false)]
#[case::no_provider(StreamingConfig { enabled: true, region: "DE".into(), ..StreamingConfig::default() }, false)]
#[case::bad_env(StreamingConfig { tmdb_key_env: "tmdb key".into(), ..StreamingConfig::default() }, false)]
fn validation(#[case] config: StreamingConfig, #[case] ok: bool) {
    assert_eq!(config.validate().is_ok(), ok);
}

/// A server answering each connection with the next `(status, body)`,
/// returning the request lines it saw.
fn serve(script: Vec<(u16, &'static str)>) -> (String, std::thread::JoinHandle<Vec<String>>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let handle = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for (status, body) in script {
            let (mut stream, _) = listener.accept().expect("connect");
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).expect("read");
                assert!(read > 0);
                request.extend_from_slice(&buffer[..read]);
            }
            seen.push(String::from_utf8_lossy(&request).to_string());
            write!(stream, "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).expect("write");
        }
        seen
    });
    (base, handle)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().expect("client")
}

const NETFLIX: &str = r#"{"id":1,"results":{"DE":{"flatrate":[{"provider_id":8,"provider_name":"Netflix"}]}}}"#;

#[tokio::test]
async fn refresh_stores_lookups_treats_404_as_nowhere_and_stops_at_a_failure() {
    let (base, server) = serve(vec![(200, NETFLIX), (404, r#"{"status_code":34}"#), (401, "{}")]);
    let mut cache = StreamingCache::default();
    let titles = [Title::Movie(1), Title::Tv(2), Title::Movie(3), Title::Movie(4)];
    let refreshed = tmdb::refresh(&client(), &base, "v3key", "DE", &mut cache, &titles, 1_000).await;
    assert_eq!(refreshed.looked_up, 2);
    assert!(matches!(refreshed.problem, Some(tmdb::TmdbError::Status(401))));
    assert_eq!(cache.known("DE"), 2, "the failed and untried titles stay unknown");
    assert_eq!(cache.streams("DE", &[8]).get(&Title::Movie(1)).map(Stream::note).as_deref(), Some("streams on Netflix (DE)"));
    let seen = server.join().expect("server");
    assert!(seen[0].starts_with("GET /3/movie/1/watch/providers?api_key=v3key "), "{}", seen[0]);
    assert!(seen[1].starts_with("GET /3/tv/2/watch/providers"), "{}", seen[1]);
    let message = refreshed.problem.map(|error| error.to_string()).unwrap_or_default();
    assert!(!message.contains("v3key"));
}

#[rstest]
#[case::redirect_not_followed(302, "")]
#[case::unreadable(200, "not json")]
#[tokio::test]
async fn a_bad_answer_gives_no_providers(#[case] status: u16, #[case] body: &'static str) {
    let (base, server) = serve(vec![(status, body)]);
    let result = tmdb::flatrate(&client(), &base, "a.b.c", Title::Movie(1), "DE").await;
    assert!(result.is_err());
    let seen = server.join().expect("server").remove(0);
    assert!(seen.to_ascii_lowercase().contains("authorization: bearer a.b.c"), "a v4 token travels as a bearer header");
    assert!(!seen.contains("api_key"));
}
