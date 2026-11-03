//! The daemon's two Ollama steps against a fake Ollama: the title vectors and
//! the taste judge, and what an Ollama that cannot answer leaves behind.

use super::{fake, now, state_dir};
use crate::embeddings::{self, Library};
use crate::taste_judge;
use flinch_archive::arr::ArrMovie;
use flinch_archive::embedding::{EmbeddingConfig, VectorStore, STORE_FILE};
use flinch_archive::ollama::OllamaConfig;
use flinch_archive::taste::{judge, AnswerCache, Neighbour, State, TasteConfig};
use std::collections::HashMap;

fn ollama(base: &str) -> OllamaConfig {
    OllamaConfig { url: base.to_string(), ..OllamaConfig::default() }
}

/// A base URL nothing listens on.
fn nowhere() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    drop(listener);
    base
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().expect("client")
}

fn movies() -> Vec<ArrMovie> {
    let movie = |id, title: &str, year| ArrMovie {
        id,
        title: title.to_string(),
        year: Some(year),
        has_file: true,
        overview: Some(format!("{title}, the film.")),
        ..ArrMovie::default()
    };
    vec![movie(1, "Heat", 1995), movie(2, "Ronin", 1998)]
}

fn config() -> EmbeddingConfig {
    EmbeddingConfig { enabled: true, dimensions: 2, ..EmbeddingConfig::default() }
}

/// An Ollama embedding every text it is sent as a 3-d vector.
fn embedder() -> fake::Server {
    fake::serve(|request| match request.route() {
        "/api/embed" => {
            let body: serde_json::Value = serde_json::from_str(&request.body).unwrap_or_default();
            let count = body["input"].as_array().map_or(0, Vec::len);
            (200, serde_json::json!({ "embeddings": vec![[0.6, 0.8, 0.5]; count] }).to_string())
        }
        _ => (404, "404 page not found".into()),
    })
}

#[tokio::test]
async fn titles_are_embedded_through_ollama_with_their_title_and_text_kept() {
    let (_turn, dir) = state_dir("ollama-embed").await;
    let server = embedder();
    let (movies, plex_content, plex_ids) = (movies(), HashMap::new(), HashMap::new());
    let library = Library { movies: &movies, series: &[], plex_content: &plex_content, plex_ids: &plex_ids };
    let refresh = embeddings::refresh(&client(), &ollama(&server.base), &config(), library, now()).await;
    assert_eq!((refresh.status.embedded, refresh.status.problem.as_deref()), (2, None));
    assert_eq!(refresh.status.model, "ollama:embeddinggemma");
    let sent: serde_json::Value = serde_json::from_str(&server.requests()[0].body).expect("a JSON request");
    assert_eq!(sent["model"], "embeddinggemma");
    assert!(sent["input"][0].as_str().is_some_and(|text| text.starts_with("task: classification | query: Title: Heat\n")), "{sent}");
    let store = VectorStore::read(&dir).expect("written");
    assert_eq!((store.model(), store.dimensions()), ("ollama:embeddinggemma", 2));
    assert_eq!(store.title("radarr-1"), Some("Heat (1995)"));
    assert!(store.text("radarr-1").is_some_and(|text| text.starts_with("Title: Heat\nYear: 1995")), "no model prompt kept");
    assert_eq!(store.vector("radarr-1"), Some(&[0.6, 0.8][..]), "cut to 2 and normalised");
}

#[tokio::test]
async fn an_unreachable_ollama_is_a_problem_in_words_and_the_cache_stays() {
    let (_turn, dir) = state_dir("ollama-down").await;
    let (mut movies, plex_content, plex_ids) = (movies(), HashMap::new(), HashMap::new());
    let library = Library { movies: &movies, series: &[], plex_content: &plex_content, plex_ids: &plex_ids };
    embeddings::refresh(&client(), &ollama(&embedder().base), &config(), library, now()).await;
    let before = std::fs::read(dir.join(STORE_FILE)).expect("embedded once");
    movies.push(ArrMovie { id: 3, title: "Thief".into(), has_file: true, ..ArrMovie::default() });
    let base = nowhere();
    let library = Library { movies: &movies, series: &[], plex_content: &plex_content, plex_ids: &plex_ids };
    let refresh = embeddings::refresh(&client(), &ollama(&base), &config(), library, now()).await;
    let problem = refresh.status.problem.unwrap_or_default();
    assert!(problem.starts_with(&format!("Ollama at {base} is unreachable: ")), "{problem}");
    assert_eq!((refresh.status.embedded, refresh.status.with_vector, refresh.status.pending), (0, 2, 1));
    assert_eq!(std::fs::read(dir.join(STORE_FILE)).expect("still there"), before, "the cache is as it was");
}

fn heat() -> State {
    State { nearest: vec![Neighbour { title: "Ronin (1998)".into(), played: true }], title: "Title: Heat".into() }
}

#[tokio::test]
async fn a_question_waits_while_clef_is_missing_and_is_answered_once_it_is_there() {
    let (_turn, dir) = state_dir("ollama-judge").await;
    let config = TasteConfig { judge_enabled: true, ..TasteConfig::default() };
    let missing = fake::serve(|_| (404, r#"{"error":"model 'clef' not found"}"#.into()));
    let first = taste_judge::judge(&client(), &ollama(&missing.base), &config, now(), |judge| judge.p(heat())).await;
    assert_eq!(first.out, None, "no answer, no taste");
    let problem = first.status.problem.clone().unwrap_or_default();
    assert!(problem.contains("has no model clef: run `ollama pull clef`"), "{problem}");
    assert_eq!((first.status.answered, first.status.pending, first.status.asked), (0, 1, 0));
    assert_eq!(AnswerCache::read(&dir).expect("written").pending(), 1, "the question waits on disk");

    let clef = fake::serve(|request| match request.route() {
        "/v1/systemone" => (200, r#"{"model":"clef","answers":{"would_play":{"type":"noul","noul":0.7}}}"#.into()),
        _ => (404, "{}".into()),
    });
    let second = taste_judge::judge(&client(), &ollama(&clef.base), &config, now(), |judge| judge.p(heat())).await;
    assert_eq!(second.out, Some(0.7), "answered this cycle, read this cycle");
    assert_eq!((second.status.answered, second.status.pending, second.status.asked, second.status.problem), (1, 0, 1, None));
    assert_eq!(second.status.budget_left, config.judge_daily_budget - 1);
    let asked: serde_json::Value = serde_json::from_str(&clef.requests()[0].body).expect("a JSON request");
    assert_eq!(asked["model"], "clef");
    assert_eq!(asked["state"], serde_json::json!({"nearest": [{"played": true, "title": "Ronin (1998)"}], "title": "Title: Heat"}));
    assert_eq!(asked["questions"][judge::QUESTION]["type"], "noul");

    // Answered: the next cycle asks nothing.
    let third = taste_judge::judge(&client(), &ollama(&nowhere()), &config, now(), |judge| judge.p(heat())).await;
    assert_eq!((third.out, third.status.asked, third.status.problem), (Some(0.7), 0, None));
}

#[tokio::test]
async fn the_judge_switched_off_asks_nothing_and_writes_nothing() {
    let (_turn, dir) = state_dir("ollama-judge-off").await;
    let off = taste_judge::judge(&client(), &ollama(&nowhere()), &TasteConfig::default(), now(), |judge| judge.p(heat())).await;
    assert_eq!((off.out, off.status.configured), (None, false));
    assert!(!dir.join(judge::ANSWERS_FILE).exists());
}
