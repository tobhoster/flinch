//! A text embedding per library subject, for the taste feature of P(watch).
//!
//! A never-played title has no history of its own; what the household did
//! with titles like it is the next best evidence. "Like it" is measured by
//! sentence-embedding vectors of each title's catalogue description:
//! [`text`] writes that description from content metadata only, [`weights`]
//! fetches the pinned model's weights once, [`engine`] opens the configured
//! encoder — EmbeddingGemma 2 ([`gemma`]) or one of two small BERT encoders
//! ([`bert`]) for hosts that cannot spare Gemma's ~650 MB — in-process with
//! candle, and [`store`] caches the vectors in the state directory so each
//! title is embedded once per text change.
//!
//! With posters on (EmbeddingGemma 2 only), the description also holds the
//! title's poster: [`poster`] cuts it into patches, [`vision`] turns them into
//! soft tokens the text encoder reads at the poster's placeholder, and the one
//! vector describes both.
//!
//! A subject is a movie or a show: every season of a show shares its vector,
//! because the description (genre, cast, premise) is the show's. The vectors
//! are L2-normalised and all of one length, so a dot product is the cosine.

pub mod bert;
pub mod engine;
pub mod gemma;
pub mod poster;
pub mod store;
pub mod text;
pub mod vision;
pub mod weights;

pub use bert::{BertEncoder, Pooling};
pub use engine::EmbeddingEngine;
pub use gemma::{Encoder, ModelError};
pub use poster::{Patches, PosterError, Preprocessing};
pub use store::{StoreError, VectorStore};
pub use text::{movie_text, poster_hash, series_text, text_hash, with_poster, POSTER_RECIPE_VERSION, RECIPE_VERSION};
pub use vision::{ImageTokens, VisionEncoder};
pub use weights::{model_id, poster_model_id, BertFiles, FetchError, ModelFiles};

/// The vector cache, in the state directory.
pub const STORE_FILE: &str = "embeddings.json";

/// The subject a card's vector belongs to: a season (`sonarr-7-s2`,
/// `sonarr@anime-7-s2`) shares its show's (`sonarr-7`, `sonarr@anime-7`; see
/// [`crate::arr::ArrSeries::subject`]); a movie is its own subject.
pub fn subject_of(card_id: &str) -> &str {
    match crate::ids::ArrRef::card(card_id) {
        Some(crate::ids::ArrRef { season: Some(_), .. }) => card_id.rsplit_once("-s").map_or(card_id, |(show, _)| show),
        _ => card_id,
    }
}

/// The sentence encoder behind the vectors. EmbeddingGemma 2 is the best
/// describer but needs ~650 MB at its peak; the two BERT encoders fit a
/// <150 MB envelope at some cost in nuance. Switching retargets the store, so
/// every title is embedded again by the new model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EmbeddingModel {
    #[default]
    #[serde(rename = "embeddinggemma-2")]
    Gemma,
    #[serde(rename = "bge-small-en-v1.5")]
    BgeSmall,
    #[serde(rename = "all-minilm-l6-v2")]
    MiniLm,
}

impl EmbeddingModel {
    pub const ALL: [Self; 3] = [Self::Gemma, Self::BgeSmall, Self::MiniLm];

    /// The model's own vector length (Gemma's before Matryoshka truncation).
    pub fn native_dimensions(self) -> u32 {
        match self {
            Self::Gemma => 768,
            Self::BgeSmall | Self::MiniLm => 384,
        }
    }

    /// What a description starts with: EmbeddingGemma 2's symmetric
    /// classification prompt; the BERT encoders were trained without one.
    pub fn text_prefix(self) -> &'static str {
        match self {
            Self::Gemma => "task: classification | query: ",
            Self::BgeSmall | Self::MiniLm => "",
        }
    }

    /// What a search query starts with: each model's own retrieval prompt
    /// (MiniLM was trained symmetric and has none).
    pub fn query_prompt(self) -> &'static str {
        match self {
            Self::Gemma => "task: search result | query: ",
            Self::BgeSmall => "Represent this sentence for searching relevant passages: ",
            Self::MiniLm => "",
        }
    }

    /// Roughly what the first download weighs, for the daemon's log.
    pub fn download_mb(self) -> u32 {
        match self {
            Self::Gemma => 580,
            Self::BgeSmall => 135,
            Self::MiniLm => 92,
        }
    }
}

/// Embedding as the operator configured it (`settings.json` `embedding`).
/// Every field defaults individually.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct EmbeddingConfig {
    /// Embed new and changed titles. Off, the cached vectors stay in use and
    /// the model is neither downloaded nor run.
    pub enabled: bool,
    /// Which encoder makes the vectors.
    pub model: EmbeddingModel,
    /// Matryoshka truncation of EmbeddingGemma 2's 768-d output. The BERT
    /// encoders always keep their native 384 and ignore this field.
    pub dimensions: u32,
    /// Most subjects embedded per UTC day, so a first run over a large library
    /// spreads over days instead of occupying the CPU for hours.
    pub daily_budget: u32,
    /// Describe each title by its poster too (the *arr's upstream poster
    /// image), through EmbeddingGemma 2's vision tower. Off, text alone.
    pub posters: bool,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self { enabled: false, model: EmbeddingModel::default(), dimensions: 256, daily_budget: 500, posters: false }
    }
}

/// The truncations EmbeddingGemma 2 was trained to keep meaningful.
pub const DIMENSIONS: [u32; 4] = [128, 256, 512, 768];
/// Bounds of [`EmbeddingConfig::daily_budget`].
pub const MAX_DAILY_BUDGET: u32 = 20_000;

/// An [`EmbeddingConfig`] outside its bounds; the message names the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidEmbeddingConfig(pub &'static str);

impl EmbeddingConfig {
    pub fn validate(&self) -> Result<(), InvalidEmbeddingConfig> {
        if self.model == EmbeddingModel::Gemma && !DIMENSIONS.contains(&self.dimensions) {
            return Err(InvalidEmbeddingConfig("embedding dimensions must be 128, 256, 512 or 768"));
        }
        if self.posters && self.model != EmbeddingModel::Gemma {
            return Err(InvalidEmbeddingConfig("poster embeddings need the EmbeddingGemma 2 model"));
        }
        if !(1..=MAX_DAILY_BUDGET).contains(&self.daily_budget) {
            return Err(InvalidEmbeddingConfig("the embedding daily budget must be 1 to 20000 titles"));
        }
        Ok(())
    }

    /// The length the stored vectors have: the truncation for Gemma, the
    /// native 384 for the BERT encoders.
    pub fn vector_dimensions(&self) -> u32 {
        match self.model {
            EmbeddingModel::Gemma => self.dimensions,
            model => model.native_dimensions(),
        }
    }
}

/// A 768-d unit vector cut to `dimensions` and L2-normalised again (a
/// Matryoshka prefix is only an embedding once re-normalised); `None` when
/// the prefix is not usable.
pub fn truncate(vector: &[f32], dimensions: usize) -> Option<Vec<f32>> {
    store::unit(vector.get(..dimensions)?.to_vec())
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
