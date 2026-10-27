//! Slint UI spike: search YouTube Music, play a result, show a now-playing bar.
//!
//! Threading model: the Slint event loop owns the UI thread. Blocking core calls
//! (search, artwork download + decode) run on `std::thread`s and hand their
//! results back with `slint::invoke_from_event_loop`. Playback state is polled
//! with a `slint::Timer`; there is no other cross-thread plumbing.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use music_core::{
    InnerTube, IosClientResolver, PlaybackState, PlaybackStatus, Player, Track, format_duration,
};
use slint::{
    ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, Timer,
    TimerMode, VecModel, Weak,
};

slint::include_modules!();

const ART_SIZE: u32 = 120;
const ART_WORKERS: usize = 4;
const POLL_INTERVAL: Duration = Duration::from_millis(250);

type Pixels = SharedPixelBuffer<Rgba8Pixel>;

/// UI-thread-only state. `slint::Image` is `!Send`, so it lives in a
/// thread-local that closures run via `invoke_from_event_loop` can reach.
struct State {
    /// Tracks backing the current results model, by row index.
    tracks: Vec<Track>,
    /// Bumped per search so late responses for older queries are dropped.
    search_generation: u64,
    /// Artwork by video id; `None` while in flight or after a failed fetch.
    art: HashMap<String, Option<Image>>,
    art_jobs: Option<Sender<(String, String)>>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State {
        tracks: Vec::new(),
        search_generation: 0,
        art: HashMap::new(),
        art_jobs: None,
    });
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with_borrow_mut(f)
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    start_art_workers(ui.as_weak());

    let player = match Player::new(Arc::new(IosClientResolver)) {
        Ok(player) => Some(player),
        Err(e) => {
            eprintln!("audio unavailable: {e}");
            ui.set_player_error(true);
            ui.set_player_message(format!("Audio unavailable: {e}").into());
            None
        }
    };

    ui.on_search({
        let ui = ui.as_weak();
        move |query| search(&ui, query.trim())
    });

    ui.on_play({
        let player = player.clone();
        move |index| {
            let track = with_state(|s| s.tracks.get(index as usize).cloned());
            if let (Some(player), Some(track)) = (&player, track) {
                player.play(track);
            }
        }
    });

    ui.on_toggle_pause({
        let player = player.clone();
        move || {
            if let Some(player) = &player {
                player.toggle_pause();
            }
        }
    });

    ui.on_seek({
        let player = player.clone();
        move |fraction| {
            let Some(player) = &player else { return };
            if let Some(duration) = player.state().duration {
                player.seek(duration.mul_f32(fraction.clamp(0.0, 1.0)));
            }
        }
    });

    // `music-slint <query>` searches on launch (handy for scripted RSS measurements).
    let query = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    if !query.is_empty() {
        ui.set_query(query.as_str().into());
        search(&ui.as_weak(), &query);
    }

    let poll = Timer::default();
    if let Some(player) = player {
        let ui = ui.as_weak();
        poll.start(TimerMode::Repeated, POLL_INTERVAL, move || {
            if let Some(ui) = ui.upgrade() {
                show_playback(&ui, &player.state());
            }
        });
    }

    ui.run()
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
    with_state(|s| s.tracks = tracks);
    // A fresh model (rather than `set_vec`) lets the .slint side reset scroll on change.
    ui.set_results(ModelRc::new(VecModel::from(rows)));
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
                    let pixels = match load_pixels(&url) {
                        Ok(pixels) => pixels,
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

fn load_pixels(url: &str) -> Result<Pixels, Box<dyn std::error::Error>> {
    let bytes = music_core::fetch_bytes(url)?;
    let rgba = image::load_from_memory(&bytes)?.into_rgba8();
    Ok(SharedPixelBuffer::clone_from_slice(
        rgba.as_raw(),
        rgba.width(),
        rgba.height(),
    ))
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

fn show_playback(ui: &AppWindow, state: &PlaybackState) {
    let Some(track) = &state.track else { return };
    if ui.get_now_video_id() != track.video_id.as_str() {
        ui.set_now_video_id(track.video_id.as_str().into());
        ui.set_now_title(track.title.as_str().into());
        ui.set_now_artists(track.artists.as_str().into());
        ui.set_now_art(artwork(track).unwrap_or_default());
    }

    let position = state.position;
    let duration = state.duration.unwrap_or_default();
    let progress = if duration.is_zero() {
        0.0
    } else {
        (position.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
    };
    ui.set_progress(progress);
    ui.set_elapsed(format_duration(position.as_secs()).into());
    ui.set_total(match state.duration {
        Some(d) => format_duration(d.as_secs()).into(),
        None => SharedString::from("–:––"),
    });
    ui.set_playing(state.status == PlaybackStatus::Playing);
    ui.set_loading(state.status == PlaybackStatus::Loading);
    ui.set_preview(state.preview_only);

    let (message, is_error) = match &state.status {
        PlaybackStatus::Loading => ("Loading…".to_owned(), false),
        PlaybackStatus::Ended if state.preview_only => ("Preview ended".to_owned(), false),
        PlaybackStatus::Error(e) => (e.clone(), true),
        _ => (String::new(), false),
    };
    ui.set_player_message(message.into());
    ui.set_player_error(is_error);
}
