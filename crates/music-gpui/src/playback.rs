//! Playback side of [`MusicApp`]: starting tracks, queue editing, transport,
//! volume, the 250 ms poll, and OS media-key events.
//!
//! Queue rules live in `queue.rs` (pure, tested); this file only maps them to
//! the engine and keeps the UI in sync. Methods that act on a track list take
//! `Rc<Vec<Track>>` so pages can hand over their list without copying it per row.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{ClickEvent, Context, Entity, Window};
use gpui_component::slider::{SliderEvent, SliderState};
use music_core::media_controls::MediaControlEvent;
use music_core::{PlaybackState, PlaybackStatus, Track};

use crate::app::{MusicApp, notify};
use crate::now_playing::HiRes;
use crate::queue::{Ended, Previous, Repeat};
use crate::tray::TrayCommand;

/// Cmd+↑ / Cmd+↓ volume step.
pub const VOLUME_STEP: f32 = 0.05;

impl MusicApp {
    // ---- starting playback ----------------------------------------------------

    /// Plays `list[index]` with the whole list as the queue.
    pub fn play_list(&mut self, list: Rc<Vec<Track>>, index: usize, cx: &mut Context<Self>) {
        let tracks = Rc::try_unwrap(list).unwrap_or_else(|rc| (*rc).clone());
        if let Some(track) = self.queue.replace(tracks, index) {
            self.play(track, cx);
        }
    }

    /// Plays `track` right away, keeping the rest of the queue after it.
    pub fn play_now(&mut self, track: Track, cx: &mut Context<Self>) {
        let track = self.queue.play_now(track);
        self.play(track, cx);
    }

    pub fn queue_play_next(
        &mut self,
        tracks: Vec<Track>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let message = enqueue_message("will play next", &tracks);
        match self.queue.play_next(tracks) {
            Some(track) => self.play(track, cx),
            None => notify(window, cx, message),
        }
        cx.notify();
    }

    pub fn queue_add(&mut self, tracks: Vec<Track>, window: &mut Window, cx: &mut Context<Self>) {
        let message = enqueue_message("added to the queue", &tracks);
        match self.queue.add(tracks) {
            Some(track) => self.play(track, cx),
            None => notify(window, cx, message),
        }
        cx.notify();
    }

    pub fn queue_jump(&mut self, uid: u64, cx: &mut Context<Self>) {
        let already = self.queue.current_item().map(|i| i.uid) == Some(uid);
        if let Some(track) = self.queue.jump(uid) {
            if already && self.current_video_id() == Some(track.video_id.as_str()) {
                // Already playing this item: restart instead of reloading.
                self.restart(cx);
            } else {
                self.play(track, cx);
            }
        }
    }

    pub fn queue_remove(&mut self, uid: u64, cx: &mut Context<Self>) {
        if self.queue.remove(uid) {
            cx.notify();
        }
    }

    pub fn queue_nudge(&mut self, uid: u64, delta: isize, cx: &mut Context<Self>) {
        if self.queue.nudge(uid, delta) {
            cx.notify();
        }
    }

    pub fn queue_move_to(&mut self, uid: u64, target: u64, cx: &mut Context<Self>) {
        if self.queue.move_to(uid, target) {
            cx.notify();
        }
    }

    pub fn queue_clear(&mut self, cx: &mut Context<Self>) {
        self.queue.clear();
        cx.notify();
    }

    pub fn set_shuffle(&mut self, on: bool, cx: &mut Context<Self>) {
        self.queue.set_shuffle(on);
        self.save_modes(cx);
    }

    pub fn toggle_shuffle(&mut self, cx: &mut Context<Self>) {
        self.set_shuffle(!self.queue.shuffle(), cx);
    }

    pub fn set_repeat(&mut self, repeat: Repeat, cx: &mut Context<Self>) {
        self.queue.set_repeat(repeat);
        self.save_modes(cx);
    }

    pub fn cycle_repeat(&mut self, cx: &mut Context<Self>) {
        self.set_repeat(self.queue.repeat().next(), cx);
    }

    fn save_modes(&mut self, cx: &mut Context<Self>) {
        let (shuffle, repeat) = (self.queue.shuffle(), self.queue.repeat());
        self.settings.update(cx, |panel, cx| {
            panel.set_playback_modes(shuffle, repeat, cx)
        });
        cx.notify();
    }

    // ---- hooks for features that live in other layers (stubs for now) ----------

    /// Radio from a track: needs InnerTube's "watch next" radio playlist.
    pub fn start_radio(&mut self, track: Track, window: &mut Window, cx: &mut Context<Self>) {
        notify(
            window,
            cx,
            format!(
                "Radio for “{}” arrives with the InnerTube radio API.",
                track.title
            ),
        );
    }

    // ---- transport -------------------------------------------------------------

    pub fn next_track(&mut self, cx: &mut Context<Self>) {
        if let Some(track) = self.queue.advance() {
            self.play(track, cx);
        }
    }

    /// Restarts the track past 3 s, else plays the previous one.
    pub fn previous_track(&mut self, cx: &mut Context<Self>) {
        if self.playback.track.is_none() {
            return;
        }
        match self.queue.previous(self.clock.position(Instant::now())) {
            Previous::Play(track) => self.play(track, cx),
            Previous::Restart => self.restart(cx),
        }
    }

    fn restart(&mut self, cx: &mut Context<Self>) {
        if let Some(engine) = &mut self.engine {
            engine.restart();
            self.clock.seek(Duration::ZERO, Instant::now());
            self.np.manual_until = None;
            self.refresh_soon(cx);
        }
    }

    pub(crate) fn play(&mut self, track: Track, cx: &mut Context<Self>) {
        // Prefetch what the Now Playing screen (and the dynamic theme) needs while
        // audio resolves.
        self.ensure_lyrics(&track, cx);
        self.ensure_hires(&track, cx);
        if self.engine.is_some() {
            // A downloaded copy plays from disk (`store.rs`).
            let track = self.playable(track);
            if let Some(engine) = &mut self.engine {
                engine.play(track);
            }
            self.refresh_soon(cx);
        } else {
            self.pending_play = Some(track);
        }
        cx.notify();
    }

    pub fn toggle_pause(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_playback(cx);
    }

    pub fn toggle_playback(&mut self, cx: &mut Context<Self>) {
        // Last session's queue: start it where it was left.
        if self.start_restored(cx) {
            return;
        }
        if let Some(engine) = &mut self.engine {
            engine.toggle_pause();
            self.refresh_soon(cx);
        }
    }

    /// Re-reads the player state shortly after a command instead of waiting for the next tick.
    pub(crate) fn refresh_soon(&mut self, cx: &mut Context<Self>) {
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            executor.timer(Duration::from_millis(30)).await;
            this.update(cx, |this, cx| {
                if let Some(engine) = &this.engine {
                    let state = engine.state();
                    if this.ingest(state, cx) {
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Takes a new snapshot; returns whether it differs from the last one.
    /// Auto-advances when the queue's current track just ended (a preview ending
    /// early is an ordinary `Ended` too); repeat-one restarts it instead.
    pub(crate) fn ingest(&mut self, state: PlaybackState, cx: &mut Context<Self>) -> bool {
        self.clock.sync(&state, Instant::now());
        if same_snapshot(&self.playback, &state) {
            return false;
        }
        if let Some(started) = self.trace {
            trace(started, &self.playback, &state);
        }
        let just_ended =
            state.status == PlaybackStatus::Ended && self.playback.status != PlaybackStatus::Ended;
        let track_changed = state.track.as_ref().map(|t| &t.video_id)
            != self.playback.track.as_ref().map(|t| &t.video_id);
        self.playback = state;
        if track_changed {
            self.update_accent(cx);
        }
        if just_ended
            && let Some(track) = &self.playback.track
            && self.queue.is_current(&track.video_id)
        {
            match self.queue.track_ended() {
                Ended::Play(track) => self.play(track, cx),
                Ended::Restart => self.restart(cx),
                Ended::Stop => {}
            }
        }
        true
    }

    pub(crate) fn poll_player(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Shortcuts need focus inside the app (e.g. after a focused page went away).
        if window.focused(cx).is_none() {
            window.focus(&self.focus, cx);
        }
        self.poll_tray(window, cx);
        let Some(engine) = &self.engine else { return };
        let state = engine.state();

        let artwork = state
            .track
            .as_ref()
            .and_then(|t| match self.hires.get(&t.video_id) {
                Some(HiRes::Ready { art, .. }) => Some(art.bytes()),
                _ => None,
            });
        let report = self.services.update(&state, artwork);
        if !report.scrobble_errors.is_empty() || report.discord_status.is_some() {
            self.settings.update(cx, |panel, cx| {
                if let Some(status) = report.discord_status {
                    panel.set_discord_status(status, cx);
                }
                panel.push_scrobble_errors(report.scrobble_errors, cx);
            });
        }
        for event in self.services.take_events() {
            self.on_media_event(event, window, cx);
        }
        // Play history, saved queue, download flags.
        self.poll_store(&state, cx);

        if !self.ingest(state, cx) {
            return;
        }
        let state = &self.playback;
        let max = state.duration.map_or(1., |d| d.as_secs_f32().max(1.));
        let pos = state.position.as_secs_f32().min(max);
        if self.scrub.is_none() {
            self.progress.update(cx, |slider, cx| {
                if (slider.max_value() - max).abs() > f32::EPSILON {
                    *slider = SliderState::new().min(0.).max(max).step(0.25);
                }
                slider.set_value(pos, window, cx);
            });
        }
        if self.autoopen && !self.autoplay && self.playback.track.is_some() {
            self.autoopen = false;
            self.open_now_playing(window, cx);
        }
        cx.notify();
    }

    /// Menu-bar clicks, and keeping its "now playing" line current.
    fn poll_tray(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tray) = &mut self.tray else { return };
        let title = self
            .playback
            .track
            .as_ref()
            .map(|t| format!("{} — {}", t.title, t.artists));
        tray.update(
            title.as_deref(),
            self.playback.status == PlaybackStatus::Playing,
        );
        for command in tray.take_commands() {
            match command {
                TrayCommand::TogglePlayback => self.toggle_playback(cx),
                TrayCommand::Next => self.next_track(cx),
                TrayCommand::Previous => self.previous_track(cx),
                TrayCommand::ToggleMiniPlayer => self.toggle_mini_player(cx),
                TrayCommand::ShowWindow => {
                    cx.activate(true);
                    window.activate_window();
                }
                TrayCommand::Quit => cx.quit(),
            }
        }
    }

    pub(crate) fn on_progress_event(
        &mut self,
        _: &Entity<SliderState>,
        event: &SliderEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SliderEvent::Change(value) => {
                self.scrub = Some(value.start());
                cx.notify();
            }
            SliderEvent::Release(value) => {
                self.scrub = None;
                self.seek_to(Duration::from_secs_f32(value.start().max(0.)), cx);
            }
        }
    }

    /// OS media keys / Control Center. With real audio only next/previous get
    /// here (`MediaControls::for_player` routes the rest to the `Player`).
    fn on_media_event(
        &mut self,
        event: MediaControlEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let playing = self.playback.status == PlaybackStatus::Playing;
        match event {
            MediaControlEvent::Next => self.next_track(cx),
            MediaControlEvent::Previous => self.previous_track(cx),
            MediaControlEvent::Toggle => self.toggle_playback(cx),
            MediaControlEvent::Play if !playing => self.toggle_playback(cx),
            MediaControlEvent::Pause | MediaControlEvent::Stop if playing => {
                self.toggle_playback(cx)
            }
            MediaControlEvent::Play | MediaControlEvent::Pause | MediaControlEvent::Stop => {}
            MediaControlEvent::SeekTo(position) => self.seek_to(position, cx),
            MediaControlEvent::SeekBy(secs) => {
                let now = self.clock.position(Instant::now()).as_secs_f64();
                self.seek_to(Duration::from_secs_f64((now + secs).max(0.)), cx);
            }
            MediaControlEvent::SetVolume(volume) => self.set_volume(volume as f32, window, cx),
        }
    }

    // ---- volume ----------------------------------------------------------------------

    pub(crate) fn on_volume_event(
        &mut self,
        _: &Entity<SliderState>,
        event: &SliderEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
        self.apply_volume(value.start(), cx);
    }

    /// Sets the volume (live + saved) and moves the slider.
    pub fn set_volume(&mut self, volume: f32, window: &mut Window, cx: &mut Context<Self>) {
        let volume = volume.clamp(0., 1.);
        self.volume_slider
            .update(cx, |slider, cx| slider.set_value(volume, window, cx));
        self.apply_volume(volume, cx);
    }

    pub fn step_volume(&mut self, delta: f32, window: &mut Window, cx: &mut Context<Self>) {
        // Snap to the step grid so repeated presses land on round values.
        let target = ((self.volume + delta) / VOLUME_STEP).round() * VOLUME_STEP;
        self.set_volume(target, window, cx);
    }

    fn apply_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        self.volume = volume;
        if volume > 0. {
            self.unmuted_volume = volume;
        }
        if let Some(engine) = &mut self.engine {
            engine.set_volume(volume);
        }
        self.settings
            .update(cx, |panel, cx| panel.set_volume(volume, cx));
        cx.notify();
    }

    pub fn toggle_mute(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let target = if self.volume > 0. {
            0.
        } else {
            self.unmuted_volume
        };
        self.set_volume(target, window, cx);
    }
}

fn enqueue_message(what: &str, tracks: &[Track]) -> String {
    match tracks {
        [one] => format!("“{}” {what}", one.title),
        many => format!("{} songs {what}", many.len()),
    }
}

/// Field-wise comparison (`PlaybackState` has no `PartialEq`); avoids redundant re-renders.
fn same_snapshot(a: &PlaybackState, b: &PlaybackState) -> bool {
    a.status == b.status
        && a.position == b.position
        && a.duration == b.duration
        && a.preview_only == b.preview_only
        && a.volume == b.volume
        && a.track.as_ref().map(|t| &t.video_id) == b.track.as_ref().map(|t| &t.video_id)
}

/// `MUSIC_TRACE=1`: one line per status/track change, seek, and every 5 s of position.
fn trace(started: Instant, old: &PlaybackState, new: &PlaybackState) {
    let id = |s: &PlaybackState| s.track.as_ref().map(|t| t.video_id.clone());
    let jumped = new.position.abs_diff(old.position) > Duration::from_millis(1500);
    let tick = new.position.as_secs() / 5 != old.position.as_secs() / 5;
    if old.status != new.status || id(old) != id(new) || jumped || tick {
        eprintln!(
            "music-gpui: [{:6.2}s] {:?} {} pos {:.1}s / {} vol {:.2}{}",
            started.elapsed().as_secs_f64(),
            new.status,
            id(new).unwrap_or_default(),
            new.position.as_secs_f64(),
            new.duration
                .map_or("?".into(), |d| format!("{:.1}s", d.as_secs_f64())),
            new.volume,
            if new.preview_only { " preview" } else { "" },
        );
    }
}
