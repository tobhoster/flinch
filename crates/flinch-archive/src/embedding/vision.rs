//! EmbeddingGemma 2's vision tower, in candle, on the CPU.
//!
//! A port of `Gemma4VisionModel` (transformers `gemma4`) as
//! `EmbeddingGemma2Model.get_image_features` runs it: each 16-pixel patch is
//! projected and given learned x and y position embeddings, 16 bidirectional
//! layers with QK and V norms and 2-D rotary positions (the first half of each
//! head turned by the patch's column, the second by its row) refine them,
//! every 3×3 block of patches is averaged into one soft token, scaled by
//! √768, and the soft tokens are normed and projected into the text model's
//! 512-d input space (`embed_vision`). The text encoder takes them from there.
//!
//! Like the text encoder it computes in float32 on bfloat16 weights that stay
//! memory-mapped and are read one layer at a time; attention runs one head at
//! a time so a 2 340-patch poster needs one 22 MB score matrix, not twelve.

use super::gemma::{bf16, float32, linear, rms_norm, ModelError};
use super::poster::{Patches, Preprocessing, PATCH_VALUES, POOL};
use super::weights::ModelFiles;
use candle_core::safetensors::MmapedSafetensors;
use candle_core::{CpuStorage, CustomOp1, DType, Device, Layout, Shape, Tensor, D};

const HIDDEN: usize = 768;
const LAYERS: usize = 16;
const HEADS: usize = 12;
const HEAD_DIM: usize = 64;
const ROPE_THETA: f32 = 100.0;
/// Rows of each axis' position-embedding table.
const POSITIONS: usize = 10_240;

/// An image as the text model takes it: one 512-d input row per soft token.
#[derive(Debug, Clone)]
pub struct ImageTokens(Tensor);

impl ImageTokens {
    pub(super) fn tensor(&self) -> &Tensor {
        &self.0
    }
}

pub struct VisionEncoder {
    weights: MmapedSafetensors,
    preprocessing: Preprocessing,
}

impl VisionEncoder {
    /// Map the vision weights and read the image processor's settings.
    pub fn open(files: &ModelFiles) -> Result<Self, ModelError> {
        let preprocessing = Preprocessing::read(&files.processor_config()).map_err(|error| ModelError::Processor(error.to_string()))?;
        // SAFETY: as for the text weights, FLINCH's own download, renamed into
        // place whole and never written again.
        let weights = unsafe { MmapedSafetensors::new(files.vision_weights())? };
        Ok(Self { weights, preprocessing })
    }

    /// How images must be cut into patches for this model.
    pub fn preprocessing(&self) -> Preprocessing {
        self.preprocessing
    }

    /// The soft tokens of one image, row by row of 3×3 patch blocks.
    pub fn encode(&self, patches: &Patches) -> Result<ImageTokens, ModelError> {
        if patches.is_empty() || !patches.rows.is_multiple_of(POOL) || !patches.columns.is_multiple_of(POOL) {
            return Err(ModelError::Processor(format!("{}×{} patches do not pool into 3×3 blocks", patches.rows, patches.columns)));
        }
        let count = patches.len();
        let pixels = Tensor::from_slice(&patches.pixels, (count, PATCH_VALUES), &Device::Cpu)?;
        // The reference scales 0–1 to −1–1 in the model, not the processor.
        let pixels = pixels.affine(2.0, -1.0)?;
        let mut hidden = (linear(&pixels, &self.matrix("vision_tower.patch_embedder.input_proj.weight")?)? + self.positions(patches)?)?;
        let rope = AxialRope::new(patches)?;
        for layer in 0..LAYERS {
            hidden = Layer::load(self, layer)?.forward(&hidden, &rope)?;
        }
        // Mean of each 3×3 block, blocks in row-major order, then √768.
        let (rows, columns) = (patches.rows / POOL, patches.columns / POOL);
        let pooled = hidden.reshape((rows, POOL, columns, POOL, HIDDEN))?.sum(3)?.sum(1)?.reshape((rows * columns, HIDDEN))?;
        let pooled = (pooled * ((HIDDEN as f64).sqrt() / (POOL * POOL) as f64))?;
        let projection = self.matrix("embed_vision.embedding_projection.weight")?;
        Ok(ImageTokens(linear(&rms_norm(&pooled, None)?, &projection)?))
    }

    /// Each patch's x-embedding plus its y-embedding, read row by row from the
    /// mapped `[2, 10240, 768]` table.
    fn positions(&self, patches: &Patches) -> Result<Tensor, ModelError> {
        let name = "vision_tower.patch_embedder.position_embedding_table";
        let (data, _) = bf16(&self.weights, name)?;
        let row_bytes = HIDDEN * 2;
        let row = |axis: usize, index: usize| -> candle_core::Result<Tensor> {
            let start = (axis * POSITIONS + index) * row_bytes;
            let bytes = data
                .get(start..start + row_bytes)
                .filter(|_| index < POSITIONS)
                .ok_or_else(|| candle_core::Error::Msg(format!("patch position {index} is outside {name}")))?;
            Tensor::from_raw_buffer(bytes, DType::BF16, &[1, HIDDEN], &Device::Cpu)?.to_dtype(DType::F32)
        };
        let columns = (0..patches.columns).map(|x| row(0, x)).collect::<candle_core::Result<Vec<_>>>()?;
        let rows = (0..patches.rows).map(|y| row(1, y)).collect::<candle_core::Result<Vec<_>>>()?;
        let sums = patches.positions().map(|(x, y)| &columns[x] + &rows[y]).collect::<candle_core::Result<Vec<_>>>()?;
        Ok(Tensor::cat(&sums, 0)?)
    }

    fn matrix(&self, name: &str) -> Result<Tensor, ModelError> {
        float32(&self.weights, name)
    }
}

/// Rotary cos/sin tables `[patches, 1, 64]`: the first 32 channels of a head
/// turn with the patch's column, the last 32 with its row, at 16 frequencies
/// each (Gemma 4's axial rope, `H-H-W-W` layout).
struct AxialRope {
    cos: Tensor,
    sin: Tensor,
}

impl AxialRope {
    fn new(patches: &Patches) -> Result<Self, ModelError> {
        let spatial = HEAD_DIM / 2;
        let inv_freq: Vec<f32> = (0..spatial / 2).map(|i| 1.0 / ROPE_THETA.powf((2 * i) as f32 / spatial as f32)).collect();
        let mut angles = Vec::with_capacity(patches.len() * HEAD_DIM);
        for (x, y) in patches.positions() {
            for position in [x, x, y, y] {
                angles.extend(inv_freq.iter().map(|frequency| position as f32 * frequency));
            }
        }
        let angles = Tensor::from_vec(angles, (patches.len(), 1, HEAD_DIM), &Device::Cpu)?;
        Ok(Self { cos: angles.cos()?, sin: angles.sin()? })
    }

    /// Each half of `[patches, heads, 64]` rotated on its own:
    /// `x·cos + rotate_half(x)·sin` per 32-channel half.
    fn apply(&self, x: &Tensor) -> Result<Tensor, ModelError> {
        let quarter = HEAD_DIM / 4;
        let part = |index: usize| x.narrow(D::Minus1, index * quarter, quarter);
        let rotated = Tensor::cat(&[&part(1)?.neg()?, &part(0)?, &part(3)?.neg()?, &part(2)?], D::Minus1)?;
        Ok((x.broadcast_mul(&self.cos)? + rotated.broadcast_mul(&self.sin)?)?)
    }
}

/// One encoder layer's weights, float32.
struct Layer {
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
}

impl Layer {
    fn load(encoder: &VisionEncoder, layer: usize) -> Result<Self, ModelError> {
        let get = |name: &str| encoder.matrix(&format!("vision_tower.encoder.layers.{layer}.{name}"));
        Ok(Self {
            input_norm: get("input_layernorm.weight")?,
            post_attention_norm: get("post_attention_layernorm.weight")?,
            pre_feedforward_norm: get("pre_feedforward_layernorm.weight")?,
            post_feedforward_norm: get("post_feedforward_layernorm.weight")?,
            q: get("self_attn.q_proj.linear.weight")?,
            k: get("self_attn.k_proj.linear.weight")?,
            v: get("self_attn.v_proj.linear.weight")?,
            o: get("self_attn.o_proj.linear.weight")?,
            q_norm: get("self_attn.q_norm.weight")?,
            k_norm: get("self_attn.k_norm.weight")?,
            gate: get("mlp.gate_proj.linear.weight")?,
            up: get("mlp.up_proj.linear.weight")?,
            down: get("mlp.down_proj.linear.weight")?,
        })
    }

    /// Attention and MLP sub-blocks, each normed before and after, with residuals.
    fn forward(&self, hidden: &Tensor, rope: &AxialRope) -> Result<Tensor, ModelError> {
        let attended = self.attention(&rms_norm(hidden, Some(&self.input_norm))?, rope)?;
        let hidden = (hidden + rms_norm(&attended, Some(&self.post_attention_norm))?)?;
        let normed = rms_norm(&hidden, Some(&self.pre_feedforward_norm))?;
        let mlp = (linear(&normed, &self.gate)?.gelu()? * linear(&normed, &self.up)?)?;
        Ok((&hidden + rms_norm(&linear(&mlp, &self.down)?, Some(&self.post_feedforward_norm))?)?)
    }

    /// Bidirectional multi-head attention over every patch, scale 1.
    fn attention(&self, x: &Tensor, rope: &AxialRope) -> Result<Tensor, ModelError> {
        let count = x.dim(0)?;
        let heads = |weight: &Tensor| -> candle_core::Result<Tensor> { linear(x, weight)?.reshape((count, HEADS, HEAD_DIM)) };
        let to_heads = |t: Tensor| -> candle_core::Result<Tensor> { t.transpose(0, 1)?.contiguous() };
        let q = to_heads(rope.apply(&rms_norm(&heads(&self.q)?, Some(&self.q_norm))?)?)?;
        let k = to_heads(rope.apply(&rms_norm(&heads(&self.k)?, Some(&self.k_norm))?)?)?;
        let v = to_heads(rms_norm(&heads(&self.v)?, None)?)?;
        let outputs = (0..HEADS)
            .map(|head| -> candle_core::Result<Tensor> {
                let (q, k, v) = (q.get(head)?, k.get(head)?, v.get(head)?);
                let weights = q.matmul(&k.t()?)?.apply_op1_no_bwd(&RowSoftmax)?;
                weights.matmul(&v)
            })
            .collect::<candle_core::Result<Vec<_>>>()?;
        Ok(linear(&Tensor::cat(&outputs, 1)?, &self.o)?)
    }
}

/// Softmax over the last axis of a contiguous float32 tensor, its rows spread
/// over the CPU's threads. Candle runs element-wise ops on one thread, and a
/// poster's attention rows (2 340 × 2 340 per head, 192 heads) are most of
/// the vision tower's element-wise work.
struct RowSoftmax;

impl CustomOp1 for RowSoftmax {
    fn name(&self) -> &'static str {
        "row-softmax"
    }

    fn cpu_fwd(&self, storage: &CpuStorage, layout: &Layout) -> candle_core::Result<(CpuStorage, Shape)> {
        let (CpuStorage::F32(data), Some((start, end))) = (storage, layout.contiguous_offsets()) else {
            return Err(candle_core::Error::Msg("row softmax takes a contiguous float32 tensor".into()));
        };
        let width = layout.dims().last().copied().unwrap_or(1).max(1);
        let mut out = data[start..end].to_vec();
        let threads = std::thread::available_parallelism().map_or(1, usize::from);
        let rows_per_thread = (out.len() / width).div_ceil(threads).max(1);
        std::thread::scope(|scope| {
            for rows in out.chunks_mut(rows_per_thread * width) {
                scope.spawn(move || rows.chunks_mut(width).for_each(softmax));
            }
        });
        Ok((CpuStorage::F32(out), layout.shape().clone()))
    }
}

/// `exp(x − max) / Σ exp(x − max)` in place.
fn softmax(row: &mut [f32]) {
    let max = row.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut sum = 0.0f32;
    for value in row.iter_mut() {
        *value = (*value - max).exp();
        sum += *value;
    }
    for value in row.iter_mut() {
        *value /= sum;
    }
}
