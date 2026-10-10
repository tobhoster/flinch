//! EmbeddingGemma 2's text encoder, in candle, on the CPU.
//!
//! A port of `EmbeddingGemma2TextModel` (transformers `embedding_gemma2`): 24
//! bidirectional layers, every sixth a wider global one, each gated by a
//! projection-only per-layer embedding (PLE), then a final norm, the 512→768
//! projection, mean pooling over every token (the prompt and any image's soft
//! tokens included, as the sentence-transformers pooling config says) and L2
//! normalisation. An image enters as the soft tokens [`super::vision`] makes
//! of it, merged in at its placeholder as `EmbeddingGemma2Model` merges them.
//!
//! The weights stay memory-mapped in bfloat16 and are read one layer at a time
//! for a whole batch, in float32 (the model card: float32 on CPUs, never
//! float16). The 262 144-row token table is never loaded: only the rows of the
//! tokens in the batch are. A batch therefore costs one layer's weights plus
//! its activations, not the 1 GB a float32 copy of the model would take.

use super::text::IMAGE_PLACEHOLDER;
use super::vision::ImageTokens;
use super::weights::ModelFiles;
use candle_core::safetensors::MmapedSafetensors;
use candle_core::{DType, Device, IndexOp, Tensor, D};
use tokenizers::Tokenizer;

/// The tokens the reference processor wraps an image's soft tokens in.
const IMAGE_OPEN: &str = "<|image>";
const IMAGE_CLOSE: &str = "<image|>";

const HIDDEN: usize = 512;
const LAYERS: usize = 24;
const HEADS: usize = 4;
const PER_LAYER_INPUT: usize = 512;
const EPS: f64 = 1e-6;
/// Bidirectional radius of a sliding layer: `|q − k| <= SLIDING_WINDOW`.
const SLIDING_WINDOW: usize = 512;
/// `sqrt(512)` as bfloat16 rounds it, the scale the model was trained with.
const EMBED_SCALE: f64 = 22.625;
/// Tokens kept per text. FLINCH's descriptions stop at 2 000 characters, far
/// below this; the cap only bounds the attention matrices of a stray input.
pub const MAX_TOKENS: usize = 2048;

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("embedding model: {0}")]
    Tensor(#[from] candle_core::Error),
    #[error("embedding tokenizer: {0}")]
    Tokenizer(String),
    #[error("embedding model config: {0}")]
    Config(String),
    #[error("embedding model weights hold {name} as {found}, not bfloat16")]
    Dtype { name: String, found: String },
    #[error("embedding image processor: {0}")]
    Processor(String),
    #[error("a text given {wanted} image(s) holds {found} image placeholder(s)")]
    Placeholders { found: usize, wanted: usize },
}

/// One layer's attention geometry: sliding layers are narrower than global ones.
#[derive(Clone, Copy)]
struct Kind {
    head_dim: usize,
    kv_heads: usize,
    rope_theta: f64,
    sliding: bool,
}

const SLIDING: Kind = Kind { head_dim: 256, kv_heads: 2, rope_theta: 10_000.0, sliding: true };
const GLOBAL: Kind = Kind { head_dim: 512, kv_heads: 1, rope_theta: 1_000_000.0, sliding: false };

fn kind(layer: usize) -> Kind {
    if layer % 6 == 5 {
        GLOBAL
    } else {
        SLIDING
    }
}

pub struct Encoder {
    weights: MmapedSafetensors,
    tokenizer: Tokenizer,
    device: Device,
}

impl Encoder {
    /// Map the weights and read the tokenizer of a downloaded model.
    pub fn open(files: &ModelFiles) -> Result<Self, ModelError> {
        // SAFETY: the file is FLINCH's own download, renamed into place whole
        // and never written again; nothing truncates it while it is mapped.
        let weights = unsafe { MmapedSafetensors::new(files.weights())? };
        let tokenizer = Tokenizer::from_file(files.tokenizer()).map_err(|error| ModelError::Tokenizer(error.to_string()))?;
        Ok(Self { weights, tokenizer, device: Device::Cpu })
    }

    /// One 768-d unit vector per text, in order.
    pub fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, ModelError> {
        let inputs: Vec<(&str, Option<&ImageTokens>)> = texts.iter().map(|text| (*text, None)).collect();
        self.embed_with_images(&inputs)
    }

    /// One 768-d unit vector per text and its image, in order. A text given an
    /// image holds one [`IMAGE_PLACEHOLDER`]; a text without one is embedded
    /// exactly as [`Self::embed`] embeds it.
    pub fn embed_with_images(&self, inputs: &[(&str, Option<&ImageTokens>)]) -> Result<Vec<Vec<f32>>, ModelError> {
        let mut states = Vec::with_capacity(inputs.len());
        let ple_projection = self.matrix("ple.per_layer_model_projection.weight")?;
        let ple_norm = self.matrix("ple.per_layer_projection_norm.weight")?;
        for (text, image) in inputs {
            let ids = self.tokens(text)?;
            let embedded = match image {
                Some(image) => self.with_image(&ids, image)?,
                None => (self.token_rows(&ids)? * EMBED_SCALE)?,
            };
            let tokens = embedded.dim(0)?;
            // Projection-only PLE: derived from the input embeddings alone.
            let per_layer = (embedded.matmul(&ple_projection.t()?)? * (HIDDEN as f64).powf(-0.5))?;
            let per_layer = rms_norm(&per_layer.reshape((tokens, LAYERS, PER_LAYER_INPUT))?, Some(&ple_norm))?;
            states.push(State { rope: [Rope::new(SLIDING, tokens)?, Rope::new(GLOBAL, tokens)?], hidden: embedded, per_layer });
        }
        for layer in 0..LAYERS {
            let weights = Layer::load(self, layer)?;
            for state in &mut states {
                let rope = &state.rope[usize::from(!weights.kind.sliding)];
                state.hidden = weights.forward(&state.hidden, &state.per_layer.i((.., layer, ..))?, rope)?;
            }
        }
        let norm = self.matrix("norm.weight")?;
        let projection = self.matrix("embedding_projection.weight")?;
        states
            .iter()
            .map(|state| {
                let tokens = rms_norm(&state.hidden, Some(&norm))?.matmul(&projection.t()?)?;
                let pooled = tokens.mean(0)?;
                let unit = pooled.broadcast_div(&pooled.sqr()?.sum_all()?.sqrt()?)?;
                Ok(unit.to_vec1::<f32>()?)
            })
            .collect()
    }

    /// `<bos> text <eos>` as the tokenizer's template writes it, capped.
    fn tokens(&self, text: &str) -> Result<Vec<u32>, ModelError> {
        let encoding = self.tokenizer.encode(text, true).map_err(|error| ModelError::Tokenizer(error.to_string()))?;
        let mut ids = encoding.get_ids().to_vec();
        ids.truncate(MAX_TOKENS);
        Ok(ids)
    }

    /// The input embeddings of a text whose one placeholder is expanded as the
    /// reference processor expands it, `<|image>`, a placeholder per soft
    /// token, `<image|>`, with the soft tokens themselves (unscaled) at the
    /// placeholders.
    fn with_image(&self, ids: &[u32], image: &ImageTokens) -> Result<Tensor, ModelError> {
        let id = |token: &str| self.tokenizer.token_to_id(token).ok_or_else(|| ModelError::Tokenizer(format!("no {token} token")));
        let (placeholder, open, close) = (id(IMAGE_PLACEHOLDER)?, id(IMAGE_OPEN)?, id(IMAGE_CLOSE)?);
        let found: Vec<usize> = ids.iter().enumerate().filter(|(_, token)| **token == placeholder).map(|(at, _)| at).collect();
        let [at] = found[..] else {
            return Err(ModelError::Placeholders { found: found.len(), wanted: 1 });
        };
        let before = [&ids[..at], &[open]].concat();
        let after = [&[close], &ids[at + 1..]].concat();
        let before = (self.token_rows(&before)? * EMBED_SCALE)?;
        let after = (self.token_rows(&after)? * EMBED_SCALE)?;
        Ok(Tensor::cat(&[&before, image.tensor(), &after], 0)?)
    }

    /// The token table's rows for `ids`, read straight from the mapped file.
    fn token_rows(&self, ids: &[u32]) -> Result<Tensor, ModelError> {
        let name = "language_model.embed_tokens.weight";
        let (data, _) = bf16(&self.weights, name)?;
        let row_bytes = HIDDEN * 2;
        let rows = ids
            .iter()
            .map(|id| {
                let start = *id as usize * row_bytes;
                let bytes =
                    data.get(start..start + row_bytes).ok_or_else(|| candle_core::Error::Msg(format!("token {id} is outside {name}")))?;
                Tensor::from_raw_buffer(bytes, DType::BF16, &[1, HIDDEN], &self.device)
            })
            .collect::<candle_core::Result<Vec<_>>>()?;
        Ok(Tensor::cat(&rows, 0)?.to_dtype(DType::F32)?)
    }

    /// A text-model tensor as float32.
    fn matrix(&self, name: &str) -> Result<Tensor, ModelError> {
        float32(&self.weights, &format!("language_model.{name}"))
    }
}

/// A mapped bfloat16 tensor's bytes and shape.
pub(super) fn bf16<'a>(weights: &'a MmapedSafetensors, name: &str) -> Result<(&'a [u8], Vec<usize>), ModelError> {
    let view = weights.get(name)?;
    match DType::try_from(view.dtype()) {
        Ok(DType::BF16) => Ok((view.data(), view.shape().to_vec())),
        found => Err(ModelError::Dtype { name: name.to_string(), found: format!("{found:?}") }),
    }
}

/// A mapped bfloat16 tensor as float32.
pub(super) fn float32(weights: &MmapedSafetensors, name: &str) -> Result<Tensor, ModelError> {
    let (data, shape) = bf16(weights, name)?;
    Ok(Tensor::from_raw_buffer(data, DType::BF16, &shape, &Device::Cpu)?.to_dtype(DType::F32)?)
}

/// One text on its way through the layers.
struct State {
    /// Sliding, then global.
    rope: [Rope; 2],
    hidden: Tensor,
    /// `[tokens, layers, 512]`.
    per_layer: Tensor,
}

/// Rotary cos/sin tables for one text length and one layer kind, `[tokens, head_dim]`.
struct Rope {
    cos: Tensor,
    sin: Tensor,
}

impl Rope {
    fn new(kind: Kind, tokens: usize) -> Result<Self, ModelError> {
        let half = kind.head_dim / 2;
        let inv_freq: Vec<f32> = (0..half).map(|i| (1.0 / kind.rope_theta.powf(2.0 * i as f64 / kind.head_dim as f64)) as f32).collect();
        let inv_freq = Tensor::from_vec(inv_freq, (1, half), &Device::Cpu)?;
        let positions = Tensor::arange(0u32, tokens as u32, &Device::Cpu)?.to_dtype(DType::F32)?.reshape((tokens, 1))?;
        let freqs = positions.broadcast_mul(&inv_freq)?;
        let angles = Tensor::cat(&[&freqs, &freqs], D::Minus1)?;
        Ok(Self { cos: angles.cos()?, sin: angles.sin()? })
    }

    /// `x·cos + rotate_half(x)·sin` over `[heads, tokens, head_dim]`.
    fn apply(&self, x: &Tensor) -> Result<Tensor, ModelError> {
        let half = x.dim(D::Minus1)? / 2;
        let rotated = Tensor::cat(&[&x.narrow(D::Minus1, half, half)?.neg()?, &x.narrow(D::Minus1, 0, half)?], D::Minus1)?;
        Ok((x.broadcast_mul(&self.cos)? + rotated.broadcast_mul(&self.sin)?)?)
    }
}

/// One encoder layer's weights, float32.
struct Layer {
    kind: Kind,
    input_norm: Tensor,
    post_attention_norm: Tensor,
    pre_feedforward_norm: Tensor,
    post_feedforward_norm: Tensor,
    q: Tensor,
    k: Tensor,
    v: Tensor,
    o: Tensor,
    q_norm: Tensor,
    k_norm: Tensor,
    gate: Tensor,
    up: Tensor,
    down: Tensor,
    ple_gate: Tensor,
    ple_projection: Tensor,
    ple_norm: Tensor,
    scalar: Tensor,
}

impl Layer {
    fn load(encoder: &Encoder, layer: usize) -> Result<Self, ModelError> {
        let get = |name: &str| encoder.matrix(&format!("layers.{layer}.{name}"));
        Ok(Self {
            kind: kind(layer),
            input_norm: get("input_layernorm.weight")?,
            post_attention_norm: get("post_attention_layernorm.weight")?,
            pre_feedforward_norm: get("pre_feedforward_layernorm.weight")?,
            post_feedforward_norm: get("post_feedforward_layernorm.weight")?,
            q: get("self_attn.q_proj.weight")?,
            k: get("self_attn.k_proj.weight")?,
            v: get("self_attn.v_proj.weight")?,
            o: get("self_attn.o_proj.weight")?,
            q_norm: get("self_attn.q_norm.weight")?,
            k_norm: get("self_attn.k_norm.weight")?,
            gate: get("mlp.gate_proj.weight")?,
            up: get("mlp.up_proj.weight")?,
            down: get("mlp.down_proj.weight")?,
            ple_gate: get("ple_block.per_layer_input_gate.weight")?,
            ple_projection: get("ple_block.per_layer_projection.weight")?,
            ple_norm: get("ple_block.post_per_layer_input_norm.weight")?,
            scalar: get("layer_scalar")?,
        })
    }

    /// Attention, MLP and PLE sub-blocks, each with its own residual.
    fn forward(&self, hidden: &Tensor, per_layer: &Tensor, rope: &Rope) -> Result<Tensor, ModelError> {
        let attended = self.attention(&rms_norm(hidden, Some(&self.input_norm))?, rope)?;
        let hidden = (hidden + rms_norm(&attended, Some(&self.post_attention_norm))?)?;

        let normed = rms_norm(&hidden, Some(&self.pre_feedforward_norm))?;
        let mlp = (linear(&normed, &self.gate)?.gelu()? * linear(&normed, &self.up)?)?;
        let hidden = (&hidden + rms_norm(&linear(&mlp, &self.down)?, Some(&self.post_feedforward_norm))?)?;

        let gated = (linear(&hidden, &self.ple_gate)?.gelu()? * per_layer)?;
        let hidden = (&hidden + rms_norm(&linear(&gated, &self.ple_projection)?, Some(&self.ple_norm))?)?;
        Ok(hidden.broadcast_mul(&self.scalar)?)
    }

    /// Bidirectional grouped-query attention with QK and V norms, scale 1.
    fn attention(&self, x: &Tensor, rope: &Rope) -> Result<Tensor, ModelError> {
        let Kind { head_dim, kv_heads, sliding, .. } = self.kind;
        let tokens = x.dim(0)?;
        let heads = |weight: &Tensor, count: usize| -> Result<Tensor, ModelError> {
            Ok(linear(x, weight)?.reshape((tokens, count, head_dim))?.transpose(0, 1)?.contiguous()?)
        };
        let q = rope.apply(&rms_norm(&heads(&self.q, HEADS)?, Some(&self.q_norm))?)?;
        let k = rope.apply(&rms_norm(&heads(&self.k, kv_heads)?, Some(&self.k_norm))?)?;
        let v = rms_norm(&heads(&self.v, kv_heads)?, None)?;
        let groups = HEADS / kv_heads;
        let repeat = |t: Tensor| -> candle_core::Result<Tensor> {
            t.unsqueeze(1)?.broadcast_as((kv_heads, groups, tokens, head_dim))?.contiguous()?.reshape((HEADS, tokens, head_dim))
        };
        let (k, v) = (repeat(k)?, repeat(v)?);
        let mut scores = q.matmul(&k.t()?)?;
        if sliding && tokens > SLIDING_WINDOW + 1 {
            scores = scores.broadcast_add(&window_mask(tokens)?)?;
        }
        let max = scores.max_keepdim(D::Minus1)?;
        let weights = scores.broadcast_sub(&max)?.exp()?;
        let weights = weights.broadcast_div(&weights.sum_keepdim(D::Minus1)?)?;
        let out = weights.matmul(&v)?.transpose(0, 1)?.contiguous()?.reshape((tokens, HEADS * head_dim))?;
        Ok(linear(&out, &self.o)?)
    }
}

/// 0 within the bidirectional window, −∞ outside it, `[tokens, tokens]`.
fn window_mask(tokens: usize) -> candle_core::Result<Tensor> {
    let mask: Vec<f32> =
        (0..tokens).flat_map(|q| (0..tokens).map(move |k| if q.abs_diff(k) <= SLIDING_WINDOW { 0.0 } else { f32::NEG_INFINITY })).collect();
    Tensor::from_vec(mask, (tokens, tokens), &Device::Cpu)
}

/// `x · Wᵀ` for a weight stored `[out, in]`.
pub(super) fn linear(x: &Tensor, weight: &Tensor) -> candle_core::Result<Tensor> {
    x.broadcast_matmul(&weight.t()?)
}

/// `x / sqrt(mean(x²) + ε)`, times `weight` when the norm has one.
pub(super) fn rms_norm(x: &Tensor, weight: Option<&Tensor>) -> candle_core::Result<Tensor> {
    let normed = x.broadcast_div(&(x.sqr()?.mean_keepdim(D::Minus1)? + EPS)?.sqrt()?)?;
    match weight {
        Some(weight) => normed.broadcast_mul(weight),
        None => Ok(normed),
    }
}
