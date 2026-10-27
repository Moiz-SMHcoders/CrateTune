//! Playback source (real audio engine or a wall-clock demo) and the smoothed
//! clock the UI interpolates between 250 ms polls.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use music_core::{PlaybackState, PlaybackStatus, Player, Track};

/// Where the demo clock starts (Instant Crush's first lyric is at ~26 s).
const DEMO_START: Duration = Duration::from_secs(20);
/// Loop length when the track duration is unknown.
const DEMO_FALLBACK_LEN: Duration = Duration::from_secs(300);

#[derive(Clone)]
pub enum Playback {
    Audio(Player),
    /// No audio device (or `MUSIC_DEMO_CLOCK=1`): position comes from a wall clock.
    Demo(Rc<RefCell<DemoClock>>),
}

impl Playback {
    pub fn demo() -> Self {
        Self::Demo(Rc::new(RefCell::new(DemoClock::default())))
    }

    pub fn is_demo(&self) -> bool {
        matches!(self, Self::Demo(_))
    }

    pub fn play(&self, track: Track) {
        match self {
            Self::Audio(p) => p.play(track),
            Self::Demo(d) => d.borrow_mut().play(track),
        }
    }

    pub fn toggle_pause(&self) {
        match self {
            Self::Audio(p) => p.toggle_pause(),
            Self::Demo(d) => d.borrow_mut().toggle_pause(),
        }
    }

    pub fn seek(&self, position: Duration) {
        match self {
            Self::Audio(p) => p.seek(position),
            Self::Demo(d) => d.borrow_mut().seek(position),
        }
    }

    pub fn state(&self) -> PlaybackState {
        match self {
            Self::Audio(p) => p.state(),
            Self::Demo(d) => d.borrow().state(),
        }
    }
}

#[derive(Default)]
pub struct DemoClock {
    track: Option<Track>,
    /// Position at `since`, or the paused position when `since` is `None`.
    anchor: Duration,
    since: Option<Instant>,
}

impl DemoClock {
    fn len(&self) -> Duration {
        self.track
            .as_ref()
            .and_then(|t| t.duration_secs)
            .map(|s| Duration::from_secs(s.into()))
            .filter(|d| *d > DEMO_START)
            .unwrap_or(DEMO_FALLBACK_LEN)
    }

    fn position(&self) -> Duration {
        let raw = self.anchor + self.since.map_or(Duration::ZERO, |s| s.elapsed());
        let len = self.len();
        if raw < len {
            raw
        } else {
            // Loop back to the demo start.
            let span = (len - DEMO_START).as_secs_f64();
            DEMO_START + Duration::from_secs_f64((raw - DEMO_START).as_secs_f64() % span)
        }
    }

    fn play(&mut self, track: Track) {
        self.track = Some(track);
        self.anchor = DEMO_START;
        self.since = Some(Instant::now());
    }

    fn toggle_pause(&mut self) {
        self.anchor = self.position();
        self.since = match self.since {
            Some(_) => None,
            None => Some(Instant::now()),
        };
    }

    fn seek(&mut self, position: Duration) {
        self.anchor = position;
        if self.since.is_some() {
            self.since = Some(Instant::now());
        }
    }

    fn state(&self) -> PlaybackState {
        let Some(track) = &self.track else {
            return PlaybackState::default();
        };
        PlaybackState {
            track: Some(track.clone()),
            status: if self.since.is_some() {
                PlaybackStatus::Playing
            } else {
                PlaybackStatus::Paused
            },
            position: self.position(),
            duration: Some(self.len()),
            ..PlaybackState::default()
        }
    }
}

/// Rust-side model of the clock the `.slint` side interpolates with `animation-tick()`.
///
/// The audio thread only updates its position every 100 ms and the UI polls every
/// 250 ms, so raw positions are stale by a varying amount. Re-anchoring to each
/// raw value would make the lyrics jitter backwards; instead small errors are
/// slewed out (20 % per poll) and only large ones (seek, stall) snap.
pub struct ClockSync {
    pos: f64,
    at: Instant,
    playing: bool,
    /// After a seek, ignore polled positions until the engine has caught up.
    hold_until: Option<Instant>,
}

impl Default for ClockSync {
    fn default() -> Self {
        Self {
            pos: 0.0,
            at: Instant::now(),
            playing: false,
            hold_until: None,
        }
    }
}

impl ClockSync {
    const SNAP: f64 = 0.35;
    const SLEW: f64 = 0.2;

    fn predicted(&self, now: Instant) -> f64 {
        if self.playing {
            self.pos + now.duration_since(self.at).as_secs_f64()
        } else {
            self.pos
        }
    }

    /// Feeds a polled position; returns the position to anchor the UI clock at, or
    /// `None` when nothing changed (paused and unchanged), so idle polls don't redraw.
    pub fn update(&mut self, polled: Duration, playing: bool) -> Option<f64> {
        let now = Instant::now();
        let predicted = self.predicted(now);
        let polled = polled.as_secs_f64();
        let next = match self.hold_until {
            Some(until) if now < until => predicted,
            _ => {
                self.hold_until = None;
                let err = polled - predicted;
                if playing != self.playing || err.abs() > Self::SNAP {
                    polled
                } else {
                    predicted + err * Self::SLEW
                }
            }
        };
        let changed = playing || self.playing != playing || (next - self.pos).abs() > 1e-3;
        self.pos = next;
        self.at = now;
        self.playing = playing;
        changed.then_some(next)
    }

    pub fn seek(&mut self, to: f64, hold: bool) {
        self.pos = to;
        self.at = Instant::now();
        self.hold_until = hold.then(|| self.at + Duration::from_millis(500));
    }

    pub fn now(&self) -> f64 {
        self.predicted(Instant::now())
    }
}
