//! Playback sources and the smooth position clock that drives lyrics.

use std::time::{Duration, Instant};

use music_core::{PlaybackState, PlaybackStatus, Player, Track};

/// Where the demo clock starts (the first lyric of most songs is a bit later).
const DEMO_START: Duration = Duration::from_secs(20);
/// The demo clock loops back to `DEMO_START` here (or at the track end, if shorter),
/// mimicking the ~60 s previews we can actually stream.
const DEMO_LOOP_END: Duration = Duration::from_secs(80);
/// A polled position further than this from the extrapolated one re-anchors the clock.
const RESYNC_THRESHOLD: Duration = Duration::from_millis(400);

/// The real audio engine, or a wall clock when there is no audio device
/// (`Player::new` failed) or `MUSIC_DEMO_CLOCK=1`.
pub enum Engine {
    Audio(Player),
    Demo(DemoPlayer),
}

impl Engine {
    pub fn play(&mut self, track: Track) {
        match self {
            Engine::Audio(player) => player.play(track),
            Engine::Demo(demo) => demo.play(track),
        }
    }

    pub fn toggle_pause(&mut self) {
        match self {
            Engine::Audio(player) => player.toggle_pause(),
            Engine::Demo(demo) => demo.toggle_pause(),
        }
    }

    pub fn seek(&mut self, position: Duration) {
        match self {
            Engine::Audio(player) => player.seek(position),
            Engine::Demo(demo) => demo.seek(position),
        }
    }

    pub fn state(&self) -> PlaybackState {
        match self {
            Engine::Audio(player) => player.state(),
            Engine::Demo(demo) => demo.state(Instant::now()),
        }
    }

    pub fn is_demo(&self) -> bool {
        matches!(self, Engine::Demo(_))
    }
}

/// Fake player: "plays" instantly from 20 s and loops, so lyrics can be verified
/// without audio.
#[derive(Default)]
pub struct DemoPlayer {
    track: Option<Track>,
    base: Duration,
    /// `Some` while playing: the instant `base` was valid.
    since: Option<Instant>,
}

impl DemoPlayer {
    fn play(&mut self, track: Track) {
        self.track = Some(track);
        self.base = DEMO_START;
        self.since = Some(Instant::now());
    }

    fn toggle_pause(&mut self) {
        let now = Instant::now();
        if self.since.is_some() {
            self.base = self.position(now);
            self.since = None;
        } else if self.track.is_some() {
            self.since = Some(now);
        }
    }

    fn seek(&mut self, position: Duration) {
        self.base = position;
        if self.since.is_some() {
            self.since = Some(Instant::now());
        }
    }

    fn loop_end(&self) -> Duration {
        let total = self
            .track
            .as_ref()
            .and_then(|t| t.duration_secs)
            .map_or(DEMO_LOOP_END, |s| Duration::from_secs(s.into()));
        total
            .min(DEMO_LOOP_END)
            .max(DEMO_START + Duration::from_secs(5))
    }

    fn position(&self, now: Instant) -> Duration {
        let pos = self.base + self.since.map_or(Duration::ZERO, |s| now - s);
        let end = self.loop_end();
        if pos < end {
            return pos;
        }
        let span = (end - DEMO_START).as_secs_f64();
        DEMO_START + Duration::from_secs_f64((pos - end).as_secs_f64() % span)
    }

    fn state(&self, now: Instant) -> PlaybackState {
        PlaybackState {
            track: self.track.clone(),
            status: match (&self.track, self.since) {
                (None, _) => PlaybackStatus::Idle,
                (Some(_), Some(_)) => PlaybackStatus::Playing,
                (Some(_), None) => PlaybackStatus::Paused,
            },
            position: self.position(now),
            duration: self
                .track
                .as_ref()
                .and_then(|t| t.duration_secs)
                .map(|s| Duration::from_secs(s.into())),
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
}

impl SmoothClock {
    pub fn position(&self, now: Instant) -> Duration {
        match (self.running, self.anchor_at) {
            (true, Some(at)) => self.anchor_pos + now.saturating_duration_since(at),
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
        if running != self.running || video_id != self.video_id || drift > RESYNC_THRESHOLD {
            self.anchor_pos = state.position;
            self.anchor_at = Some(now);
        }
        self.running = running;
        self.video_id = video_id;
    }

    /// Optimistic jump after a user seek (the next poll confirms or corrects it).
    pub fn seek(&mut self, position: Duration, now: Instant) {
        self.anchor_pos = position;
        self.anchor_at = Some(now);
    }
}
