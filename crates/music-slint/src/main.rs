//! Slint UI spike: search YouTube Music, play a result, show a now-playing bar
//! and a full-window Now Playing screen with synced karaoke lyrics.
//!
//! Threading model: the Slint event loop owns the UI thread. Blocking core calls
//! (search, artwork download + decode + blur, lyrics) run on `std::thread`s and
//! hand their results back with `slint::invoke_from_event_loop`. Playback state
//! is polled with a `slint::Timer`; the `.slint` side interpolates the clock at
//! display refresh rate between polls (see `sync-clock` in `ui/app.slint`).
//!
//! Scripting: `music-slint <query>` or `MUSIC_QUERY=...` searches at launch,
//! `MUSIC_AUTOOPEN=1` plays the first result and opens Now Playing,
//! `MUSIC_DEMO_CLOCK=1` (or no audio device) drives playback from a wall clock.

mod lyrics_layout;
mod now_playing;
mod playback;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use music_core::{
    InnerTube, IosClientResolver, Lyrics, PlaybackState, PlaybackStatus, Player, Track,
    format_duration,
};
use slint::{
    ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode,
    VecModel, Weak,
};

use crate::lyrics_layout::WordMeasurer;
use crate::playback::{ClockSync, Playback};

slint::include_modules!();

const ART_SIZE: u32 = 120;
const ART_WORKERS: usize = 4;
const POLL_INTERVAL: Duration = Duration::from_millis(250);

type Pixels = SharedPixelBuffer<Rgba8Pixel>;

/// UI-thread-only state. `slint::Image` is `!Send`, so it lives in a
/// thread-local that closures run via `invoke_from_event_loop` can reach.
#[derive(Default)]
struct State {
    /// Tracks backing the current results model, by row index.
    tracks: Vec<Track>,
    /// Bumped per search so late responses for older queries are dropped.
    search_generation: u64,
    /// Artwork by video id; `None` while in flight or after a failed fetch.
    art: HashMap<String, Option<Image>>,
    art_jobs: Option<Sender<(String, String)>>,
    /// `MUSIC_AUTOOPEN=1`: play the first result of the first search and open Now Playing.
    autoopen: bool,
    clock: ClockSync,
    /// Lyrics of the current track, once loaded.
    lyrics: Option<Lyrics>,
    measurer: Option<WordMeasurer>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with_borrow_mut(f)
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1")
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    start_art_workers(ui.as_weak());

    let playback = if env_flag("MUSIC_DEMO_CLOCK") {
        Playback::demo()
    } else {
        match Player::new(Arc::new(IosClientResolver)) {
            Ok(player) => Playback::Audio(player),
            Err(e) => {
                eprintln!("audio unavailable ({e}); using the demo clock");
                Playback::demo()
            }
        }
    };
    ui.set_demo_clock(playback.is_demo());
    with_state(|s| s.autoopen = env_flag("MUSIC_AUTOOPEN"));

    ui.on_search({
        let ui = ui.as_weak();
        move |query| search(&ui, query.trim())
    });

    ui.on_play({
        let (ui, playback) = (ui.as_weak(), playback.clone());
        move |index| {
            let Some(track) = with_state(|s| s.tracks.get(index as usize).cloned()) else {
                return;
            };
            let Some(ui) = ui.upgrade() else { return };
            // Show the track right away; the audio engine reports it asynchronously.
            now_playing::show_track(&ui, &track);
            playback.play(track);
            refresh(&ui, &playback);
        }
    });

    ui.on_toggle_pause({
        let (ui, playback) = (ui.as_weak(), playback.clone());
        move || {
            playback.toggle_pause();
            // The audio thread applies the toggle within ~100 ms; poll again shortly.
            let (ui, playback) = (ui.clone(), playback.clone());
            Timer::single_shot(Duration::from_millis(60), move || {
                if let Some(ui) = ui.upgrade() {
                    refresh(&ui, &playback);
                }
            });
        }
    });

    ui.on_seek_to({
        let (ui, playback) = (ui.as_weak(), playback.clone());
        move |secs| {
            let Some(ui) = ui.upgrade() else { return };
            let secs = secs.max(0.0);
            playback.seek(Duration::from_secs_f32(secs));
            with_state(|s| s.clock.seek(secs.into(), !playback.is_demo()));
            ui.invoke_sync_clock(secs);
            now_playing::sync_active_line(&ui, secs);
        }
    });

    ui.on_lyrics_layout_changed({
        let ui = ui.as_weak();
        move || {
            if let Some(ui) = ui.upgrade() {
                now_playing::relayout_lyrics(&ui);
            }
        }
    });

    // `music-slint <query>` or `MUSIC_QUERY=<query>` searches on launch.
    let query = std::env::var("MUSIC_QUERY")
        .ok()
        .filter(|q| !q.trim().is_empty())
        .unwrap_or_else(|| std::env::args().skip(1).collect::<Vec<_>>().join(" "));
    if !query.is_empty() {
        ui.set_query(query.as_str().into());
        search(&ui.as_weak(), &query);
    }

    let poll = Timer::default();
    poll.start(TimerMode::Repeated, POLL_INTERVAL, {
        let ui = ui.as_weak();
        move || {
            if let Some(ui) = ui.upgrade() {
                refresh(&ui, &playback);
            }
        }
    });

    ui.run()
}

fn refresh(ui: &AppWindow, playback: &Playback) {
    show_playback(ui, &playback.state(), playback.is_demo());
}

fn search(ui: &Weak<AppWindow>, query: &str) {
    let Some(window) = ui.upgrade() else { return };
    if query.is_empty() {
        return;
    }
    let generation = with_state(|s| {
        s.search_generation += 1;
        s.search_generation
    });
    window.set_searching(true);
    window.set_search_status(format!("Searching for “{query}”…").into());

    let (ui, query) = (ui.clone(), query.to_owned());
    thread::spawn(move || {
        let result = InnerTube::default().search_songs(&query);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui.upgrade() else { return };
            if with_state(|s| s.search_generation) != generation {
                return;
            }
            ui.set_searching(false);
            match result {
                Ok(tracks) => show_results(&ui, &query, tracks),
                Err(e) => ui.set_search_status(format!("Search failed: {e}").into()),
            }
        });
    });
}

fn show_results(ui: &AppWindow, query: &str, tracks: Vec<Track>) {
    let status = match tracks.len() {
        0 => format!("No songs for “{query}”"),
        1 => format!("1 song for “{query}”"),
        n => format!("{n} songs for “{query}”"),
    };
    ui.set_search_status(status.into());

    let rows: Vec<TrackRow> = tracks
        .iter()
        .map(|t| {
            let subtitle = match &t.album {
                Some(album) => format!("{} · {album}", t.artists),
                None => t.artists.clone(),
            };
            TrackRow {
                video_id: t.video_id.as_str().into(),
                title: t.title.as_str().into(),
                subtitle: subtitle.into(),
                duration: t
                    .duration_secs
                    .map(|s| format_duration(s.into()))
                    .unwrap_or_default()
                    .into(),
                art: artwork(t).unwrap_or_default(),
            }
        })
        .collect();
    let autoopen = with_state(|s| {
        s.tracks = tracks;
        !s.tracks.is_empty() && std::mem::take(&mut s.autoopen)
    });
    // A fresh model (rather than `set_vec`) lets the .slint side reset scroll on change.
    ui.set_results(ModelRc::new(VecModel::from(rows)));
    if autoopen {
        ui.invoke_play(0);
        ui.invoke_open_now_playing();
    }
}

/// Cached artwork for `track`, or `None` after queueing a fetch (once per video id).
fn artwork(track: &Track) -> Option<Image> {
    with_state(|s| {
        if let Some(cached) = s.art.get(&track.video_id) {
            return cached.clone();
        }
        let url = track.artwork_url(ART_SIZE)?;
        if let Some(jobs) = &s.art_jobs {
            s.art.insert(track.video_id.clone(), None);
            let _ = jobs.send((track.video_id.clone(), url));
        }
        None
    })
}

/// Small fixed pool so a page of results doesn't spawn a thread per row.
fn start_art_workers(ui: Weak<AppWindow>) {
    let (tx, rx) = mpsc::channel::<(String, String)>();
    let rx = Arc::new(Mutex::new(rx));
    for i in 0..ART_WORKERS {
        let (rx, ui) = (rx.clone(), ui.clone());
        thread::Builder::new()
            .name(format!("artwork-{i}"))
            .spawn(move || {
                loop {
                    let job = rx.lock().unwrap().recv();
                    let Ok((video_id, url)) = job else { return };
                    let pixels = match load_rgba(&url) {
                        Ok(rgba) => to_pixels(&rgba),
                        Err(e) => {
                            eprintln!("artwork {video_id}: {e}");
                            continue;
                        }
                    };
                    let ui = ui.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui.upgrade() {
                            artwork_loaded(&ui, video_id, pixels);
                        }
                    });
                }
            })
            .expect("spawn artwork worker");
    }
    with_state(|s| s.art_jobs = Some(tx));
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

fn load_rgba(url: &str) -> Result<image::RgbaImage, BoxError> {
    let bytes = music_core::fetch_bytes(url)?;
    Ok(image::load_from_memory(&bytes)?.into_rgba8())
}

fn to_pixels(rgba: &image::RgbaImage) -> Pixels {
    SharedPixelBuffer::clone_from_slice(rgba.as_raw(), rgba.width(), rgba.height())
}

fn artwork_loaded(ui: &AppWindow, video_id: String, pixels: Pixels) {
    let image = Image::from_rgba8(pixels);
    let rows = ui.get_results();
    for i in 0..rows.row_count() {
        if let Some(mut row) = rows.row_data(i)
            && row.video_id == video_id.as_str()
        {
            row.art = image.clone();
            rows.set_row_data(i, row);
        }
    }
    if ui.get_now_video_id() == video_id.as_str() {
        ui.set_now_art(image.clone());
    }
    with_state(|s| s.art.insert(video_id, Some(image)));
}

fn show_playback(ui: &AppWindow, state: &PlaybackState, demo: bool) {
    let Some(track) = &state.track else { return };
    if ui.get_now_video_id() != track.video_id.as_str() {
        now_playing::show_track(ui, track);
    }

    let playing = state.status == PlaybackStatus::Playing;
    ui.set_duration_s(state.duration.unwrap_or_default().as_secs_f32());
    ui.set_playing(playing);
    ui.set_loading(state.status == PlaybackStatus::Loading);
    ui.set_preview(state.preview_only);
    if let Some(pos) = with_state(|s| s.clock.update(state.position, playing)) {
        ui.invoke_sync_clock(pos as f32);
    }

    let (message, is_error) = match &state.status {
        PlaybackStatus::Loading => ("Loading…".to_owned(), false),
        PlaybackStatus::Ended if state.preview_only => ("Preview ended".to_owned(), false),
        PlaybackStatus::Error(e) => (e.clone(), true),
        _ if demo => ("Demo clock — no audio output".to_owned(), false),
        _ => (String::new(), false),
    };
    ui.set_player_message(message.into());
    ui.set_player_error(is_error);
}
