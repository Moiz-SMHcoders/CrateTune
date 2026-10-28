// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/repository/PlaylistCoverRepository.kt
// (image preparation) — © Rukamori and ArchiveTune contributors, GPL-3.0
//! "Change cover…" for the account's own YouTube Music playlists: pick an image,
//! crop it to the centre square, scale it to at most 1080 px, encode it as JPEG
//! (quality 92 → 60 until it fits in 2 MiB, like ArchiveTune) and upload it with
//! `InnerTube::upload_playlist_cover`. The library mirror and the cached online page
//! pick up the new cover. The upload itself is live-untested (no account here).

use std::io::Cursor;
use std::path::PathBuf;

use gpui::{Context, PathPromptOptions, Window};
use image::imageops::FilterType;
use music_core::innertube::MAX_COVER_BYTES;

use crate::app::{MusicApp, notify};
use crate::util::off_thread;

const UPLOAD_SIDE: u32 = 1080;
const MAX_DECODE_PIXELS: u64 = 40_000_000;

/// Image bytes (JPEG / PNG / WebP) → a square JPEG of at most [`MAX_COVER_BYTES`].
pub fn prepare_cover(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("Couldn't read the image: {e}"))?;
    let (w, h) = reader
        .into_dimensions()
        .map_err(|_| "This file isn't an image the app can read (JPEG, PNG or WebP).".to_owned())?;
    if u64::from(w) * u64::from(h) > MAX_DECODE_PIXELS {
        return Err("The image is too large (more than 40 megapixels).".into());
    }
    let img =
        image::load_from_memory(bytes).map_err(|e| format!("Couldn't decode the image: {e}"))?;
    let side = w.min(h);
    let square = img.crop_imm((w - side) / 2, (h - side) / 2, side, side);
    let square = if side > UPLOAD_SIDE {
        square.resize_exact(UPLOAD_SIDE, UPLOAD_SIDE, FilterType::Lanczos3)
    } else {
        square
    };
    let rgb = square.to_rgb8();
    let mut quality = 92u8;
    loop {
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
            .encode_image(&rgb)
            .map_err(|e| format!("Couldn't encode the cover: {e}"))?;
        if out.len() <= MAX_COVER_BYTES {
            return Ok(out);
        }
        if quality <= 60 {
            return Err("The cover is still larger than 2 MiB after compression.".into());
        }
        quality -= 8;
    }
}

impl MusicApp {
    /// Picks an image and makes it the cover of YouTube playlist `playlist_id`
    /// (`local_id`: its library mirror, updated too).
    pub fn change_playlist_cover(
        &mut self,
        playlist_id: String,
        local_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.store.account.is_signed_in() {
            notify(
                window,
                cx,
                "Sign in to YouTube Music to change playlist covers.",
            );
            return;
        }
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Use as cover".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = picked.await else {
                return;
            };
            let Some(path) = paths.pop() else { return };
            this.update_in(cx, |this, window, cx| {
                this.upload_playlist_cover(playlist_id, local_id, path, window, cx)
            })
            .ok();
        })
        .detach();
    }

    /// The upload part of [`Self::change_playlist_cover`] (also `@playlist-cover`).
    pub fn upload_playlist_cover(
        &mut self,
        playlist_id: String,
        local_id: Option<String>,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let client = self.account_client();
        let library = self.store.library.clone();
        notify(window, cx, "Uploading the cover…");
        cx.spawn_in(window, async move |this, cx| {
            let id = playlist_id.clone();
            let result = off_thread(move || {
                let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                let jpeg = prepare_cover(&bytes)?;
                let url = client
                    .upload_playlist_cover(&id, &jpeg)
                    .map_err(|e| e.to_string())?;
                if let (Some(lib), Some(local), Some(url)) = (library, local_id, url.as_deref()) {
                    let _ = lib.set_playlist_thumbnail(&local, Some(url));
                }
                Ok::<_, String>(url)
            })
            .await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(_) => {
                    let id = playlist_id.strip_prefix("VL").unwrap_or(&playlist_id);
                    crate::browse::PageCache::remove(&format!("playlist:{id}"), cx);
                    this.library_changed(cx);
                    this.refresh_pages(window, cx);
                    notify(window, cx, "Playlist cover changed");
                }
                Err(e) => notify(window, cx, format!("Couldn't change the cover: {e}")),
            })
            .ok();
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x ^ y) % 256) as u8])
        });
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn covers_are_square_jpegs_within_limits() {
        let jpeg = prepare_cover(&png(1600, 900)).unwrap();
        assert!(jpeg.len() <= MAX_COVER_BYTES);
        let img = image::load_from_memory(&jpeg).unwrap();
        assert_eq!((img.width(), img.height()), (900, 900));
        assert_eq!(
            image::guess_format(&jpeg).unwrap(),
            image::ImageFormat::Jpeg
        );
        let big = image::load_from_memory(&prepare_cover(&png(1500, 1200)).unwrap()).unwrap();
        assert_eq!((big.width(), big.height()), (1080, 1080));
        assert!(prepare_cover(b"not an image").is_err());
    }
}
