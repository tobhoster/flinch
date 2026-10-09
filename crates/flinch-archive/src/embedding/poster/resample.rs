//! Pillow's bicubic resize, step for step.
//!
//! The reference image processor resizes with `PIL.Image.resize(…, BICUBIC)`,
//! and the vision tower is sensitive to the result: a generic Catmull-Rom
//! resize, a few levels off here and there, moved single soft tokens to a
//! cosine of 0.84 with the reference's. This is Pillow's `ImagingResample` for
//! 8-bit RGB: a horizontal pass, then a vertical one, each skipped when that
//! side keeps its length; per output pixel a cubic (a = −0.5) kernel
//! stretched by the downscale factor, normalised, and applied in 22-bit fixed
//! point with rounding and clamping to 8 bits after each pass.

use image::RgbImage;

const PRECISION_BITS: u32 = 32 - 8 - 2;
/// The cubic's reach in source pixels, before stretching.
const SUPPORT: f64 = 2.0;

/// `image` resized to `width` × `height` as Pillow resizes it.
pub(super) fn bicubic(image: &RgbImage, width: u32, height: u32) -> RgbImage {
    let (in_width, in_height) = (image.width() as usize, image.height() as usize);
    let (width, height) = (width as usize, height as usize);
    let mut pixels = image.as_raw().clone();
    if width != in_width {
        pixels = pass(&pixels, in_width, in_height, &taps(in_width, width), Axis::Horizontal);
    }
    if height != in_height {
        pixels = pass(&pixels, width, in_height, &taps(in_height, height), Axis::Vertical);
    }
    RgbImage::from_raw(width as u32, height as u32, pixels).unwrap_or_default()
}

/// Keys' cubic with a = −0.5, as Pillow's `bicubic_filter`.
fn cubic(x: f64) -> f64 {
    const A: f64 = -0.5;
    let x = x.abs();
    if x < 1.0 {
        ((A + 2.0) * x - (A + 3.0)) * x * x + 1.0
    } else if x < 2.0 {
        (((x - 5.0) * x + 8.0) * x - 4.0) * A
    } else {
        0.0
    }
}

/// Per output index along one side, the first source index and the
/// fixed-point weights of the source run from it (`precompute_coeffs` and
/// `normalize_coeffs_8bpc`).
fn taps(input: usize, output: usize) -> Vec<(usize, Vec<i64>)> {
    let scale = input as f64 / output as f64;
    let stretch = scale.max(1.0);
    let support = SUPPORT * stretch;
    let one = f64::from(1u32 << PRECISION_BITS);
    (0..output)
        .map(|index| {
            let center = (index as f64 + 0.5) * scale;
            // C's `(int)` truncates toward zero; the clamps make that a floor here.
            let first = ((center - support + 0.5) as i64).max(0) as usize;
            let end = ((center + support + 0.5) as i64).min(input as i64).max(first as i64) as usize;
            let weights: Vec<f64> = (first..end).map(|source| cubic((source as f64 - center + 0.5) / stretch)).collect();
            let total: f64 = weights.iter().sum();
            let fixed = weights
                .iter()
                .map(|weight| {
                    let weight = if total == 0.0 { *weight } else { weight / total };
                    (if weight < 0.0 { -0.5 + weight * one } else { 0.5 + weight * one }) as i64
                })
                .collect();
            (first, fixed)
        })
        .collect()
}

#[derive(Clone, Copy)]
enum Axis {
    Horizontal,
    Vertical,
}

/// One pass over interleaved RGB `width` × `height`, along `axis`.
fn pass(source: &[u8], width: usize, height: usize, taps: &[(usize, Vec<i64>)], axis: Axis) -> Vec<u8> {
    let (out_width, out_height) = match axis {
        Axis::Horizontal => (taps.len(), height),
        Axis::Vertical => (width, taps.len()),
    };
    let mut out = vec![0u8; out_width * out_height * 3];
    for y in 0..out_height {
        for x in 0..out_width {
            let (first, weights) = match axis {
                Axis::Horizontal => &taps[x],
                Axis::Vertical => &taps[y],
            };
            for channel in 0..3 {
                let mut sum: i64 = 1 << (PRECISION_BITS - 1);
                for (offset, weight) in weights.iter().enumerate() {
                    let (sx, sy) = match axis {
                        Axis::Horizontal => (first + offset, y),
                        Axis::Vertical => (x, first + offset),
                    };
                    sum += i64::from(source[(sy * width + sx) * 3 + channel]) * weight;
                }
                out[(y * out_width + x) * 3 + channel] = (sum >> PRECISION_BITS).clamp(0, 255) as u8;
            }
        }
    }
    out
}
