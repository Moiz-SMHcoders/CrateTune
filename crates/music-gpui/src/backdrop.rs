//! CPU-side artwork blur for the Now Playing background (GPUI has no blur filter).

use std::sync::Arc;

use gpui::{Hsla, RenderImage, Rgba};
use image::{Frame, RgbaImage, imageops};

/// Background colours taken from the artwork: [main, secondary, most vivid].
pub type Palette = [Hsla; 3];

/// Work size: blurring a downscaled copy is ~100x cheaper and the result is
/// upscaled anyway. A 9 px sigma here is a ~100 px blur at window size.
const WORK_SIZE: u32 = 96;
const SIGMA: f32 = 9.0;
/// Uploaded size; large enough that GPU bilinear upscaling shows no blockiness.
const OUT_SIZE: u32 = 320;

/// Decodes artwork bytes and returns a heavily blurred, slightly saturated and
/// darkened copy as a GPU-ready (BGRA) image, plus a colour palette for the
/// gradient / solid / glow backgrounds. Runs on a background thread.
pub fn blurred_backdrop(bytes: &[u8]) -> Result<(Arc<RenderImage>, Palette), image::ImageError> {
    let source = image::load_from_memory(bytes)?;
    let small = source.resize_to_fill(WORK_SIZE, WORK_SIZE, imageops::FilterType::Triangle);
    let palette = palette(&small.to_rgba8());
    let mut blurred = imageops::blur(&small.to_rgba8(), SIGMA);
    for px in blurred.pixels_mut() {
        let [r, g, b, _] = px.0.map(f32::from);
        // Saturate (+40%) around luma, then darken so white lyrics stay legible.
        let luma = 0.299 * r + 0.587 * g + 0.114 * b;
        let tone = |c: f32| ((luma + (c - luma) * 1.4) * 0.8).clamp(0.0, 255.0) as u8;
        px.0 = [tone(r), tone(g), tone(b), 255];
    }
    let mut out: RgbaImage = imageops::resize(
        &blurred,
        OUT_SIZE,
        OUT_SIZE,
        imageops::FilterType::CatmullRom,
    );
    // GPUI's sprite atlas expects BGRA.
    for px in out.pixels_mut() {
        px.0.swap(0, 2);
    }
    Ok((Arc::new(RenderImage::new(vec![Frame::new(out)])), palette))
}

/// Size of a glow blob image (upscaled a lot; the falloff is smooth).
const GLOW_SIZE: u32 = 64;

/// Soft round "light" in `color` with a Gaussian-like falloff to transparent,
/// for the animated glow background (GPUI has no radial gradients).
pub fn glow_blob(color: Hsla) -> Arc<RenderImage> {
    let rgba = Rgba::from(color);
    let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let half = GLOW_SIZE as f32 / 2.0;
    let mut image = RgbaImage::new(GLOW_SIZE, GLOW_SIZE);
    for (x, y, px) in image.enumerate_pixels_mut() {
        let dx = (x as f32 + 0.5 - half) / half;
        let dy = (y as f32 + 0.5 - half) / half;
        let d2 = dx * dx + dy * dy;
        // exp falloff, faded to exactly 0 at the edge so the square never shows.
        let alpha = (-d2 * 3.0).exp() * (1.0 - d2).max(0.0) * rgba.a;
        // BGRA, premultiplied-free (GPUI sprites are straight alpha).
        px.0 = [to_u8(rgba.b), to_u8(rgba.g), to_u8(rgba.r), to_u8(alpha)];
    }
    Arc::new(RenderImage::new(vec![Frame::new(image)]))
}

/// Average colour of the top-left and bottom-right quarters, and the most
/// saturated mid-tone pixel (falls back to the first colour on grey art).
pub fn palette(image: &RgbaImage) -> Palette {
    let (w, h) = image.dimensions();
    let average = |x0: u32, y0: u32, x1: u32, y1: u32| {
        let mut sum = [0f32; 3];
        let mut n = 0f32;
        for y in y0..y1.min(h) {
            for x in x0..x1.min(w) {
                let p = image.get_pixel(x, y).0;
                for c in 0..3 {
                    sum[c] += f32::from(p[c]);
                }
                n += 1.0;
            }
        }
        let n = n.max(1.0);
        hsla_of([sum[0] / n, sum[1] / n, sum[2] / n])
    };
    let first = average(0, 0, w / 2, h / 2);
    let second = average(w / 2, h / 2, w, h);
    let vivid = image
        .pixels()
        .step_by(7)
        .map(|p| hsla_of([p.0[0], p.0[1], p.0[2]].map(f32::from)))
        .filter(|c| (0.2..0.8).contains(&c.l))
        .max_by(|a, b| a.s.total_cmp(&b.s))
        .filter(|c| c.s > 0.25)
        .unwrap_or(first);
    [first, second, vivid]
}

fn hsla_of([r, g, b]: [f32; 3]) -> Hsla {
    Hsla::from(Rgba {
        r: r / 255.0,
        g: g / 255.0,
        b: b / 255.0,
        a: 1.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_of_a_two_colour_image() {
        let mut image = RgbaImage::new(8, 8);
        for (x, y, p) in image.enumerate_pixels_mut() {
            p.0 = if x < 4 && y < 4 {
                [200, 20, 20, 255]
            } else {
                [20, 20, 200, 255]
            };
        }
        let [first, second, vivid] = palette(&image);
        assert!(first.h < 0.05 || first.h > 0.95, "red: {first:?}");
        assert!((second.h - 0.667).abs() < 0.05, "blue: {second:?}");
        assert!(vivid.s > 0.5);
    }
}
