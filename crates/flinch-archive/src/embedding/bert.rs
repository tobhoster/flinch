//! The small BERT sentence encoders (bge-small-en-v1.5, all-MiniLM-L6-v2),
//! in candle, on the CPU.
//!
//! They exist for hosts that cannot spare EmbeddingGemma 2's ~650 MB peak:
//! 33 M and 23 M parameters in float32, memory-mapped, so a batch costs the
//! mapped pages it touches plus its activations. candle's own BERT port runs
//! the layers; this file adds what sentence-transformers adds around it —
//! batch padding, the checkpoint's pooling, L2 normalisation.
//!
//! Texts are cut to [`MAX_TOKENS`]: FLINCH's 2 000-character descriptions run
//! a little past it, and the cut falls in the overview, which comes last.
//! Padding is to the batch's longest text and masked out of attention and of
//! mean pooling, so a text's vector does not depend on its batch.

use super::engine::EmbeddingEngine;
use super::ModelError;
use candle_core::{DType, Device, IndexOp, Tensor, D};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use std::path::Path;
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

/// Tokens kept per text.
pub const MAX_TOKENS: usize = 256;
/// Texts per forward pass. The weights are resident, so a larger pass saves
/// nothing; its attention scores (`pass × heads × 256²` floats, several
/// copies at once) are what grows. Measured on MiniLM, 16 × 256 tokens: a
/// pass of 4 is as fast as one of 16 (≈1.6 s) and peaks ~245 MB lower.
const PASS: usize = 4;

/// How token states become one sentence vector, as the checkpoint's
/// `1_Pooling/config.json` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pooling {
    /// The average of the real (unpadded) tokens' states (MiniLM).
    Mean,
    /// The first (`[CLS]`) token's state (bge).
    Cls,
}

pub struct BertEncoder {
    model: BertModel,
    tokenizer: Tokenizer,
    pooling: Pooling,
    dimension: usize,
    device: Device,
}

impl BertEncoder {
    /// Map the weights and read the config and tokenizer in `dir`.
    pub fn open(dir: &Path, pooling: Pooling) -> Result<Self, ModelError> {
        let config_error = |error: &dyn std::fmt::Display| ModelError::Config(error.to_string());
        let text = std::fs::read_to_string(dir.join("config.json")).map_err(|error| config_error(&error))?;
        let config: Config = serde_json::from_str(&text).map_err(|error| config_error(&error))?;
        let dimension = serde_json::from_str::<Width>(&text).map_err(|error| config_error(&error))?.hidden_size;
        let mut tokenizer = Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|error| ModelError::Tokenizer(error.to_string()))?;
        let pad_id = tokenizer.token_to_id("[PAD]").ok_or_else(|| ModelError::Tokenizer("no [PAD] token".into()))?;
        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::BatchLongest,
            pad_id,
            pad_token: "[PAD]".into(),
            ..PaddingParams::default()
        }));
        tokenizer
            .with_truncation(Some(TruncationParams { max_length: MAX_TOKENS, ..TruncationParams::default() }))
            .map_err(|error| ModelError::Tokenizer(error.to_string()))?;
        let device = Device::Cpu;
        // SAFETY: the file is FLINCH's own download, renamed into place whole
        // and never written again; nothing truncates it while it is mapped.
        let weights = unsafe { VarBuilder::from_mmaped_safetensors(&[dir.join("model.safetensors")], DType::F32, &device)? };
        let model = BertModel::load(weights, &config)?;
        Ok(Self { model, tokenizer, pooling, dimension, device })
    }
}

/// The one config field candle's `Config` keeps private.
#[derive(serde::Deserialize)]
struct Width {
    hidden_size: usize,
}

impl EmbeddingEngine for BertEncoder {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, ModelError> {
        let mut vectors = Vec::with_capacity(texts.len());
        for pass in texts.chunks(PASS) {
            vectors.extend(self.pass(pass)?);
        }
        Ok(vectors)
    }

    fn dimension(&self) -> usize {
        self.dimension
    }
}

impl BertEncoder {
    /// One forward pass over at most [`PASS`] texts.
    fn pass(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, ModelError> {
        let encodings = self.tokenizer.encode_batch(texts.to_vec(), true).map_err(|error| ModelError::Tokenizer(error.to_string()))?;
        let rows = |field: fn(&tokenizers::Encoding) -> &[u32]| {
            let rows =
                encodings.iter().map(|encoding| Tensor::new(field(encoding), &self.device)).collect::<candle_core::Result<Vec<_>>>()?;
            Tensor::stack(&rows, 0)
        };
        let ids = rows(tokenizers::Encoding::get_ids)?;
        let mask = rows(tokenizers::Encoding::get_attention_mask)?;
        let token_types = ids.zeros_like()?;
        let states = self.model.forward(&ids, &token_types, Some(&mask))?;
        let pooled = match self.pooling {
            Pooling::Cls => states.i((.., 0))?,
            Pooling::Mean => {
                let mask = mask.to_dtype(DType::F32)?.unsqueeze(D::Minus1)?;
                let summed = states.broadcast_mul(&mask)?.sum(1)?;
                summed.broadcast_div(&mask.sum(1)?)?
            }
        };
        let unit = pooled.broadcast_div(&pooled.sqr()?.sum_keepdim(D::Minus1)?.sqrt()?)?;
        Ok(unit.to_vec2::<f32>()?)
    }
}
