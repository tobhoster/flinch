//! A poster as the vision tower sees it: decoded, resized and cut into patches.
//!
//! A port of the Gemma 4 image processor EmbeddingGemma 2 ships with
//! (`processor_config.json` `image_processor`, its Pillow backend): convert
//! to RGB, resize with Pillow's bicubic filter (see [`resample`]) to the
//! largest size that keeps the aspect ratio, fits the soft-token budget and
//! divides into 3×3 blocks of 16-pixel patches, scale to 0–1 and cut into
//! patches of 16·16·3 values in (row, column, channel) order, listed row by
//! row. Nothing is padded: the reference pads to the budget and then masks the
//! padding out of every step, which is the same arithmetic on fewer rows.
//!
//! Only pure-Rust codecs for what TMDB and TheTVDB serve (JPEG, PNG, WebP) are
//! built in, and decoding is bounded so a hostile file cannot take the
//! daemon's memory.

use image::{ImageReader, Limits, RgbImage};
use std::io::Cursor;
use std::path::Path;

mod resample;

/// Pixels per patch side.
pub const PATCH: usize = 16;
/// Patches per soft-token side: each soft token averages a 3×3 block.
pub const POOL: usize = 3;
/// Values per patch: 16 × 16 pixels × RGB.
pub const PATCH_VALUES: usize = PATCH * PATCH * 3;
/// The soft-token budgets the model was trained with.
const SOFT_TOKEN_BUDGETS: [usize; 5] = [70, 140, 280, 560, 1120];
/// A poster wider or taller than this is refused before it is decoded.
const MAX_SIDE: u32 = 12_000;
/// The most a decode may allocate.
const MAX_DECODE_BYTES: u64 = 256 << 20;

#[derive(Debug, thiserror::Error)]
pub enum PosterError {
    #[error("poster unreadable: {0}")]
    Decode(#[from] image::ImageError),
    #[error("poster of {width}×{height} pixels is too small for one 48-pixel block")]
    TooSmall { width: u32, height: u32 },
    #[error("image processor settings unusable: {0}")]
    Config(String),
}

/// How the pinned revision turns an image into patches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preprocessing {
    /// Most soft tokens per image; the image is resized to fit.
    pub max_soft_tokens: usize,
}

impl Default for Preprocessing {
    fn default() -> Self {
        Self { max_soft_tokens: 280 }
    }
}

impl Preprocessing {
    /// Read `image_processor` from a downloaded `processor_config.json`,
    /// refusing settings this port does not implement.
    pub fn read(path: &Path) -> Result<Self, PosterError> {
        let text = std::fs::read_to_string(path).map_err(|error| PosterError::Config(error.to_string()))?;
        let config: serde_json::Value = serde_json::from_str(&text).map_err(|error| PosterError::Config(error.to_string()))?;
        Self::from_config(&config["image_processor"])
    }

    fn from_config(config: &serde_json::Value) -> Result<Self, PosterError> {
        let number = |key: &str| config[key].as_f64().ok_or_else(|| PosterError::Config(format!("no {key}")));
        let flag = |key: &str| config[key].as_bool().ok_or_else(|| PosterError::Config(format!("no {key}")));
        let expected = [
            ("patch_size", number("patch_size")? == PATCH as f64),
            ("pooling_kernel_size", number("pooling_kernel_size")? == POOL as f64),
            // PIL's BICUBIC, the Catmull-Rom cubic.
            ("resample", number("resample")? == 3.0),
            ("rescale_factor", (number("rescale_factor")? - 1.0 / 255.0).abs() < 1e-9),
            ("do_rescale", flag("do_rescale")?),
            ("do_resize", flag("do_resize")?),
            ("do_normalize", !flag("do_normalize")?),
        ];
        if let Some((key, _)) = expected.iter().find(|(_, ok)| !ok) {
            return Err(PosterError::Config(format!("{key} differs from what this port implements")));
        }
        let max_soft_tokens = number("max_soft_tokens")? as usize;
        if !SOFT_TOKEN_BUDGETS.contains(&max_soft_tokens) {
            return Err(PosterError::Config(format!("max_soft_tokens {max_soft_tokens} is not a trained budget")));
        }
        Ok(Self { max_soft_tokens })
    }

    /// The `(height, width)` an image is resized to: the largest that keeps
    /// its aspect ratio, has at most `max_soft_tokens · 9` patches and sides
    /// that are multiples of 48 (Gemma 4's `get_aspect_ratio_preserving_size`,
    /// to the same floating-point steps).
    pub fn target_size(&self, height: u32, width: u32) -> Result<(u32, u32), PosterError> {
        let max_patches = self.max_soft_tokens * POOL * POOL;
        let target_pixels = (max_patches * PATCH * PATCH) as f64;
        let factor = (target_pixels / (f64::from(height) * f64::from(width))).sqrt();
        let side = (POOL * PATCH) as f64;
        let mut target_height = (factor * f64::from(height) / side).floor() * side;
        let mut target_width = (factor * f64::from(width) / side).floor() * side;
        let max_side = ((max_patches / (POOL * POOL)) as f64) * side;
        if target_height == 0.0 && target_width == 0.0 {
            return Err(PosterError::TooSmall { width, height });
        } else if target_height == 0.0 {
            target_height = side;
            target_width = ((f64::from(width) / f64::from(height)).floor() * side).min(max_side);
        } else if target_width == 0.0 {
            target_width = side;
            target_height = ((f64::from(height) / f64::from(width)).floor() * side).min(max_side);
        }
        if target_height * target_width > target_pixels {
            return Err(PosterError::TooSmall { width, height });
        }
        Ok((target_height as u32, target_width as u32))
    }

    /// Decode an image file (JPEG, PNG or WebP) and cut it into patches.
    pub fn patches_of_file(&self, bytes: &[u8]) -> Result<Patches, PosterError> {
        let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().map_err(image::ImageError::IoError)?;
        let mut limits = Limits::default();
        limits.max_image_width = Some(MAX_SIDE);
        limits.max_image_height = Some(MAX_SIDE);
        limits.max_alloc = Some(MAX_DECODE_BYTES);
        reader.limits(limits);
        // RGB as Pillow's `convert("RGB")` makes it: any alpha is dropped.
        self.patches(&reader.decode()?.into_rgb8())
    }

    /// Resize an RGB image and cut it into patches.
    pub fn patches(&self, image: &RgbImage) -> Result<Patches, PosterError> {
        let (height, width) = self.target_size(image.height(), image.width())?;
        let resized;
        let image = if (width, height) == image.dimensions() {
            image
        } else {
            resized = resample::bicubic(image, width, height);
            &resized
        };
        let (rows, columns) = (height as usize / PATCH, width as usize / PATCH);
        let mut pixels = Vec::with_capacity(rows * columns * PATCH_VALUES);
        for row in 0..rows {
            for column in 0..columns {
                for y in 0..PATCH {
                    for x in 0..PATCH {
                        let pixel = image.get_pixel((column * PATCH + x) as u32, (row * PATCH + y) as u32);
                        // Rescaled in float64, then stored as float32, as the reference does.
                        pixels.extend(pixel.0.iter().map(|value| (f64::from(*value) * (1.0 / 255.0)) as f32));
                    }
                }
            }
        }
        Ok(Patches { pixels, rows, columns })
    }
}

/// An image cut into patches, row by row.
#[derive(Debug, Clone, PartialEq)]
pub struct Patches {
    /// `rows · columns · 768` values in 0–1, one patch after another.
    pub pixels: Vec<f32>,
    pub rows: usize,
    pub columns: usize,
}

impl Patches {
    pub fn len(&self) -> usize {
        self.rows * self.columns
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Soft tokens the vision tower makes of them: one per 3×3 block.
    pub fn soft_tokens(&self) -> usize {
        self.len() / (POOL * POOL)
    }

    /// Each patch's `(x, y)`: its column and row.
    pub fn positions(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        (0..self.rows).flat_map(move |row| (0..self.columns).map(move |column| (column, row)))
    }
}
