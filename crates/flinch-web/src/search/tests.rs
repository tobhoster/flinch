//! Search through the whole app, against a fake Ollama on a loopback port.

use super::*;
use crate::tests::{body_of, get, scratch, send, state, TOKEN};
use axum::routing::post;
use axum::{Json, Router};
use flinch_archive::embedding::{Description, RECIPE_VERSION};
use rstest::rstest;
use serde_json::{json, Value};
use std::path::PathBuf;

const BEARER: (&str, &str) = ("authorization", "Bearer s3cret");

/// An Ollama serving only `model`: it maps a query to a fixed direction by
/// its first word, and insists on that model's prompt. Returns its URL.
async fn fake_ollama(model: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind the fake Ollama");
    let url = format!("http://{}", listener.local_addr().expect("fake address"));
    let embed = move |Json(body): Json<Value>| async move {
        let asked = body["model"].as_str().unwrap_or_default().to_string();
        if asked != model {
            let error = json!({ "error": format!("model \"{asked}\" not found, try pulling it first") });
            return (StatusCode::NOT_FOUND, Json(error));
        }
        let text = body["input"][0].as_str().unwrap_or_default();
        let Some(query) = text.strip_prefix(embedding::query_prompt(model)) else {
            return (StatusCode::BAD_REQUEST, Json(json!({ "error": format!("unprompted query {text:?}") })));
        };
        let vector = match query.split_whitespace().next() {
            Some("space") => [1.0, 0.0, 0.0, 0.0],
            Some("romance") => [0.0, 1.0, 0.0, 0.0],
            _ => [0.0, 0.0, 1.0, 0.0],
        };
        (StatusCode::OK, Json(json!({ "model": model, "embeddings": [vector] })))
    };
    let app = Router::new().route("/api/embed", post(embed));
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

/// Point the state directory's settings at the Ollama at `url`.
fn use_ollama(st: &AppState, url: &str) {
    let settings = json!({ "ollama": { "url": url } });
    std::fs::write(st.dir.join("settings.json"), settings.to_string()).unwrap();
}

/// A store of `model`'s vectors, as the daemon writes it.
fn store_of(model: &str, vectors: &[(&str, Vec<f32>)]) -> VectorStore {
    let mut store = VectorStore::default();
    store.retarget(&embedding::model_id(model), 0, RECIPE_VERSION);
    for (subject, vector) in vectors {
        store.insert(subject.to_string(), Description::default(), vector.clone()).unwrap();
    }
    store
}

/// A server whose library is Alien, Notting Hill, a two-season show between
/// them, and one movie the daemon has not embedded yet, searched through a
/// fake Ollama serving EmbeddingGemma.
async fn library(label: &str) -> (PathBuf, AppState) {
    let tmp = scratch(label);
    let st = state(&tmp, Some(TOKEN));
    let rows = json!([
        { "id": "radarr-2", "title": "Notting Hill", "kind": "movie", "year": 1999 },
        { "id": "sonarr-7-s2", "title": "Space Romance", "kind": "season", "season_label": "S2" },
        { "id": "radarr-3", "title": "Unembedded", "kind": "movie" },
        { "id": "radarr-1", "title": "Alien", "kind": "movie", "year": 1979 },
        { "id": "sonarr-7-s1", "title": "Space Romance", "kind": "season", "season_label": "S1" },
    ]);
    std::fs::write(st.dir.join("items.json"), rows.to_string()).unwrap();
    store_of(
        "embeddinggemma",
        &[("radarr-1", vec![1.0, 0.0, 0.0, 0.0]), ("radarr-2", vec![0.0, 1.0, 0.0, 0.0]), ("sonarr-7", vec![0.8, 0.6, 0.0, 0.0])],
    )
    .write(&st.dir)
    .unwrap();
    use_ollama(&st, &fake_ollama("embeddinggemma").await);
    (tmp, st)
}

async fn json_of(res: Response) -> Value {
    serde_json::from_str(&body_of(res).await).unwrap()
}

fn ids(found: &Value) -> Vec<&str> {
    found["results"].as_array().unwrap().iter().map(|hit| hit["id"].as_str().unwrap()).collect()
}

#[tokio::test]
async fn search_needs_a_login_or_the_api_key() {
    let (tmp, st) = library("auth").await;
    assert_eq!(send(&st, get("/api/search?q=space", &[])).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(send(&st, get("/api/search?q=space", &[("x-api-key", "guess")])).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(send(&st, get("/api/search?q=space", &[BEARER])).await.status(), StatusCode::OK);
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn items_rank_by_meaning_seasons_score_as_their_show_and_the_unembedded_are_counted() {
    let (tmp, st) = library("rank").await;
    let res = send(&st, get("/api/search?q=space%20horror", &[BEARER])).await;
    assert_eq!(res.status(), StatusCode::OK);
    let found = json_of(res).await;
    assert_eq!(ids(&found), ["radarr-1", "sonarr-7-s1", "sonarr-7-s2", "radarr-2"]);
    assert_eq!(found["results"][1]["season_label"], "S1");
    assert_eq!(found["results"][0]["year"], 1979);
    assert_eq!(found["unranked"], 1);

    let found = json_of(send(&st, get("/api/search?q=romance&limit=2", &[BEARER])).await).await;
    assert_eq!(ids(&found), ["radarr-2", "sonarr-7-s1"]);

    let found = json_of(send(&st, get("/api/search?q=space&kind=movie", &[BEARER])).await).await;
    assert_eq!(ids(&found), ["radarr-1", "radarr-2"]);
    assert_eq!(found["unranked"], 1);
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn a_named_instances_seasons_score_as_its_own_show() {
    let (tmp, st) = library("named").await;
    let rows = json!([
        { "id": "radarr-1", "title": "Alien", "kind": "movie" },
        { "id": "radarr@4k-1", "title": "Alien", "kind": "movie" },
        { "id": "sonarr@anime-7-s1", "title": "Space Brothers", "kind": "season", "season_label": "S1" },
        { "id": "sonarr-7-s1", "title": "Space Romance", "kind": "season", "season_label": "S1" },
    ]);
    std::fs::write(st.dir.join("items.json"), rows.to_string()).unwrap();
    // Sonarr's show 7 and the anime Sonarr's show 7 are different shows.
    store_of(
        "embeddinggemma",
        &[
            ("radarr-1", vec![0.6, 0.8, 0.0, 0.0]),
            ("radarr@4k-1", vec![0.6, 0.8, 0.0, 0.0]),
            ("sonarr@anime-7", vec![1.0, 0.0, 0.0, 0.0]),
            ("sonarr-7", vec![0.0, 1.0, 0.0, 0.0]),
        ],
    )
    .write(&st.dir)
    .unwrap();
    let found = json_of(send(&st, get("/api/search?q=space", &[BEARER])).await).await;
    assert_eq!(ids(&found), ["sonarr@anime-7-s1", "radarr-1", "radarr@4k-1", "sonarr-7-s1"]);
    assert_eq!(found["unranked"], 0);
    std::fs::remove_dir_all(&tmp).ok();
}

#[rstest]
#[case::empty("")]
#[case::blank("%20%20")]
#[case::too_long(&"a".repeat(MAX_QUERY_CHARS + 1))]
#[tokio::test]
async fn an_empty_or_overlong_query_is_refused(#[case] q: &str) {
    let (tmp, st) = library("bad-query").await;
    let res = send(&st, get(&format!("/api/search?q={q}"), &[BEARER])).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert!(json_of(res).await["error"].is_string());
    std::fs::remove_dir_all(&tmp).ok();
}

/// What is wrong, set up on the fake library; the answer must name it.
enum Broken {
    NoVectors,
    InProcessVectors,
    OllamaDown,
    ModelNotPulled,
}

#[rstest]
#[case::no_vectors(Broken::NoVectors, StatusCode::CONFLICT, "title vectors")]
#[case::vectors_of_an_in_process_model(Broken::InProcessVectors, StatusCode::CONFLICT, "made with google/embeddinggemma-1")]
#[case::ollama_down(Broken::OllamaDown, StatusCode::BAD_GATEWAY, "is unreachable")]
#[case::model_not_pulled(Broken::ModelNotPulled, StatusCode::BAD_GATEWAY, "run `ollama pull embeddinggemma`")]
#[tokio::test]
async fn a_search_that_cannot_run_says_what_to_do(#[case] broken: Broken, #[case] status: StatusCode, #[case] says: &str) {
    let (tmp, st) = library("broken").await;
    match broken {
        Broken::NoVectors => std::fs::remove_file(st.dir.join(embedding::STORE_FILE)).unwrap(),
        Broken::InProcessVectors => {
            let mut store = VectorStore::default();
            store.retarget("google/embeddinggemma-1@00000000", 4, RECIPE_VERSION);
            store.insert("radarr-1".to_string(), Description::default(), vec![1.0, 0.0, 0.0, 0.0]).unwrap();
            store.write(&st.dir).unwrap();
        }
        Broken::OllamaDown => {
            let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", closed.local_addr().unwrap());
            drop(closed);
            use_ollama(&st, &url);
        }
        Broken::ModelNotPulled => use_ollama(&st, &fake_ollama("all-minilm").await),
    }
    let res = send(&st, get("/api/search?q=space", &[BEARER])).await;
    assert_eq!(res.status(), status);
    let error = json_of(res).await["error"].as_str().unwrap().to_string();
    assert!(error.contains(says), "{error}");
    std::fs::remove_dir_all(&tmp).ok();
}

#[rstest]
#[case::bge("bge-m3")]
#[case::minilm("all-minilm")]
#[tokio::test]
async fn the_query_is_embedded_by_the_stores_model_with_its_prompt(#[case] model: &'static str) {
    // The fake serves only the store's model, and only with its prompt.
    let (tmp, st) = library("model").await;
    use_ollama(&st, &fake_ollama(model).await);
    store_of(model, &[("radarr-1", vec![1.0, 0.0, 0.0, 0.0])]).write(&st.dir).unwrap();
    let res = send(&st, get("/api/search?q=space", &[BEARER])).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(ids(&json_of(res).await), ["radarr-1"]);
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn a_new_vector_file_is_read_again() {
    let (tmp, st) = library("reread").await;
    assert_eq!(ids(&json_of(send(&st, get("/api/search?q=space", &[BEARER])).await).await)[0], "radarr-1");
    store_of("embeddinggemma", &[("radarr-1", vec![0.0, 1.0, 0.0]), ("radarr-3", vec![1.0, 0.0, 0.0])]).write(&st.dir).unwrap();
    let found = json_of(send(&st, get("/api/search?q=space", &[BEARER])).await).await;
    assert_eq!(ids(&found)[0], "radarr-3");
    assert_eq!(found["unranked"], 3);
    std::fs::remove_dir_all(&tmp).ok();
}
