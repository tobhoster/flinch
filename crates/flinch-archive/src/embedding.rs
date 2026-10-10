//! A text embedding per library subject, for the taste feature of P(watch).
//!
//! A never-played title has no history of its own; what the household did
//! with titles like it is the next best evidence. "Like it" is measured by
//! EmbeddingGemma 2 vectors of each title's catalogue description:
//! [`text`] writes that description from content metadata only, [`weights`]
//! fetches the pinned model's weights once, [`gemma`] runs the encoder
//! in-process with candle, and [`store`] caches the vectors in the state
//! directory so each title is embedded once per text change.
//!
//! With posters on, the description also holds the title's poster:
//! [`poster`] cuts it into patches, [`vision`] turns them into soft tokens
//! the text encoder reads at the poster's placeholder, and the one vector
//! describes both.
//!
//! A subject is a movie or a show: every season of a show shares its vector,
//! because the description (genre, cast, premise) is the show's. The vectors
//! are L2-normalised and all of one length, so a dot product is the cosine.

pub mod gemma;
pub mod poster;
pub mod store;
pub mod text;
pub mod vision;
pub mod weights;

pub use gemma::{Encoder, ModelError};
pub use poster::{Patches, PosterError, Preprocessing};
pub use store::{StoreError, VectorStore};
pub use text::{movie_text, poster_hash, series_text, text_hash, with_poster, POSTER_RECIPE_VERSION, RECIPE_VERSION};
pub use vision::{ImageTokens, VisionEncoder};
pub use weights::{model_id, poster_model_id, FetchError, ModelFiles};

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

/// Embedding as the operator configured it (`settings.json` `embedding`).
/// Every field defaults individually.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct EmbeddingConfig {
    /// Embed new and changed titles. Off, the cached vectors stay in use and
    /// the model is neither downloaded nor run.
    pub enabled: bool,
    /// Matryoshka truncation of the model's 768-d output.
    pub dimensions: u32,
    /// Most subjects embedded per UTC day, so a first run over a large library
    /// spreads over days instead of occupying the CPU for hours.
    pub daily_budget: u32,
    /// Describe each title by its poster too (the *arr's upstream poster
    /// image), through the model's vision tower. Off, text alone, as before.
    pub posters: bool,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self { enabled: false, dimensions: 256, daily_budget: 500, posters: false }
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
        if !DIMENSIONS.contains(&self.dimensions) {
            return Err(InvalidEmbeddingConfig("embedding dimensions must be 128, 256, 512 or 768"));
        }
        if !(1..=MAX_DAILY_BUDGET).contains(&self.daily_budget) {
            return Err(InvalidEmbeddingConfig("the embedding daily budget must be 1 to 20000 titles"));
        }
        Ok(())
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
