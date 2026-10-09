//! Search through the whole app, on a stand-in encoder so no weights are needed.

use super::*;
use crate::tests::{body_of, get, scratch, send, state, TOKEN};
use rstest::rstest;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

const BEARER: (&str, &str) = ("authorization", "Bearer s3cret");

/// Maps a query to a fixed direction by its first word, and insists on the prompt.
struct Stub;

impl QueryEncoder for Stub {
    fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        let query = text.strip_prefix(QUERY_PROMPT).ok_or_else(|| format!("unprompted query {text:?}"))?;
        Ok(match query.split_whitespace().next() {
            Some("space") => vec![1.0, 0.0, 0.0, 0.0],
            Some("romance") => vec![0.0, 1.0, 0.0, 0.0],
            _ => vec![0.0, 0.0, 1.0, 0.0],
        })
    }
}

fn stub_loader(loads: Arc<AtomicUsize>) -> Box<Loader> {
    Box::new(move |_| {
        loads.fetch_add(1, Ordering::Relaxed);
        Ok(Arc::new(Stub))
    })
}

/// A server whose library is Alien, Notting Hill, a two-season show between
/// them, and one movie the daemon has not embedded yet.
fn library(label: &str, search: Search) -> (PathBuf, AppState) {
    let tmp = scratch(label);
    let st = AppState { search: Arc::new(search), ..state(&tmp, Some(TOKEN)) };
    let rows = serde_json::json!([
        { "id": "radarr-2", "title": "Notting Hill", "kind": "movie", "year": 1999 },
        { "id": "sonarr-7-s2", "title": "Space Romance", "kind": "season", "season_label": "S2" },
        { "id": "radarr-3", "title": "Unembedded", "kind": "movie" },
        { "id": "radarr-1", "title": "Alien", "kind": "movie", "year": 1979 },
        { "id": "sonarr-7-s1", "title": "Space Romance", "kind": "season", "season_label": "S1" },
    ]);
    std::fs::write(st.dir.join("items.json"), rows.to_string()).unwrap();
    VectorStore::from_vectors([
        ("radarr-1".to_string(), vec![1.0, 0.0, 0.0, 0.0]),
        ("radarr-2".to_string(), vec![0.0, 1.0, 0.0, 0.0]),
        ("sonarr-7".to_string(), vec![0.8, 0.6, 0.0, 0.0]),
    ])
    .write(&st.dir)
    .unwrap();
    (tmp, st)
}

async fn json_of(res: Response) -> serde_json::Value {
    serde_json::from_str(&body_of(res).await).unwrap()
}

fn ids(found: &serde_json::Value) -> Vec<&str> {
    found["results"].as_array().unwrap().iter().map(|hit| hit["id"].as_str().unwrap()).collect()
}

#[tokio::test]
async fn search_needs_a_login_or_the_api_key() {
    let (tmp, st) = library("auth", Search::with_loader(stub_loader(Arc::default())));
    assert_eq!(send(&st, get("/api/search?q=space", &[])).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(send(&st, get("/api/search?q=space", &[("x-api-key", "guess")])).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(send(&st, get("/api/search?q=space", &[BEARER])).await.status(), StatusCode::OK);
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn items_rank_by_meaning_seasons_score_as_their_show_and_the_unembedded_are_counted() {
    let (tmp, st) = library("rank", Search::with_loader(stub_loader(Arc::default())));
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

#[rstest]
#[case::empty("")]
#[case::blank("%20%20")]
#[case::too_long(&"a".repeat(MAX_QUERY_CHARS + 1))]
#[tokio::test]
async fn an_empty_or_overlong_query_is_refused(#[case] q: &str) {
    let (tmp, st) = library("bad-query", Search::with_loader(stub_loader(Arc::default())));
    let res = send(&st, get(&format!("/api/search?q={q}"), &[BEARER])).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert!(json_of(res).await["error"].is_string());
    std::fs::remove_dir_all(&tmp).ok();
}

/// What is missing, set up on the stub library; the 409's message must name it.
enum Missing {
    Weights,
    Vectors,
    OtherModel,
}

#[rstest]
#[case::no_weights(Missing::Weights, "weights")]
#[case::no_vectors(Missing::Vectors, "title vectors")]
#[case::vectors_of_another_model(Missing::OtherModel, "made with google/embeddinggemma-1")]
#[tokio::test]
async fn missing_weights_or_vectors_are_a_conflict_that_says_what_to_do(#[case] missing: Missing, #[case] says: &str) {
    // The real loader: the scratch state directory holds no model.
    let (tmp, st) = library("conflict", Search::new());
    match missing {
        Missing::Weights => {}
        Missing::Vectors => std::fs::remove_file(st.dir.join(embedding::STORE_FILE)).unwrap(),
        Missing::OtherModel => {
            let mut store = VectorStore::default();
            store.retarget("google/embeddinggemma-1@00000000", 4, embedding::RECIPE_VERSION);
            store.insert("radarr-1".to_string(), String::new(), vec![1.0, 0.0, 0.0, 0.0]).unwrap();
            store.write(&st.dir).unwrap();
        }
    }
    let res = send(&st, get("/api/search?q=space", &[BEARER])).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let error = json_of(res).await["error"].as_str().unwrap().to_string();
    assert!(error.contains(says), "{error}");
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn text_and_poster_vectors_are_searched_like_text_ones() {
    let (tmp, st) = library("posters", Search::with_loader(stub_loader(Arc::default())));
    let mut store = VectorStore::default();
    store.retarget(&embedding::poster_model_id(), 4, embedding::POSTER_RECIPE_VERSION);
    store.insert("radarr-2".to_string(), String::new(), vec![1.0, 0.0, 0.0, 0.0]).unwrap();
    store.write(&st.dir).unwrap();
    let res = send(&st, get("/api/search?q=space", &[BEARER])).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(ids(&json_of(res).await), ["radarr-2"]);
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn the_encoder_opens_once_and_a_failed_open_is_tried_again() {
    let loads = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&loads);
    let search = Search::with_loader(Box::new(move |_| match counted.fetch_add(1, Ordering::Relaxed) {
        0 => Err(SearchError::NoModel),
        _ => Ok(Arc::new(Stub)),
    }));
    let (tmp, st) = library("lazy", search);
    assert_eq!(send(&st, get("/api/search?q=space", &[BEARER])).await.status(), StatusCode::CONFLICT);
    for _ in 0..2 {
        assert_eq!(send(&st, get("/api/search?q=space", &[BEARER])).await.status(), StatusCode::OK);
    }
    assert_eq!(loads.load(Ordering::Relaxed), 2);
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn a_new_vector_file_is_read_again() {
    let (tmp, st) = library("reread", Search::with_loader(stub_loader(Arc::default())));
    assert_eq!(ids(&json_of(send(&st, get("/api/search?q=space", &[BEARER])).await).await)[0], "radarr-1");
    VectorStore::from_vectors([("radarr-1".to_string(), vec![0.0, 1.0, 0.0]), ("radarr-3".to_string(), vec![1.0, 0.0, 0.0])])
        .write(&st.dir)
        .unwrap();
    let found = json_of(send(&st, get("/api/search?q=space", &[BEARER])).await).await;
    assert_eq!(ids(&found)[0], "radarr-3");
    assert_eq!(found["unranked"], 3);
    std::fs::remove_dir_all(&tmp).ok();
}
