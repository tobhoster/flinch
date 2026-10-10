//! Semantic search: the library ranked by how well each title's description
//! matches a free-text query ("heist film set in Paris", "cosy baking show").
//!
//! The daemon keeps one EmbeddingGemma 2 vector per movie or show in
//! `embeddings.json` (see `flinch_archive::embedding`). A search embeds the
//! query with the same model, on the weights the daemon already downloaded
//! to the state volume, cuts it to the store's Matryoshka length and scores
//! every items.json row by cosine; a season scores as its show.
//!
//! The query carries EmbeddingGemma 2's retrieval prompt (`task: search
//! result`) although the cached descriptions carry the classification one:
//! on a hand-labelled probe of short topical queries against
//! classification-prompt descriptions, the retrieval prompt put the intended
//! title first every time and with wider margins, where the symmetric prompt
//! confused "dinosaurs" with a nature documentary (docs/how-it-works.md, "The
//! web UI", Search by meaning).
//!
//! The encoder is opened on the first search and kept: its weights stay
//! memory-mapped, so only the pages a query touches are resident and the
//! kernel may drop them again. Queries run one at a time on the blocking pool,
//! because each one converts every layer's weights to float32 in turn and two
//! at once would double that on a small pod. Missing weights or vectors are a
//! 409 that says what to switch on; nothing is ever downloaded from here.
//!
//! Advice only: search reads the published state and writes nothing.

use crate::{refuse, AppState};
use axum::extract::{Query, State as AxumState};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use flinch_archive::embedding::{self, model_id, poster_model_id, Encoder, ModelFiles, StoreError, VectorStore};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

/// EmbeddingGemma 2's prompt for a search query.
pub(crate) const QUERY_PROMPT: &str = "task: search result | query: ";
/// The longest query accepted, in characters: a search is a phrase, and the
/// encoder's cost grows with every token.
pub(crate) const MAX_QUERY_CHARS: usize = 200;
const DEFAULT_LIMIT: usize = 50;
/// Most hits one search returns.
pub(crate) const MAX_LIMIT: usize = 1_000;

/// Turns one prompted query into a 768-d unit vector: the model, or a test's
/// stand-in that needs no weights.
pub(crate) trait QueryEncoder: Send + Sync {
    fn embed(&self, text: &str) -> Result<Vec<f32>, String>;
}

impl QueryEncoder for Encoder {
    fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        let mut vectors = Encoder::embed(self, &[text]).map_err(|error| error.to_string())?;
        vectors.pop().ok_or_else(|| "the embedding model returned no vector".to_string())
    }
}

/// Opens the encoder for a state directory.
pub(crate) type Loader = dyn Fn(&Path) -> Result<Arc<dyn QueryEncoder>, SearchError> + Send + Sync;

#[derive(Debug, thiserror::Error)]
pub(crate) enum SearchError {
    #[error("search needs the EmbeddingGemma 2 weights, which are not downloaded yet: switch on Embed titles in Settings and the daemon fetches them on its next run")]
    NoModel,
    #[error("search needs title vectors and there are none yet: switch on Embed titles in Settings and let the daemon embed the library")]
    NoVectors,
    #[error("the title vectors were made with {found}, not {wanted}: search is back once the daemon has embedded the library again")]
    OtherModel { found: String, wanted: String },
    #[error("there is no library snapshot to search yet: wait for the daemon's first run")]
    NoItems,
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("the embedding model failed: {0}")]
    Model(String),
}

impl SearchError {
    fn status(&self) -> StatusCode {
        match self {
            Self::NoModel | Self::NoVectors | Self::OtherModel { .. } | Self::NoItems => StatusCode::CONFLICT,
            Self::Store(_) | Self::Model(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// The encoder once opened, the vectors as last read, and the one-query gate.
pub(crate) struct Search {
    load: Box<Loader>,
    encoder: Mutex<Option<Arc<dyn QueryEncoder>>>,
    vectors: Mutex<Option<(Stamp, Arc<VectorStore>)>>,
    running: Arc<tokio::sync::Semaphore>,
}

/// What identifies one version of `embeddings.json`: the daemon replaces the
/// file whole, so a new write changes its modification time or length.
type Stamp = (SystemTime, u64);

impl Search {
    /// Search on the downloaded model.
    pub(crate) fn new() -> Self {
        Self::with_loader(Box::new(open_model))
    }

    pub(crate) fn with_loader(load: Box<Loader>) -> Self {
        Self { load, encoder: Mutex::new(None), vectors: Mutex::new(None), running: Arc::new(tokio::sync::Semaphore::new(1)) }
    }

    /// One query at a time; the permit travels with the blocking task, so a
    /// search the browser gave up on still holds the gate until it ends.
    async fn run(self: Arc<Self>, dir: Arc<std::path::PathBuf>, ask: Ask) -> Result<Found, SearchError> {
        let permit = Arc::clone(&self.running).acquire_owned().await.map_err(|error| SearchError::Model(error.to_string()))?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            self.search(&dir, &ask)
        })
        .await
        .map_err(|error| SearchError::Model(error.to_string()))?
    }

    fn search(&self, dir: &Path, ask: &Ask) -> Result<Found, SearchError> {
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
        // Text-and-poster vectors come from the same model's shared text and
        // image space, so a text query compares with them as with text-only ones.
        let (found, wanted) = (store.model(), model_id());
        if !(found.is_empty() || found == wanted || found == poster_model_id()) {
            return Err(SearchError::OtherModel { found: found.to_string(), wanted });
        }
        let raw = self.encoder(dir)?.embed(&format!("{QUERY_PROMPT}{}", ask.query)).map_err(SearchError::Model)?;
        let query = embedding::truncate(&raw, store.dimensions() as usize)
            .ok_or_else(|| SearchError::Model(format!("a {}-d query vector cannot be cut to {}", raw.len(), store.dimensions())))?;
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

    /// The encoder, opened on first use. A failure is not kept: the weights
    /// may arrive with the daemon's next run.
    fn encoder(&self, dir: &Path) -> Result<Arc<dyn QueryEncoder>, SearchError> {
        let mut cached = self.encoder.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(encoder) = cached.as_ref() {
            return Ok(Arc::clone(encoder));
        }
        let encoder = (self.load)(dir)?;
        *cached = Some(Arc::clone(&encoder));
        Ok(encoder)
    }
}

fn open_model(dir: &Path) -> Result<Arc<dyn QueryEncoder>, SearchError> {
    let files = ModelFiles::in_state(dir);
    if !files.present() {
        return Err(SearchError::NoModel);
    }
    let encoder = Encoder::open(&files).map_err(|error| SearchError::Model(error.to_string()))?;
    Ok(Arc::new(encoder))
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
    match Arc::clone(&st.search).run(Arc::clone(&st.dir), ask).await {
        Ok(found) => axum::Json(found).into_response(),
        Err(error) => refuse(error.status(), &error.to_string()),
    }
}

#[cfg(test)]
mod tests;
