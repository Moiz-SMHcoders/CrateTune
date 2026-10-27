//! Now Playing screen: large artwork, blurred background and synced lyrics.

use std::thread;
use std::time::Duration;

use image::imageops::{self, FilterType};
use music_core::Track;
use slint::{ComponentHandle, Image, ModelRc, VecModel};

use crate::lyrics_layout::WordMeasurer;
use crate::{
    AppWindow, BoxError, LyricLineData, Pixels, artwork, load_rgba, to_pixels, with_state,
};

const LARGE_ART: u32 = 544;
/// The background is blurred at this size and upscaled (bilinear) by the GPU.
const BLUR_SIZE: u32 = 128;
const BLUR_SIGMA: f32 = 9.0;
const SATURATION: f32 = 1.35;

/// Switches every per-track part of the UI to `track` and starts its loads.
pub fn show_track(ui: &AppWindow, track: &Track) {
    ui.set_now_video_id(track.video_id.as_str().into());
    ui.set_now_title(track.title.as_str().into());
    ui.set_now_artists(track.artists.as_str().into());
    ui.set_now_album(track.album.clone().unwrap_or_default().into());
    ui.set_now_art(artwork(track).unwrap_or_default());
    ui.set_np_art(Image::default());
    ui.set_duration_s(track.duration_secs.unwrap_or_default() as f32);

    with_state(|s| s.lyrics = None);
    ui.set_lyrics_state(0);
    ui.set_lyric_lines(ModelRc::default());
    ui.set_active_line(-1);

    load_large_art(ui, track);
    load_lyrics(ui, track);
}

fn load_large_art(ui: &AppWindow, track: &Track) {
    let Some(url) = track.artwork_url(LARGE_ART) else {
        return;
    };
    let (ui, video_id) = (ui.as_weak(), track.video_id.clone());
    thread::spawn(move || {
        let result = load_rgba(&url).map(|rgba| (to_pixels(&rgba), to_pixels(&blurred(&rgba))));
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui.upgrade() else { return };
            if ui.get_now_video_id() != video_id.as_str() {
                return;
            }
            match result {
                Ok((sharp, blurred)) => show_large_art(&ui, sharp, blurred),
                Err(e) => eprintln!("large artwork {video_id}: {e}"),
            }
        });
    });
}

/// Heavy blur done once per track on a worker thread: downscale, gaussian blur, boost saturation.
fn blurred(rgba: &image::RgbaImage) -> image::RgbaImage {
    let small = imageops::resize(rgba, BLUR_SIZE, BLUR_SIZE, FilterType::Triangle);
    let mut out = imageops::blur(&small, BLUR_SIGMA);
    for px in out.pixels_mut() {
        let [r, g, b, _] = px.0.map(f32::from);
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        for (c, v) in px.0.iter_mut().zip([r, g, b]) {
            *c = (luma + (v - luma) * SATURATION).clamp(0.0, 255.0) as u8;
        }
    }
    out
}

fn show_large_art(ui: &AppWindow, sharp: Pixels, blurred: Pixels) {
    ui.set_np_art(Image::from_rgba8(sharp));
    // Put the new background on the hidden layer, then flip: the .slint side cross-fades.
    let blurred = Image::from_rgba8(blurred);
    if ui.get_bg_b_front() {
        ui.set_bg_a(blurred);
        ui.set_bg_b_front(false);
    } else {
        ui.set_bg_b(blurred);
        ui.set_bg_b_front(true);
    }
}

fn load_lyrics(ui: &AppWindow, track: &Track) {
    let (ui, track) = (ui.as_weak(), track.clone());
    thread::spawn(move || {
        let result = music_core::fetch_lyrics(&track).map_err(BoxError::from);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui.upgrade() else { return };
            if ui.get_now_video_id() != track.video_id.as_str() {
                return;
            }
            match result {
                Ok(Some(lyrics)) => {
                    ui.set_lyrics_note(
                        if lyrics.word_synced {
                            "Word-synced lyrics · LRCLIB"
                        } else {
                            "Line-synced lyrics · word timing estimated · LRCLIB"
                        }
                        .into(),
                    );
                    with_state(|s| s.lyrics = Some(lyrics));
                    relayout_lyrics(&ui);
                    let now = with_state(|s| s.clock.now());
                    sync_active_line(&ui, now as f32);
                    ui.set_lyrics_state(2);
                }
                Ok(None) => {
                    ui.set_lyrics_message(
                        "LRCLIB has no time-synced lyrics for this track.".into(),
                    );
                    ui.set_lyrics_state(1);
                }
                Err(e) => {
                    ui.set_lyrics_message(e.to_string().into());
                    ui.set_lyrics_state(3);
                }
            }
        });
    });
}

/// Re-flows the lyric words for the current column width and font size.
pub fn relayout_lyrics(ui: &AppWindow) {
    let (width, size) = (ui.get_lyrics_width(), ui.get_lyrics_font_size());
    if width <= 0.0 {
        return;
    }
    let lines: Option<Vec<LyricLineData>> = with_state(|s| {
        let lyrics = s.lyrics.as_ref()?;
        let measurer = s.measurer.get_or_insert_with(WordMeasurer::new);
        Some(measurer.layout(lyrics, width, size))
    });
    if let Some(lines) = lines {
        ui.set_lyric_lines(ModelRc::new(VecModel::from(lines)));
    }
}

/// Points the scroll position at the line under `secs` (after a seek or a load);
/// during playback each line reports its own activation from the `.slint` side.
pub fn sync_active_line(ui: &AppWindow, secs: f32) {
    let index = with_state(|s| {
        let lyrics = s.lyrics.as_ref()?;
        let pos = Duration::from_secs_f32(secs.max(0.0));
        Some(match lyrics.active_line(pos) {
            Some(i) => i as i32,
            // Past the last line: stay on it. Before the first: top.
            None if lyrics.lines.first().is_some_and(|l| pos >= l.start) => {
                lyrics.lines.len() as i32 - 1
            }
            None => -1,
        })
    });
    if let Some(index) = index {
        ui.set_active_line(index);
    }
}
