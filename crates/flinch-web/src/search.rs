//! Semantic search: the library ranked by how well each title's description
//! matches a free-text query ("heist film set in Paris", "cosy baking show").
//!
//! The daemon keeps one vector per movie or show in `embeddings.json` (see
//! `flinch_archive::embedding`), made by the operator's Ollama. A search
//! embeds the query through the same Ollama (`settings.json` `ollama`) with
//! the model the store names, cuts it to the store's length and scores every
//! items.json row by cosine; a season scores as its show.
//!
//! The query carries the model's retrieval prompt
//! (`flinch_archive::embedding::query_prompt`). For EmbeddingGemma that is
//! `task: search result` although the cached descriptions carry the
//! classification one: on a hand-labelled probe of short topical queries
//! against classification-prompt descriptions, the retrieval prompt put the
//! intended title first every time and with wider margins, where the
//! symmetric prompt confused "dinosaurs" with a nature documentary
//! (docs/how-it-works.md, "The web UI", Search by meaning). bge models have
//! their own query instruction; others take the bare query.
//!
//! Missing vectors are a 409 that says what to switch on; Ollama unreachable
//! or without the model is a 502 that says what to fix. Nothing is ever
//! downloaded from here.
//!
//! Advice only: search reads the published state and writes nothing.

use crate::{refuse, AppState};
use axum::extract::{Query, State as AxumState};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use flinch_archive::daemon::read_settings;
use flinch_archive::embedding::{self, StoreError, VectorStore};
use flinch_archive::ollama::{self, OllamaError};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

/// The longest query accepted, in characters: a search is a phrase.
pub(crate) const MAX_QUERY_CHARS: usize = 200;
const DEFAULT_LIMIT: usize = 50;
/// Most hits one search returns.
pub(crate) const MAX_LIMIT: usize = 1_000;

#[derive(Debug, thiserror::Error)]
pub(crate) enum SearchError {
    #[error("search needs title vectors and there are none yet: switch on Embed titles in Settings and let the daemon embed the library")]
    NoVectors,
    #[error("the title vectors were made with {found}, not by Ollama: search is back once the daemon has embedded the library again")]
    OtherModel { found: String },
    #[error("there is no library snapshot to search yet: wait for the daemon's first run")]
    NoItems,
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("search cannot read the Ollama settings: {0}")]
    Settings(#[from] flinch_archive::daemon::SettingsError),
    #[error("search could not embed the query: {0}")]
    Ollama(#[from] OllamaError),
    #[error("the query vector is unusable: {0}")]
    Query(String),
}

impl SearchError {
    fn status(&self) -> StatusCode {
        match self {
            Self::NoVectors | Self::OtherModel { .. } | Self::NoItems => StatusCode::CONFLICT,
            Self::Ollama(_) => StatusCode::BAD_GATEWAY,
            Self::Store(_) | Self::Settings(_) | Self::Query(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// The client that asks Ollama, and the vectors as last read.
pub(crate) struct Search {
    http: reqwest::Client,
    vectors: Mutex<Option<(Stamp, Arc<VectorStore>)>>,
}

/// What identifies one version of `embeddings.json`: the daemon replaces the
/// file whole, so a new write changes its modification time or length.
type Stamp = (SystemTime, u64);

impl Search {
    /// Redirects are never followed, so an Ollama key never reaches another host.
    pub(crate) fn new() -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build()?;
        Ok(Self { http, vectors: Mutex::new(None) })
    }

    async fn search(&self, dir: &Path, ask: &Ask) -> Result<Found, SearchError> {
        let mut items: Vec<Row> = std::fs::read_to_string(dir.join("items.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .ok_or(SearchError::NoItems)?;
        if let Some(kind) = &ask.kind {
            items.retain(|row| &row.kind == kind);
        }
        let store = self.vectors(dir)?;
        if store.is_empty() {
            return Err(SearchError::NoVectors);
        }
        // The store names its model; the query is embedded by that one.
        let Some(model) = embedding::ollama_model(store.model()) else {
            return Err(SearchError::OtherModel { found: store.model().to_string() });
        };
        let ollama = read_settings(&dir.join("settings.json"))?.ollama;
        let query = format!("{}{}", embedding::query_prompt(model), ask.query);
        let raw = ollama::embed(&self.http, &ollama, model, &[&query]).await?.pop().unwrap_or_default();
        let query = embedding::truncate(&raw, store.dimensions() as usize)
            .ok_or_else(|| SearchError::Query(format!("a {}-d query vector cannot be cut to {}", raw.len(), store.dimensions())))?;
        Ok(rank(items, &store, &query, ask.limit))
    }

    /// The vector store, re-read only when the daemon has replaced the file.
    fn vectors(&self, dir: &Path) -> Result<Arc<VectorStore>, SearchError> {
        let stamp = match std::fs::metadata(dir.join(embedding::STORE_FILE)) {
            Ok(meta) => (meta.modified().map_err(StoreError::from)?, meta.len()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(SearchError::NoVectors),
            Err(error) => return Err(StoreError::from(error).into()),
        };
        let mut cached = self.vectors.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((seen, store)) = cached.as_ref() {
            if *seen == stamp {
                return Ok(Arc::clone(store));
            }
        }
        let store = Arc::new(VectorStore::read(dir)?);
        *cached = Some((stamp, Arc::clone(&store)));
        Ok(store)
    }
}

/// The items.json fields a hit reports; the rest of a row stays unread.
#[derive(serde::Deserialize)]
struct Row {
    id: String,
    title: String,
    kind: String,
    #[serde(default)]
    season_label: Option<String>,
    #[serde(default)]
    year: Option<u32>,
}

#[derive(Debug, serde::Serialize)]
struct Hit {
    id: String,
    title: String,
    kind: String,
    season_label: Option<String>,
    year: Option<u32>,
    /// Cosine of query and description, −1..1; only the order is meaningful.
    score: f32,
}

/// One search: the trimmed query, the item kind it is confined to, the most hits.
struct Ask {
    query: String,
    kind: Option<String>,
    limit: usize,
}

#[derive(Debug, serde::Serialize)]
struct Found {
    /// Best first, at most the limit asked for.
    results: Vec<Hit>,
    /// Items that have no vector yet, so could not be ranked.
    unranked: usize,
}

/// Every item with a vector, best match first (ties by id), cut to `limit`.
fn rank(items: Vec<Row>, store: &VectorStore, query: &[f32], limit: usize) -> Found {
    let mut unranked = 0;
    let mut results: Vec<Hit> = items
        .into_iter()
        .filter_map(|row| {
            let Some(vector) = store.vector(embedding::subject_of(&row.id)) else {
                unranked += 1;
                return None;
            };
            let score = vector.iter().zip(query).map(|(a, b)| a * b).sum();
            Some(Hit { id: row.id, title: row.title, kind: row.kind, season_label: row.season_label, year: row.year, score })
        })
        .collect();
    results.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
    results.truncate(limit);
    Found { results, unranked }
}

#[derive(serde::Deserialize)]
pub(crate) struct Params {
    #[serde(default)]
    q: String,
    /// Only items.json rows of this kind (`movie`, `season`).
    kind: Option<String>,
    limit: Option<usize>,
}

/// `GET /api/search?q=…&kind=movie&limit=50`: the items ranked by meaning
/// (see the module docs). An empty or overlong query is a 400.
pub(crate) async fn api_search(AxumState(st): AxumState<AppState>, Query(params): Query<Params>) -> Response {
    let query = params.q.trim();
    if query.is_empty() {
        return refuse(StatusCode::BAD_REQUEST, "type something to search for");
    }
    if query.chars().count() > MAX_QUERY_CHARS {
        return refuse(StatusCode::BAD_REQUEST, &format!("a search is at most {MAX_QUERY_CHARS} characters"));
    }
    let ask = Ask { query: query.to_string(), kind: params.kind, limit: params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) };
    match st.search.search(&st.dir, &ask).await {
        Ok(found) => axum::Json(found).into_response(),
        Err(error) => refuse(error.status(), &error.to_string()),
    }
}

#[cfg(test)]
mod tests;
