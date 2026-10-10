//! Drawing the "LEAVES OCT 23" band onto a poster. The text comes from a
//! fixed alphabet (LEAVES, the month abbreviations, digits), so a built-in
//! 5×7 bitmap font scaled up covers it: no font rasterizer crate and no font
//! file (with its licence) to ship, only the `image` crate already in tree.

use image::codecs::jpeg::JpegEncoder;
use image::{Rgb, RgbImage};

const GLYPH_W: u32 = 5;
const GLYPH_H: u32 = 7;
/// Band colour (deep red) and its opacity over the poster, in percent.
const BAND: [u8; 3] = [178, 24, 32];
const BAND_ALPHA: u16 = 88;
const JPEG_QUALITY: u8 = 90;

/// Rows top to bottom, the five low bits left to right.
fn glyph(c: char) -> [u8; 7] {
    match c {
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'Y' => [0x11, 0x11, 0x11, 0x0A, 0x04, 0x04, 0x04],
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        _ => [0; 7],
    }
}

/// The poster with `text` (upper-cased) in a band across its bottom, as JPEG.
pub fn draw(poster: &[u8], text: &str) -> Result<Vec<u8>, image::ImageError> {
    let mut image = image::load_from_memory(poster)?.to_rgb8();
    stamp(&mut image, &text.to_uppercase());
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY).encode_image(&image)?;
    Ok(out)
}

fn stamp(image: &mut RgbImage, text: &str) {
    let (width, height) = image.dimensions();
    let band = (height / 9).max(GLYPH_H + 2);
    let top = height.saturating_sub(band);
    for y in top..height {
        for x in 0..width {
            let pixel = image.get_pixel_mut(x, y);
            for (channel, band) in pixel.0.iter_mut().zip(BAND) {
                *channel = ((u16::from(band) * BAND_ALPHA + u16::from(*channel) * (100 - BAND_ALPHA)) / 100) as u8;
            }
        }
    }
    let columns = (text.chars().count() as u32 * (GLYPH_W + 1)).saturating_sub(1).max(1);
    let scale = (band * 3 / 5 / GLYPH_H).min(width * 9 / 10 / columns).max(1);
    let left = width.saturating_sub(columns * scale) / 2;
    let baseline = top + band.saturating_sub(GLYPH_H * scale) / 2;
    for (i, c) in text.chars().enumerate() {
        let origin = left + i as u32 * (GLYPH_W + 1) * scale;
        for (row, bits) in glyph(c).iter().enumerate() {
            for col in 0..GLYPH_W {
                if bits & (0x10 >> col) == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let (x, y) = (origin + col * scale + dx, baseline + row as u32 * scale + dy);
                        if x < width && y < height {
                            image.put_pixel(x, y, Rgb([255, 255, 255]));
                        }
                    }
                }
            }
        }
    }
}
