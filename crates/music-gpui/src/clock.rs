//! Playback sources and the smooth position clock that drives lyrics.

use std::time::{Duration, Instant};

use music_core::{
    EqualizerSettings, PlaybackState, PlaybackStatus, Player, SilenceConfig, SleepTimer,
    SleepTimerState, Track,
};

use crate::settings::prefs::AudioConfig;

/// Where the demo clock starts (the first lyric of most songs is a bit later).
const DEMO_START: Duration = Duration::from_secs(20);
/// The demo track "ends" here (or at the track end, if shorter), like the ~60 s
/// previews we can actually stream, so auto-advance can be exercised without audio.
const DEMO_END: Duration = Duration::from_secs(80);
/// A polled position further than this from the extrapolated one re-anchors the clock.
const RESYNC_THRESHOLD: Duration = Duration::from_millis(400);

/// The real audio engine, or a wall clock when there is no audio device
/// (`Player::new` failed) or `MUSIC_DEMO_CLOCK=1`.
pub enum Engine {
    Audio(Player),
    Demo(Box<DemoPlayer>),
    /// Casting to a Chromecast (the local engine waits inside, paused).
    Cast(Box<crate::integrations::cast::CastEngine>),
}

impl Engine {
    pub fn play(&mut self, track: Track) {
        match self {
            Engine::Audio(player) => player.play(track),
            Engine::Demo(demo) => demo.play(track),
            Engine::Cast(cast) => cast.play(track),
        }
    }

    pub fn toggle_pause(&mut self) {
        match self {
            Engine::Audio(player) => player.toggle_pause(),
            Engine::Demo(demo) => demo.toggle_pause(),
            Engine::Cast(cast) => cast.toggle_pause(),
        }
    }

    pub fn seek(&mut self, position: Duration) {
        match self {
            Engine::Audio(player) => player.seek(position),
            Engine::Demo(demo) => demo.seek(position),
            Engine::Cast(cast) => cast.seek(position),
        }
    }

    /// Back to 0 s; replays the track if it already ended.
    pub fn restart(&mut self) {
        let ended = self.state().status == PlaybackStatus::Ended;
        match self {
            Engine::Audio(player) if ended => player.resume(),
            Engine::Audio(player) => player.seek(Duration::ZERO),
            Engine::Demo(demo) => demo.seek(Duration::ZERO),
            Engine::Cast(cast) => cast.restart(),
        }
    }

    pub fn set_volume(&mut self, volume: f32) {
        match self {
            Engine::Audio(player) => player.set_volume(volume),
            Engine::Demo(demo) => demo.volume = volume.clamp(0., 1.),
            Engine::Cast(cast) => cast.set_volume(volume.clamp(0., 1.)),
        }
    }

    /// `None` turns the equalizer off. The demo clock has no audio to equalize.
    pub fn set_equalizer(&mut self, settings: Option<EqualizerSettings>) {
        if let Engine::Cast(cast) = self {
            cast.local_mut().set_equalizer(settings);
        } else if let Engine::Audio(player) = self {
            match settings {
                Some(settings) => player.set_equalizer(settings),
                None => player.clear_equalizer(),
            }
        }
    }

    /// The track the engine continues with when the current one ends (gapless /
    /// crossfade); the switch shows up as a new `PlaybackState::advance_count`.
    pub fn set_next(&mut self, track: Option<Track>) {
        match self {
            Engine::Audio(player) => player.set_next(track),
            Engine::Demo(demo) => demo.next = track,
            Engine::Cast(cast) => cast.set_next(track),
        }
    }

    /// Quality, crossfade, skip silence, speed, pitch, auto-skip, normalization
    /// and the output device. The demo clock only honours the speed.
    pub fn apply_audio(&mut self, audio: &AudioConfig) {
        match self {
            Engine::Audio(player) => {
                player.set_quality(audio.quality);
                player.set_crossfade(audio.crossfade());
                player.set_skip_silence(audio.skip_silence.then(SilenceConfig::default));
                player.set_speed(audio.speed);
                player.set_pitch(audio.pitch);
                player.set_auto_skip(audio.auto_skip);
                player.set_normalization(audio.normalize);
                player.set_output_device(audio.output_device.clone());
            }
            Engine::Demo(demo) => {
                let now = Instant::now();
                demo.rebase(now);
                demo.speed = audio.speed;
            }
            Engine::Cast(cast) => cast.local_mut().apply_audio(audio),
        }
    }

    /// Arms (`Some`) or cancels the sleep timer.
    pub fn set_sleep_timer(&mut self, timer: Option<SleepTimer>) {
        match self {
            Engine::Audio(player) => player.set_sleep_timer(timer),
            Engine::Demo(demo) => demo.set_sleep_timer(timer, Instant::now()),
            // Not while casting (the local engine keeps it for later).
            Engine::Cast(cast) => cast.local_mut().set_sleep_timer(timer),
        }
    }

    pub fn state(&mut self) -> PlaybackState {
        match self {
            Engine::Audio(player) => player.state(),
            Engine::Demo(demo) => {
                let now = Instant::now();
                demo.tick(now);
                demo.state(now)
            }
            Engine::Cast(cast) => cast.state(),
        }
    }

    pub fn is_demo(&self) -> bool {
        matches!(self, Engine::Demo(_))
    }
}

/// Fake player: "plays" instantly from 20 s and ends at 80 s like a preview, so
/// lyrics and the queue can be verified without audio. Like the engine it
/// switches to the track given to `set_next` by itself (bumping
/// `advance_count`), honours the speed and the sleep timer.
pub struct DemoPlayer {
    track: Option<Track>,
    base: Duration,
    /// `Some` while playing: the instant `base` was valid.
    since: Option<Instant>,
    volume: f32,
    next: Option<Track>,
    advance_count: u64,
    speed: f32,
    sleep_deadline: Option<Instant>,
    sleep_end_of_track: bool,
}

impl Default for DemoPlayer {
    fn default() -> Self {
        Self {
            track: None,
            base: Duration::ZERO,
            since: None,
            volume: 1.0,
            next: None,
            advance_count: 0,
            speed: 1.0,
            sleep_deadline: None,
            sleep_end_of_track: false,
        }
    }
}

impl DemoPlayer {
    /// Folds the elapsed time into `base` (before changing the speed).
    fn rebase(&mut self, now: Instant) {
        if self.since.is_some() {
            self.base = self.position(now);
            self.since = Some(now);
        }
    }

    fn set_sleep_timer(&mut self, timer: Option<SleepTimer>, now: Instant) {
        self.sleep_deadline = None;
        self.sleep_end_of_track = false;
        match timer {
            Some(SleepTimer::After(d)) => self.sleep_deadline = Some(now + d),
            Some(SleepTimer::EndOfTrack) => self.sleep_end_of_track = true,
            None => {}
        }
    }

    /// Sleep timer expiry and the gapless switch to `next`.
    fn tick(&mut self, now: Instant) {
        if self.sleep_deadline.is_some_and(|d| now >= d) {
            self.sleep_deadline = None;
            if self.since.is_some() {
                self.toggle_pause();
            }
        }
        if self.track.is_some() && self.since.is_some() && self.ended(now) {
            if self.sleep_end_of_track {
                self.sleep_end_of_track = false;
                self.base = self.end();
                self.since = None;
                if let Some(next) = self.next.take() {
                    // Like the engine: move on, but wait paused at the start.
                    self.track = Some(next);
                    self.base = Duration::ZERO;
                    self.advance_count += 1;
                }
            } else if let Some(next) = self.next.take() {
                self.track = Some(next);
                self.base = DEMO_START;
                self.since = Some(now);
                self.advance_count += 1;
            }
        }
    }

    fn play(&mut self, track: Track) {
        if self
            .next
            .as_ref()
            .is_some_and(|n| n.video_id == track.video_id)
        {
            self.next = None;
        }
        self.track = Some(track);
        self.base = DEMO_START;
        self.since = Some(Instant::now());
    }

    fn toggle_pause(&mut self) {
        let now = Instant::now();
        if self.ended(now) {
            // Like `Player`: resuming an ended track replays it.
            self.base = Duration::ZERO;
            self.since = Some(now);
        } else if self.since.is_some() {
            self.base = self.position(now);
            self.since = None;
        } else if self.track.is_some() {
            self.since = Some(now);
        }
    }

    fn ended(&self, now: Instant) -> bool {
        self.track.is_some() && self.position(now) >= self.end()
    }

    fn seek(&mut self, position: Duration) {
        self.base = position;
        if self.since.is_some() {
            self.since = Some(Instant::now());
        }
    }

    fn end(&self) -> Duration {
        let total = self
            .track
            .as_ref()
            .and_then(|t| t.duration_secs)
            .map_or(DEMO_END, |s| Duration::from_secs(s.into()));
        total.min(DEMO_END).max(DEMO_START + Duration::from_secs(5))
    }

    fn position(&self, now: Instant) -> Duration {
        let elapsed = self.since.map_or(Duration::ZERO, |s| now - s);
        let pos = self.base + elapsed.mul_f32(self.speed.max(0.1));
        pos.min(self.end())
    }

    fn state(&self, now: Instant) -> PlaybackState {
        PlaybackState {
            track: self.track.clone(),
            status: match (&self.track, self.since) {
                (None, _) => PlaybackStatus::Idle,
                (Some(_), _) if self.ended(now) => PlaybackStatus::Ended,
                (Some(_), Some(_)) => PlaybackStatus::Playing,
                (Some(_), None) => PlaybackStatus::Paused,
            },
            position: self.position(now),
            duration: self
                .track
                .as_ref()
                .and_then(|t| t.duration_secs)
                .map(|s| Duration::from_secs(s.into())),
            volume: self.volume,
            preview_only: true,
            advance_count: self.advance_count,
            next_track: self.next.clone(),
            speed: self.speed,
            sleep_timer: (self.sleep_deadline.is_some() || self.sleep_end_of_track).then(|| {
                SleepTimerState {
                    remaining: self
                        .sleep_deadline
                        .map(|d| d.saturating_duration_since(now)),
                    end_of_track: self.sleep_end_of_track,
                }
            }),
            ..PlaybackState::default()
        }
    }
}

/// Extrapolates the 250 ms `Player::state()` samples with a monotonic clock so
/// lyrics can move at display refresh rate.
#[derive(Default)]
pub struct SmoothClock {
    anchor_pos: Duration,
    anchor_at: Option<Instant>,
    running: bool,
    video_id: Option<String>,
    /// Playback speed (track time per wall-clock time); 0 = unknown → 1.
    speed: f32,
}

impl SmoothClock {
    pub fn position(&self, now: Instant) -> Duration {
        match (self.running, self.anchor_at) {
            (true, Some(at)) => {
                let speed = if self.speed > 0.0 { self.speed } else { 1.0 };
                self.anchor_pos + now.saturating_duration_since(at).mul_f32(speed)
            }
            _ => self.anchor_pos,
        }
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Feeds a polled snapshot. Small drift is ignored (the engine only updates its
    /// position every 100 ms, so samples jitter); jumps (seeks, loops, track changes,
    /// pause/resume) re-anchor.
    pub fn sync(&mut self, state: &PlaybackState, now: Instant) {
        let running = state.status == PlaybackStatus::Playing;
        let video_id = state.track.as_ref().map(|t| t.video_id.clone());
        let predicted = self.position(now);
        let drift = predicted.abs_diff(state.position);
        let speed_changed = (state.speed - self.speed).abs() > f32::EPSILON;
        if running != self.running
            || video_id != self.video_id
            || drift > RESYNC_THRESHOLD
            || speed_changed
        {
            self.anchor_pos = state.position;
            self.anchor_at = Some(now);
        }
        self.speed = state.speed;
        self.running = running;
        self.video_id = video_id;
    }

    /// Optimistic jump after a user seek (the next poll confirms or corrects it).
    pub fn seek(&mut self, position: Duration, now: Instant) {
        self.anchor_pos = position;
        self.anchor_at = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str) -> Track {
        Track {
            video_id: id.into(),
            title: id.into(),
            duration_secs: Some(200),
            ..Track::default()
        }
    }

    #[test]
    fn demo_switches_to_next_and_counts_the_advance() {
        let t0 = Instant::now();
        let mut demo = DemoPlayer::default();
        demo.play(track("a"));
        demo.since = Some(t0);
        demo.next = Some(track("b"));
        let later = t0 + (DEMO_END - DEMO_START) + Duration::from_millis(10);
        demo.tick(later);
        let state = demo.state(later);
        assert_eq!(state.track.unwrap().video_id, "b");
        assert_eq!(state.advance_count, 1);
        assert_eq!(state.status, PlaybackStatus::Playing);
        assert!(state.next_track.is_none());
    }

    #[test]
    fn demo_speed_and_sleep_timer() {
        let t0 = Instant::now();
        let mut demo = DemoPlayer::default();
        demo.play(track("a"));
        demo.since = Some(t0);
        demo.speed = 2.0;
        let pos = demo.state(t0 + Duration::from_secs(5)).position;
        assert_eq!(pos, DEMO_START + Duration::from_secs(10));

        demo.set_sleep_timer(Some(SleepTimer::After(Duration::from_secs(3))), t0);
        let s = demo.state(t0 + Duration::from_secs(1)).sleep_timer.unwrap();
        assert_eq!(s.remaining, Some(Duration::from_secs(2)));
        demo.tick(t0 + Duration::from_secs(4));
        let state = demo.state(t0 + Duration::from_secs(4));
        assert_eq!(state.status, PlaybackStatus::Paused);
        assert!(state.sleep_timer.is_none());

        // End of track: moves on to the next one, paused at its start.
        demo.set_sleep_timer(Some(SleepTimer::EndOfTrack), t0);
        demo.next = Some(track("b"));
        demo.toggle_pause();
        demo.tick(Instant::now() + Duration::from_secs(60));
        let state = demo.state(Instant::now() + Duration::from_secs(60));
        assert_eq!(state.track.unwrap().video_id, "b");
        assert_eq!(state.status, PlaybackStatus::Paused);
        assert_eq!(state.position, Duration::ZERO);
    }

    #[test]
    fn smooth_clock_follows_the_speed() {
        let t0 = Instant::now();
        let mut clock = SmoothClock::default();
        let state = PlaybackState {
            track: Some(track("a")),
            status: PlaybackStatus::Playing,
            position: Duration::from_secs(10),
            speed: 1.5,
            ..PlaybackState::default()
        };
        clock.sync(&state, t0);
        assert_eq!(
            clock.position(t0 + Duration::from_secs(2)),
            Duration::from_secs(13)
        );
    }
}
