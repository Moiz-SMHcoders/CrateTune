//! CPU-side artwork blur for the Now Playing background (GPUI has no blur filter).

use std::sync::Arc;

use gpui::RenderImage;
use image::{Frame, RgbaImage, imageops};

/// Work size: blurring a downscaled copy is ~100x cheaper and the result is
/// upscaled anyway. A 9 px sigma here is a ~100 px blur at window size.
const WORK_SIZE: u32 = 96;
const SIGMA: f32 = 9.0;
/// Uploaded size; large enough that GPU bilinear upscaling shows no blockiness.
const OUT_SIZE: u32 = 320;

/// Decodes artwork bytes and returns a heavily blurred, slightly saturated and
/// darkened copy as a GPU-ready (BGRA) image. Runs on a background thread.
pub fn blurred_backdrop(bytes: &[u8]) -> Result<Arc<RenderImage>, image::ImageError> {
    let source = image::load_from_memory(bytes)?;
    let small = source.resize_to_fill(WORK_SIZE, WORK_SIZE, imageops::FilterType::Triangle);
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
    Ok(Arc::new(RenderImage::new(vec![Frame::new(out)])))
}
