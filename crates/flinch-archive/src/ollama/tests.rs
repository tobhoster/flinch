//! The Ollama client against a scripted fake server over a real socket.

use super::*;
use rstest::rstest;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::JoinHandle;

/// One request as the fake saw it: its head (request line and headers,
/// lowercased) and its body.
struct Seen {
    head: String,
    body: serde_json::Value,
}

/// Serves `(status, body)` in order, one connection each.
fn fake(script: Vec<(u16, &'static str)>) -> (String, JoinHandle<Vec<Seen>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the fake Ollama");
    let base = format!("http://{}", listener.local_addr().expect("fake address"));
    let handle = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for (status, body) in script {
            let (mut stream, _) = listener.accept().expect("the client connects");
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            let head_end = loop {
                if let Some(at) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    break at + 4;
                }
                let read = stream.read(&mut buffer).expect("the request arrives");
                assert!(read > 0, "the request ended before its head");
                request.extend_from_slice(&buffer[..read]);
            };
            let head = String::from_utf8_lossy(&request[..head_end]).to_lowercase();
            let length: usize =
                head.lines().find_map(|line| line.strip_prefix("content-length:")).and_then(|value| value.trim().parse().ok()).unwrap_or(0);
            while request.len() < head_end + length {
                let read = stream.read(&mut buffer).expect("the body arrives");
                request.extend_from_slice(&buffer[..read]);
            }
            let sent = serde_json::from_slice(&request[head_end..]).unwrap_or(serde_json::Value::Null);
            seen.push(Seen { head, body: sent });
            write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .expect("the answer is sent");
        }
        seen
    });
    (base, handle)
}

fn config(url: &str, token: &str) -> OllamaConfig {
    OllamaConfig { url: url.to_string(), token: token.to_string(), api_key_env: String::new() }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().expect("client")
}

#[tokio::test]
async fn texts_are_embedded_in_order_with_the_key_as_a_bearer_token() {
    let (base, server) = fake(vec![(200, r#"{"model":"embeddinggemma","embeddings":[[0.1,0.2],[0.3,0.4]]}"#)]);
    let vectors = embed(&client(), &config(&format!("{base}/"), "k3y"), "embeddinggemma", &["a", "b"]).await.expect("embedded");
    assert_eq!(vectors, vec![vec![0.1, 0.2], vec![0.3, 0.4]]);
    let seen = server.join().expect("fake");
    assert!(seen[0].head.starts_with("post /api/embed "), "{}", seen[0].head);
    assert!(seen[0].head.contains("authorization: bearer k3y"), "{}", seen[0].head);
    assert_eq!(seen[0].body, serde_json::json!({"model": "embeddinggemma", "input": ["a", "b"], "truncate": true}));
}

#[tokio::test]
async fn a_local_ollama_is_asked_without_any_key() {
    let (base, server) = fake(vec![(200, r#"{"embeddings":[[1.0]]}"#)]);
    embed(&client(), &config(&base, ""), "all-minilm", &["a"]).await.expect("embedded");
    assert!(!server.join().expect("fake")[0].head.contains("authorization"));
}

#[rstest]
#[case::model_not_pulled(
    404,
    r#"{"error":"model \"embeddinggemma\" not found, try pulling it first"}"#,
    "has no model embeddinggemma: run `ollama pull embeddinggemma`"
)]
#[case::old_ollama(404, "404 page not found", "does not serve /api/embed: update Ollama")]
#[case::wrong_key(401, r#"{"error":"unauthorized"}"#, "refused the request (HTTP 401)")]
#[case::server_error(500, r#"{"error":"out of memory"}"#, "answered /api/embed with HTTP 500: out of memory")]
#[case::too_few_vectors(200, r#"{"embeddings":[[1.0]]}"#, "1 vectors for 2 texts")]
#[tokio::test]
async fn a_refusal_says_what_to_do_and_never_the_key(#[case] status: u16, #[case] body: &'static str, #[case] says: &str) {
    let (base, server) = fake(vec![(status, body)]);
    let error = embed(&client(), &config(&format!("{base}/?secret=q"), "k3y"), "embeddinggemma", &["a", "b"]).await.expect_err("refused");
    server.join().expect("fake");
    let message = error.to_string();
    assert!(message.contains(says), "{message}");
    assert!(message.starts_with(&format!("Ollama at {base} ")), "{message}");
    assert!(!message.contains("k3y") && !message.contains("secret"), "{message}");
}

#[tokio::test]
async fn an_unreachable_ollama_is_named_by_its_origin() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    drop(listener);
    let error = embed(&client(), &config(&format!("{base}/path?key=k3y"), ""), "embeddinggemma", &["a"]).await.expect_err("refused");
    let message = error.to_string();
    assert!(message.starts_with(&format!("Ollama at {base} is unreachable: ")), "{message}");
    assert!(!message.contains("k3y") && !message.contains("/path"), "{message}");
}

fn would_play() -> Questions {
    Questions::from([("would_play".to_string(), Question::Noul { instructions: "Would they play it?".to_string() })])
}

#[tokio::test]
async fn clef_is_asked_the_state_and_typed_questions() {
    let (base, server) =
        fake(vec![(200, r#"{"model":"clef","answers":{"would_play":{"type":"noul","noul":0.65}},"usage":{"input_tokens":411}}"#)]);
    let state = serde_json::json!({"title": "Heat", "nearest": []});
    let answers = decide(&client(), &config(&base, ""), "clef", &state, &would_play()).await.expect("answered");
    assert_eq!(answers.get("would_play"), Some(&Answer::Noul { noul: 0.65 }));
    let seen = server.join().expect("fake");
    assert!(seen[0].head.starts_with("post /v1/systemone "), "{}", seen[0].head);
    assert_eq!(
        seen[0].body,
        serde_json::json!({"model": "clef", "state": state, "questions": {"would_play": {"type": "noul", "instructions": "Would they play it?"}}})
    );
}

#[rstest]
#[case::unanswered(r#"{"answers":{"other":{"type":"noul","noul":0.5}}}"#, "no answer to would_play")]
#[case::not_a_probability(r#"{"answers":{"would_play":{"type":"noul","noul":1.5}}}"#, "not a probability")]
#[case::another_type(r#"{"answers":{"would_play":{"type":"choice","choice":"x","probabilities":{}}}}"#, "unusable /v1/systemone answer")]
#[tokio::test]
async fn an_answer_missing_or_out_of_range_is_an_error(#[case] body: &'static str, #[case] says: &str) {
    let (base, server) = fake(vec![(200, body)]);
    let error = decide(&client(), &config(&base, ""), "clef", &serde_json::json!({}), &would_play()).await.expect_err("refused");
    server.join().expect("fake");
    assert!(error.to_string().contains(says), "{error}");
}

#[rstest]
#[case::default(OllamaConfig::default(), true)]
#[case::cloud(config("https://ollama.com", ""), true)]
#[case::no_scheme(config("localhost:11434", ""), false)]
#[case::blank(config("", ""), false)]
#[case::spaces(config("http://local host:11434", ""), false)]
#[case::bad_variable(OllamaConfig { api_key_env: "OLLAMA-KEY".into(), ..OllamaConfig::default() }, false)]
fn only_an_http_url_and_a_plain_variable_name_are_accepted(#[case] config: OllamaConfig, #[case] valid: bool) {
    assert_eq!(config.validate().is_ok(), valid, "{:?}", config.validate());
}

#[test]
fn the_settings_token_wins_over_the_variable() {
    let config = OllamaConfig { token: " typed ".into(), api_key_env: "FLINCH_TEST_OLLAMA_UNSET".into(), ..OllamaConfig::default() };
    assert_eq!(config.api_key().as_deref(), Some("typed"));
    assert_eq!(OllamaConfig { token: String::new(), ..config }.api_key(), None);
}
