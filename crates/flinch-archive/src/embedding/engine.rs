//! One face for every text encoder, so the daemon's refresh and the web's
//! search open whichever model the operator chose (or the store was made
//! with) without knowing which. Posters stay EmbeddingGemma 2's own
//! ([`super::Encoder::embed_with_images`]): only it has a vision tower.

use super::weights::{bert_checkpoint, BertFiles, FetchError, ModelFiles};
use super::{BertEncoder, EmbeddingModel, Encoder, ModelError};
use std::path::Path;

/// A sentence encoder: one L2-normalised vector per text, in order.
pub trait EmbeddingEngine: Send + Sync {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, ModelError>;
    /// The length of every vector it returns.
    fn dimension(&self) -> usize;
}

impl EmbeddingEngine for Encoder {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, ModelError> {
        Encoder::embed(self, texts)
    }

    fn dimension(&self) -> usize {
        768
    }
}

/// `model`'s text files are on the state volume.
pub fn present(model: EmbeddingModel, state_dir: &Path) -> bool {
    match bert_checkpoint(model) {
        Some(checkpoint) => BertFiles::in_state(state_dir, checkpoint).present(),
        None => ModelFiles::in_state(state_dir).present(),
    }
}

/// Download whatever of `model`'s text files is missing.
pub async fn fetch(model: EmbeddingModel, state_dir: &Path) -> Result<(), FetchError> {
    match bert_checkpoint(model) {
        Some(checkpoint) => BertFiles::in_state(state_dir, checkpoint).fetch().await,
        None => ModelFiles::in_state(state_dir).fetch().await,
    }
}

/// Open `model` from its downloaded files.
pub fn open(model: EmbeddingModel, state_dir: &Path) -> Result<Box<dyn EmbeddingEngine>, ModelError> {
    Ok(match bert_checkpoint(model) {
        Some(checkpoint) => Box::new(BertEncoder::open(BertFiles::in_state(state_dir, checkpoint).dir(), checkpoint.pooling)?),
        None => Box::new(Encoder::open(&ModelFiles::in_state(state_dir))?),
    })
}
