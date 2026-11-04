//! A text embedding per library subject: what taste retrieves neighbours by.
//!
//! A never-played title has no history of its own; what the household did
//! with titles like it is the next best evidence. "Like it" is measured by
//! sentence-embedding vectors of each title's catalogue description:
//! [`text`] writes that description from content metadata only, the
//! operator's Ollama embeds it ([`crate::ollama::embed`], MiniLM by
//! default), and [`store`] caches the vectors in the state directory so each
//! title is embedded once per text change. Nothing runs in-process.
//!
//! A subject is a movie or a show: every season of a show shares its vector,
//! because the description (genre, cast, premise) is the show's. The vectors
//! are L2-normalised and all of one length, so a dot product is the cosine.

pub mod store;
pub mod text;

pub use store::{Description, StoreError, VectorStore};
pub use text::{display_title, movie_text, series_text, text_hash, RECIPE_VERSION};

/// The vector cache, in the state directory.
pub const STORE_FILE: &str = "embeddings.json";
/// The Ollama model embedding titles unless the operator names another.
pub const DEFAULT_MODEL: &str = "all-minilm";
/// What a store's model id starts with: its vectors came from Ollama.
const OLLAMA_MODEL: &str = "ollama:";

/// The subject a card's vector belongs to: a season (`sonarr-7-s2`,
/// `sonarr@anime-7-s2`) shares its show's (`sonarr-7`, `sonarr@anime-7`; see
/// [`crate::arr::ArrSeries::subject`]); a movie is its own subject.
pub fn subject_of(card_id: &str) -> &str {
    match crate::ids::ArrRef::card(card_id) {
        Some(crate::ids::ArrRef { season: Some(_), .. }) => card_id.rsplit_once("-s").map_or(card_id, |(show, _)| show),
        _ => card_id,
    }
}

/// The store's id for vectors from the Ollama model `model`.
pub fn model_id(model: &str) -> String {
    format!("{OLLAMA_MODEL}{}", model.trim())
}

/// The Ollama model a store's vectors came from; `None` for any other source.
pub fn ollama_model(model_id: &str) -> Option<&str> {
    model_id.strip_prefix(OLLAMA_MODEL).filter(|model| !model.is_empty())
}

/// The model's family, for its prompts: the name without a namespace
/// (`hf.co/…/`) or tag (`:latest`), lowercased.
fn family(model: &str) -> String {
    let name = model.trim().rsplit('/').next().unwrap_or_default();
    name.split(':').next().unwrap_or_default().to_lowercase()
}

/// What a description starts with: EmbeddingGemma's symmetric classification
/// prompt; other models were trained without one.
pub fn text_prefix(model: &str) -> &'static str {
    if family(model).starts_with("embeddinggemma") {
        "task: classification | query: "
    } else {
        ""
    }
}

/// What a search query starts with: each model's own retrieval prompt
/// (EmbeddingGemma's search task, bge's query instruction); none for models
/// trained symmetric.
pub fn query_prompt(model: &str) -> &'static str {
    let family = family(model);
    if family.starts_with("embeddinggemma") {
        "task: search result | query: "
    } else if family.starts_with("bge") {
        "Represent this sentence for searching relevant passages: "
    } else {
        ""
    }
}

/// Embedding as the operator configured it (`settings.json` `embedding`).
/// Every field defaults individually.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct EmbeddingConfig {
    /// Embed new and changed titles. Off, the cached vectors stay in use and
    /// Ollama is not asked.
    pub enabled: bool,
    /// The Ollama embedding model (`ollama pull <model>` on the server).
    pub model: String,
    /// Matryoshka truncation of the model's vectors, re-normalised, for models
    /// trained for it; 0 (the default) keeps the model's own length.
    pub dimensions: u32,
    /// Most subjects embedded per UTC day, so a first run over a large library
    /// spreads over days instead of occupying Ollama for hours.
    pub daily_budget: u32,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self { enabled: false, model: DEFAULT_MODEL.to_string(), dimensions: 0, daily_budget: 500 }
    }
}

/// Bounds of a non-zero [`EmbeddingConfig::dimensions`].
pub const DIMENSIONS: (u32, u32) = (32, 4096);
/// Bounds of [`EmbeddingConfig::daily_budget`].
pub const MAX_DAILY_BUDGET: u32 = 20_000;
/// The longest model name accepted.
const MAX_MODEL_CHARS: usize = 200;

/// An [`EmbeddingConfig`] outside its bounds; the message names the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidEmbeddingConfig(pub &'static str);

impl EmbeddingConfig {
    pub fn validate(&self) -> Result<(), InvalidEmbeddingConfig> {
        let model = self.model.trim();
        if model.is_empty() || model.chars().count() > MAX_MODEL_CHARS || model.chars().any(char::is_whitespace) {
            return Err(InvalidEmbeddingConfig("the embedding model (embedding.model) must be an Ollama model name, without spaces"));
        }
        let (low, high) = DIMENSIONS;
        if self.dimensions != 0 && !(low..=high).contains(&self.dimensions) {
            return Err(InvalidEmbeddingConfig("embedding dimensions must be 0 (the model's own) or 32 to 4096"));
        }
        if !(1..=MAX_DAILY_BUDGET).contains(&self.daily_budget) {
            return Err(InvalidEmbeddingConfig("the embedding daily budget must be 1 to 20000 titles"));
        }
        Ok(())
    }
}

/// A vector cut to `dimensions` and L2-normalised again (a Matryoshka prefix
/// is only an embedding once re-normalised); `dimensions` 0 keeps its whole
/// length. `None` when the result is not usable: too short, zero or not finite.
pub fn truncate(vector: &[f32], dimensions: usize) -> Option<Vec<f32>> {
    let kept = if dimensions == 0 { vector } else { vector.get(..dimensions)? };
    store::unit(kept.to_vec())
}

/// What status.json reports about the vectors (`embedding`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EmbeddingStatus {
    /// Embedding is switched on; off, the cached vectors are still used.
    pub configured: bool,
    pub model: String,
    pub dimensions: u32,
    /// Movies and shows in Radarr and Sonarr.
    pub subjects: usize,
    /// Of those, how many have a vector (current or awaiting a re-embed).
    pub with_vector: usize,
    /// Of those, how many lack a vector for their current text.
    pub pending: usize,
    /// Subjects embedded this cycle.
    pub embedded: usize,
    /// Subjects that may still be embedded today (UTC).
    pub budget_left: u32,
    /// Why this cycle embedded less than it could, in words.
    pub problem: Option<String>,
}

#[cfg(test)]
mod tests;
