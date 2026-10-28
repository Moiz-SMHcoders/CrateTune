//! Playback side of [`MusicApp`]: starting tracks, queue editing, transport,
//! volume, the 250 ms poll, and OS media-key events.
//!
//! Queue rules live in `queue.rs` (pure, tested); this file only maps them to
//! the engine and keeps the UI in sync. Methods that act on a track list take
//! `Rc<Vec<Track>>` so pages can hand over their list without copying it per row.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{ClickEvent, Context, Entity, Window};
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::slider::{SliderEvent, SliderState};
use music_core::media_controls::MediaControlEvent;
use music_core::{PlaybackState, PlaybackStatus, SleepTimer, Track, format_duration};

use crate::app::{MusicApp, notify, update_app};
use crate::now_playing::HiRes;
use crate::queue::{Ended, Previous, Repeat};
use crate::settings::prefs::SLEEP_PRESETS;
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
        let mode = music_core::together::AddTrackMode::PlayNext;
        if self.together_intercept_enqueue(&tracks, mode, window, cx) {
            return; // a Together guest suggests to the host instead
        }
        let message = enqueue_message(true, &tracks);
        match self.queue.play_next(tracks) {
            Some(track) => self.play(track, cx),
            None => notify(window, cx, message),
        }
        cx.notify();
    }

    pub fn queue_add(&mut self, tracks: Vec<Track>, window: &mut Window, cx: &mut Context<Self>) {
        let mode = music_core::together::AddTrackMode::AddToQueue;
        if self.together_intercept_enqueue(&tracks, mode, window, cx) {
            return;
        }
        let message = enqueue_message(false, &tracks);
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
            self.np.list.manual_until = None;
            self.refresh_soon(cx);
        }
    }

    pub(crate) fn play(&mut self, track: Track, cx: &mut Context<Self>) {
        // Prefetch what the Now Playing screen (and the dynamic theme) needs while
        // audio resolves.
        self.ensure_lyrics(&track, cx);
        self.ensure_hires(&track, cx);
        let track = self.engine_track(track);
        match &mut self.engine {
            Some(engine) => {
                engine.play(track);
                // `play` may consume the engine's next track; send it again.
                self.next_sent = None;
                self.sync_engine_next();
                self.refresh_soon(cx);
            }
            None => self.pending_play = Some(track),
        }
        cx.notify();
    }

    /// Hook for what the engine gets for a queue track (e.g. a downloaded copy's
    /// `local_path`); used for `play` and for the preloaded next track alike.
    pub(crate) fn engine_track(&self, track: Track) -> Track {
        // A downloaded copy plays from disk (`store.rs`).
        self.playable(track)
    }

    /// The track the engine should continue with by itself: the current one
    /// again with repeat-one, else the queue's next (repeat-all wraps).
    fn engine_next(&self) -> Option<Track> {
        match self.queue.repeat() {
            Repeat::One => self.queue.current().cloned(),
            Repeat::Off | Repeat::All => self.queue.peek_next().cloned(),
        }
    }

    /// Keeps the engine's preloaded next track in step with the queue (shuffle,
    /// repeat and edits included). Cheap when nothing changed; runs on every poll
    /// and after `play`.
    pub(crate) fn sync_engine_next(&mut self) {
        let next = self.engine_next();
        let id = next.as_ref().map(|t| t.video_id.clone());
        if self.next_sent.as_ref() == Some(&id) {
            return;
        }
        let next = next.map(|t| self.engine_track(t));
        if let Some(engine) = &mut self.engine {
            engine.set_next(next);
            self.next_sent = Some(id);
        }
    }

    /// The engine moved on by itself (gapless / crossfade switch, or an auto-skip
    /// after a load error): advance the queue to the track it now plays, without
    /// calling `play` again.
    fn engine_advanced(&mut self, state: &PlaybackState) {
        self.next_sent = None;
        let Some(id) = state.track.as_ref().map(|t| t.video_id.as_str()) else {
            return;
        };
        if let Some(started) = self.trace {
            eprintln!(
                "music-gpui: [{:6.2}s] engine advanced to {id} (advance #{}); queue next was {:?}",
                started.elapsed().as_secs_f64(),
                state.advance_count,
                self.queue.peek_next().map(|t| t.video_id.as_str()),
            );
        }
        if self.queue.repeat() == Repeat::One && self.queue.is_current(id) {
            return; // repeat-one looped gaplessly
        }
        if self.queue.peek_next().is_some_and(|t| t.video_id == id) {
            self.queue.advance();
        } else if let Some(uid) = self
            .queue
            .upcoming()
            .iter()
            .find(|i| i.track.video_id == id)
            .map(|i| i.uid)
        {
            // The queue changed after the engine had preloaded: catch up.
            self.queue.jump(uid);
        }
    }

    // ---- sleep timer ---------------------------------------------------------------

    /// Arms the sleep timer (`None` cancels it). Shown in the bar, Now Playing
    /// and the Playback settings.
    pub fn set_sleep_timer(&mut self, timer: Option<SleepTimer>, cx: &mut Context<Self>) {
        if let Some(engine) = &mut self.engine {
            engine.set_sleep_timer(timer);
            self.refresh_soon(cx);
        }
        cx.notify();
    }

    /// Bar / Now Playing menu: presets, end of track, the saved custom length, off.
    pub fn sleep_timer_menu(menu: PopupMenu, armed: bool, custom: Duration) -> PopupMenu {
        let item = |label: String, timer: Option<SleepTimer>| {
            PopupMenuItem::new(label).on_click(move |_, _, cx| {
                update_app(cx, |app, cx| app.set_sleep_timer(timer, cx));
            })
        };
        let mut menu = menu.label(t!("player.sleep.title"));
        for minutes in SLEEP_PRESETS {
            menu = menu.item(item(
                tp!("player.sleep.minutes", minutes),
                Some(SleepTimer::After(Duration::from_secs(
                    u64::from(minutes) * 60,
                ))),
            ));
        }
        menu = menu
            .item(item(
                t!("player.sleep.end_of_track").into(),
                Some(SleepTimer::EndOfTrack),
            ))
            .item(item(
                t!("player.sleep.custom", minutes = custom.as_secs() / 60),
                Some(SleepTimer::After(custom)),
            ));
        if armed {
            menu = menu
                .separator()
                .item(item(t!("player.sleep.turn_off").into(), None));
        }
        menu
    }

    /// "23:41 left" / "End of track" while armed.
    pub fn sleep_timer_label(&self) -> Option<String> {
        let timer = self.playback.sleep_timer?;
        Some(match timer.remaining {
            Some(left) if !timer.end_of_track => {
                t!(
                    "player.sleep.left",
                    time = format_duration(left.as_secs() + 1)
                )
            }
            _ => t!("player.sleep.end_of_track").into(),
        })
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
                if let Some(engine) = &mut this.engine {
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
        let advanced = state.advance_count > self.seen_advance;
        self.seen_advance = state.advance_count;
        if state.error_count > self.playback.error_count
            && let Some(failed) = &state.failed_track_id
        {
            let title = [self.playback.track.as_ref(), self.queue.current()]
                .into_iter()
                .flatten()
                .find(|t| &t.video_id == failed)
                .map_or_else(|| failed.clone(), |t| t.title.clone());
            self.last_failure = Some((title, Instant::now()));
        }
        if advanced {
            self.engine_advanced(&state);
        }
        if advanced
            && track_changed
            && let Some(track) = state.track.clone()
        {
            self.ensure_lyrics(&track, cx);
            self.ensure_hires(&track, cx);
        }
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
        self.poll_spectrum();
        self.sync_engine_next();
        let Some(engine) = &mut self.engine else {
            return;
        };
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
        // SponsorBlock, Music Together.
        self.poll_integrations(&state, window, cx);

        let prev_second = self.playback.position.as_secs();
        let position_only = same_except_position(&self.playback, &state);
        if !self.ingest(state, cx) {
            return;
        }
        // The progress bar moves < 1 px per 250 ms and the time label changes once a
        // second, but every notify re-lays out the whole window (page included). So
        // ticks that only advance the position within the same second wait for the
        // next second — unless Now Playing is up (it paces its own frames) or the
        // user is scrubbing. Anything else that changed this tick shows ≤ 1 s later.
        if position_only
            && !self.np.open
            && self.scrub.is_none()
            && self.playback.position.as_secs() == prev_second
        {
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
}

/// "“Title” will play next" / "3 songs added to the queue".
fn enqueue_message(play_next: bool, tracks: &[Track]) -> String {
    match (tracks, play_next) {
        ([one], true) => t!("queue.toast.play_next_one", title = one.title),
        ([one], false) => t!("queue.toast.added_one", title = one.title),
        (many, true) => tp!("queue.toast.play_next_many", many.len()),
        (many, false) => tp!("queue.toast.added_many", many.len()),
    }
}

/// Field-wise comparison (`PlaybackState` has no `PartialEq`); avoids redundant re-renders.
fn same_snapshot(a: &PlaybackState, b: &PlaybackState) -> bool {
    a.position == b.position && same_except_position(a, b)
}

/// Everything the UI shows from a snapshot, except the position.
fn same_except_position(a: &PlaybackState, b: &PlaybackState) -> bool {
    a.status == b.status
        && a.advance_count == b.advance_count
        && a.error_count == b.error_count
        && a.buffering == b.buffering
        && a.crossfading == b.crossfading
        && a.sleep_timer == b.sleep_timer
        && a.speed == b.speed
        && a.output_device == b.output_device
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
