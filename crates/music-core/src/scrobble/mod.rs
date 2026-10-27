//! Scrobbling to Last.fm and ListenBrainz.
//!
//! The clients are ported from Hummingbird `src/services/mmb/{lastfm,listenbrainz}`
//! (upstream commit e15e8ba0ef6ff5445294030d373e3c8ccc7d780c). Copyright
//! Hummingbird contributors, Apache-2.0, modified: blocking `ureq` instead of
//! async reqwest, std types instead of GPUI/chrono, and the "when to scrobble"
//! bookkeeping pulled out into [`ScrobbleTracker`], which works on
//! [`PlaybackState`] snapshots.
//!
//! # Wiring
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use music_core::{IosClientResolver, Player};
//! use music_core::scrobble::{LastFm, ListenBrainz, ScrobbleService, Scrobbler};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let player = Player::new(Arc::new(IosClientResolver))?;
//! // Credentials come from the user / app config at runtime (see `LastFm` for the auth flow).
//! let services: Vec<Box<dyn ScrobbleService>> = vec![
//!     Box::new(LastFm::new("API_KEY", "API_SECRET").with_session("SESSION_KEY")),
//!     Box::new(ListenBrainz::new("USER_TOKEN")),
//! ];
//! let mut scrobbler = Scrobbler::new(services);
//! // From the UI's existing ~250 ms poll:
//! scrobbler.update(&player.state());
//! # Ok(()) }
//! ```
//!
//! [`Scrobbler::update`] is cheap and never blocks: the network requests run
//! on a background thread, failures are logged (and returned by
//! [`Scrobbler::take_errors`]). Use [`ScrobbleTracker`] directly to drive your
//! own clients.
//!
//! # Rules (Last.fm's, also fine for ListenBrainz)
//!
//! * "Now playing" is sent when a track starts playing and again when it is
//!   resumed after a pause.
//! * A play is scrobbled once the track is longer than 30 s and has actually
//!   been played for half its length or 4 minutes, whichever comes first.
//!   Only position advances during playback count: pauses, loading and seeks
//!   forward don't. The timestamp is when playback started.
//! * Replaying a track after it ended is a new play.
//! * Previews (no PoToken) usually stop before the threshold, so they rarely scrobble.

mod lastfm;
mod listenbrainz;

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

pub use lastfm::{LastFm, LastFmSession};
pub use listenbrainz::ListenBrainz;

use crate::{PlaybackState, PlaybackStatus, Result, Track};

/// The track data scrobbling services need.
#[derive(Debug, Clone, PartialEq)]
pub struct ScrobbleTrack {
    /// [`Track::artists`] as shown (YouTube Music joins several with ", ").
    pub artist: String,
    pub title: String,
    pub album: Option<String>,
    pub duration: Option<Duration>,
    /// When this play started (Last.fm's scrobble timestamp).
    pub started_at: SystemTime,
    /// YouTube video id, for services that accept a link to the source.
    pub video_id: Option<String>,
}

impl ScrobbleTrack {
    pub fn from_track(track: &Track, duration: Option<Duration>, started_at: SystemTime) -> Self {
        Self {
            artist: track.artists.clone(),
            title: track.title.clone(),
            album: track.album.clone(),
            duration: duration.or(track.duration_secs.map(|s| Duration::from_secs(s.into()))),
            started_at,
            video_id: Some(track.video_id.clone()),
        }
    }

    /// Seconds since the Unix epoch of [`Self::started_at`].
    pub fn timestamp(&self) -> u64 {
        self.started_at
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }
}

/// What a scrobbling service should do now.
#[derive(Debug, Clone, PartialEq)]
pub enum ScrobbleEvent {
    NowPlaying(ScrobbleTrack),
    Scrobble(ScrobbleTrack),
}

/// Last.fm: tracks this short or shorter are never scrobbled.
pub const MIN_TRACK_LENGTH: Duration = Duration::from_secs(30);
/// Last.fm: a play counts after this much listening even if under half the track.
pub const MAX_PLAY_THRESHOLD: Duration = Duration::from_secs(240);
/// When a play counts as a scrobble. The default is Last.fm's rule: tracks
/// longer than 30 s, after half the track or 4 minutes, whichever comes first.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrobbleRules {
    /// Fraction of the track that has to be played (0.0..=1.0).
    pub percent: f32,
    /// Tracks this short or shorter are never scrobbled.
    pub min_track_length: Duration,
    /// A play counts after this much listening even below `percent`.
    pub max_delay: Duration,
}

impl Default for ScrobbleRules {
    fn default() -> Self {
        Self {
            percent: 0.5,
            min_track_length: MIN_TRACK_LENGTH,
            max_delay: MAX_PLAY_THRESHOLD,
        }
    }
}

impl ScrobbleRules {
    /// Listening time after which a track of `duration` is scrobbled, or
    /// `None` when it is too short to ever count.
    pub fn threshold(&self, duration: Duration) -> Option<Duration> {
        if duration <= self.min_track_length {
            return None;
        }
        let percent = if self.percent.is_finite() {
            self.percent.clamp(0.0, 1.0)
        } else {
            0.5
        };
        Some(duration.mul_f32(percent).min(self.max_delay))
    }
}

/// Position advances up to this much beyond the wall-clock time are still counted
/// (polling jitter); larger jumps are seeks.
const JITTER: Duration = Duration::from_millis(1000);

struct Play {
    track: Track,
    started_at: SystemTime,
    started: bool,
    played: Duration,
    now_playing_sent: bool,
    scrobbled: bool,
}

/// Decides now-playing updates and scrobbles from [`PlaybackState`] snapshots.
/// Pure bookkeeping, no I/O; see the [module docs](self) for the rules.
#[derive(Default)]
pub struct ScrobbleTracker {
    play: Option<Play>,
    last: Option<(PlaybackStatus, Duration, Instant)>,
    rules: ScrobbleRules,
}

impl ScrobbleTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_rules(rules: ScrobbleRules) -> Self {
        Self {
            rules,
            ..Self::default()
        }
    }

    /// Changes the rules; applies to the play in progress too.
    pub fn set_rules(&mut self, rules: ScrobbleRules) {
        self.rules = rules;
    }

    pub fn rules(&self) -> ScrobbleRules {
        self.rules
    }

    /// Feeds one snapshot; call it on every poll of [`crate::Player::state`].
    pub fn update(&mut self, state: &PlaybackState) -> Vec<ScrobbleEvent> {
        self.update_at(state, Instant::now(), SystemTime::now())
    }

    /// [`Self::update`] with explicit clocks (monotonic for play time,
    /// wall-clock for timestamps), for tests and replays.
    pub fn update_at(
        &mut self,
        state: &PlaybackState,
        now: Instant,
        wall: SystemTime,
    ) -> Vec<ScrobbleEvent> {
        let mut events = Vec::new();
        let last = self.last.take();
        self.last = Some((state.status.clone(), state.position, now));

        let Some(track) = &state.track else {
            self.play = None;
            return events;
        };
        let replay = matches!(&last, Some((PlaybackStatus::Ended, ..)))
            && matches!(
                state.status,
                PlaybackStatus::Playing | PlaybackStatus::Loading
            );
        let new_play = replay
            || self
                .play
                .as_ref()
                .is_none_or(|p| p.track.video_id != track.video_id);
        if new_play {
            self.play = Some(Play {
                track: track.clone(),
                started_at: wall,
                started: false,
                played: Duration::ZERO,
                now_playing_sent: false,
                scrobbled: false,
            });
        }
        let play = self.play.as_mut().expect("set above");
        if state.status != PlaybackStatus::Playing {
            return events;
        }

        let duration = state
            .duration
            .or(track.duration_secs.map(|s| Duration::from_secs(s.into())));
        let snapshot =
            |play: &Play| ScrobbleTrack::from_track(&play.track, duration, play.started_at);
        if !play.started {
            // Loading time doesn't count: the play starts when audio does.
            play.started = true;
            play.started_at = wall;
        }
        let resumed = matches!(&last, Some((PlaybackStatus::Paused, ..)));
        if !play.now_playing_sent || (resumed && !new_play) {
            play.now_playing_sent = true;
            events.push(ScrobbleEvent::NowPlaying(snapshot(play)));
        }

        if !new_play
            && let Some((PlaybackStatus::Playing, position, at)) = last
            && state.position > position
        {
            let advanced = state.position - position;
            if advanced <= now.saturating_duration_since(at) + JITTER {
                play.played += advanced;
            }
        }

        if !play.scrobbled
            && let Some(threshold) = duration.and_then(|d| self.rules.threshold(d))
            && play.played >= threshold
        {
            play.scrobbled = true;
            events.push(ScrobbleEvent::Scrobble(snapshot(play)));
        }
        events
    }
}

/// A scrobbling backend. Calls are made from the [`Scrobbler`] worker thread
/// and may block on the network.
pub trait ScrobbleService: Send {
    /// Short name for logs, e.g. "Last.fm".
    fn name(&self) -> &str;
    fn now_playing(&mut self, track: &ScrobbleTrack) -> Result<()>;
    fn scrobble(&mut self, track: &ScrobbleTrack) -> Result<()>;
}

/// [`ScrobbleTracker`] plus a worker thread that sends its events to every
/// service. Dropping it finishes the queued requests in the background.
pub struct Scrobbler {
    tracker: ScrobbleTracker,
    tx: Sender<ScrobbleEvent>,
    errors: Arc<Mutex<Vec<String>>>,
}

impl Scrobbler {
    pub fn new(mut services: Vec<Box<dyn ScrobbleService>>) -> Self {
        let (tx, rx) = mpsc::channel::<ScrobbleEvent>();
        let errors = Arc::new(Mutex::new(Vec::new()));
        let worker_errors = errors.clone();
        let spawned = thread::Builder::new()
            .name("music-core-scrobble".into())
            .spawn(move || {
                for event in rx {
                    for service in &mut services {
                        let (what, result) = match &event {
                            ScrobbleEvent::NowPlaying(t) => ("now playing", service.now_playing(t)),
                            ScrobbleEvent::Scrobble(t) => ("scrobble", service.scrobble(t)),
                        };
                        if let Err(e) = result {
                            let message = format!("{} {what}: {e}", service.name());
                            log::warn!("{message}");
                            let mut errors = worker_errors.lock().unwrap();
                            errors.push(message);
                            let excess = errors.len().saturating_sub(16);
                            errors.drain(..excess);
                        }
                    }
                }
            });
        if let Err(e) = spawned {
            log::warn!("could not start the scrobble thread: {e}");
        }
        Self {
            tracker: ScrobbleTracker::new(),
            tx,
            errors,
        }
    }

    /// See [`ScrobbleTracker::set_rules`].
    pub fn set_rules(&mut self, rules: ScrobbleRules) {
        self.tracker.set_rules(rules);
    }

    /// Feeds one snapshot; any resulting requests are queued, never awaited.
    pub fn update(&mut self, state: &PlaybackState) {
        for event in self.tracker.update(state) {
            let _ = self.tx.send(event);
        }
    }

    /// Request failures since the last call (most recent 16), for showing in the UI.
    pub fn take_errors(&self) -> Vec<String> {
        std::mem::take(&mut *self.errors.lock().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str, secs: u32) -> Track {
        Track {
            video_id: id.into(),
            title: format!("Song {id}"),
            artists: "Artist".into(),
            album: None,
            duration_secs: Some(secs),
            thumbnail_url: None,
            ..Default::default()
        }
    }

    /// Drives a tracker with a simulated clock.
    struct Sim {
        tracker: ScrobbleTracker,
        t0: Instant,
        wall0: SystemTime,
        elapsed: Duration,
    }

    impl Sim {
        fn new() -> Self {
            Self {
                tracker: ScrobbleTracker::new(),
                t0: Instant::now(),
                wall0: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                elapsed: Duration::ZERO,
            }
        }

        fn step(
            &mut self,
            track: &Track,
            status: PlaybackStatus,
            position: f64,
        ) -> Vec<ScrobbleEvent> {
            let state = PlaybackState {
                track: Some(track.clone()),
                status,
                position: Duration::from_secs_f64(position),
                ..PlaybackState::default()
            };
            self.tracker
                .update_at(&state, self.t0 + self.elapsed, self.wall0 + self.elapsed)
        }

        /// Plays `track` from `from` for `secs` seconds, polling every 250 ms.
        fn play(&mut self, track: &Track, from: f64, secs: f64) -> Vec<ScrobbleEvent> {
            let mut events = Vec::new();
            let steps = (secs * 4.0) as usize;
            for i in 0..=steps {
                events.extend(self.step(track, PlaybackStatus::Playing, from + i as f64 * 0.25));
                if i < steps {
                    self.elapsed += Duration::from_millis(250);
                }
            }
            events
        }

        fn wait(&mut self, secs: f64) {
            self.elapsed += Duration::from_secs_f64(secs);
        }
    }

    fn kinds(events: &[ScrobbleEvent]) -> Vec<&'static str> {
        events
            .iter()
            .map(|e| match e {
                ScrobbleEvent::NowPlaying(_) => "now",
                ScrobbleEvent::Scrobble(_) => "scrobble",
            })
            .collect()
    }

    #[test]
    fn custom_rules_change_the_threshold() {
        let rules = ScrobbleRules {
            percent: 0.25,
            min_track_length: Duration::from_secs(60),
            max_delay: Duration::from_secs(30),
        };
        assert_eq!(rules.threshold(Duration::from_secs(60)), None);
        assert_eq!(
            rules.threshold(Duration::from_secs(100)),
            Some(Duration::from_secs(25))
        );
        assert_eq!(
            rules.threshold(Duration::from_secs(400)),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            ScrobbleRules::default().threshold(Duration::from_secs(200)),
            Some(Duration::from_secs(100))
        );

        let mut sim = Sim::new();
        sim.tracker.set_rules(rules);
        let t = track("a", 100);
        assert_eq!(kinds(&sim.play(&t, 0.0, 20.0)), vec!["now"]);
        assert_eq!(kinds(&sim.play(&t, 20.0, 6.0)), vec!["scrobble"]);
        // Too short under these rules.
        let short = track("b", 50);
        assert_eq!(kinds(&sim.play(&short, 0.0, 50.0)), vec!["now"]);
    }

    #[test]
    fn scrobbles_after_half_the_track() {
        let mut sim = Sim::new();
        let t = track("a", 200);
        assert!(sim.step(&t, PlaybackStatus::Loading, 0.0).is_empty());
        sim.wait(2.0);
        let events = sim.play(&t, 0.0, 99.0);
        assert_eq!(kinds(&events), ["now"]);
        let events = sim.play(&t, 99.25, 2.0);
        assert_eq!(kinds(&events), ["scrobble"]);
        let ScrobbleEvent::Scrobble(s) = &events[0] else {
            unreachable!()
        };
        // Timestamp is when audio started, not when loading began.
        assert_eq!(s.timestamp(), 1_700_000_002);
        assert_eq!(s.duration, Some(Duration::from_secs(200)));
        assert_eq!(s.video_id.as_deref(), Some("a"));
        // Only once per play.
        assert!(sim.play(&t, 101.5, 50.0).is_empty());
    }

    #[test]
    fn long_tracks_scrobble_after_four_minutes() {
        let mut sim = Sim::new();
        let t = track("long", 1_200);
        let events = sim.play(&t, 0.0, 239.0);
        assert_eq!(kinds(&events), ["now"]);
        assert_eq!(kinds(&sim.play(&t, 239.25, 1.0)), ["scrobble"]);
    }

    #[test]
    fn short_tracks_never_scrobble() {
        let mut sim = Sim::new();
        let t = track("short", 30);
        assert_eq!(kinds(&sim.play(&t, 0.0, 30.0)), ["now"]);
    }

    #[test]
    fn pauses_and_seeks_do_not_count() {
        let mut sim = Sim::new();
        let t = track("a", 200);
        sim.play(&t, 0.0, 50.0);
        // Paused for 5 minutes.
        sim.wait(0.25);
        assert!(sim.step(&t, PlaybackStatus::Paused, 50.0).is_empty());
        sim.wait(300.0);
        assert!(sim.step(&t, PlaybackStatus::Paused, 50.0).is_empty());
        sim.wait(0.25);
        // Resuming refreshes "now playing".
        assert_eq!(kinds(&sim.play(&t, 50.0, 10.0)), ["now"]);
        // Seeking from 60 s to 150 s skips 90 s that weren't heard.
        sim.wait(0.25);
        assert!(sim.play(&t, 150.0, 20.0).is_empty());
        // 50 + 10 + 20 = 80 s heard; 20 more reach 100 s = half.
        assert_eq!(kinds(&sim.play(&t, 170.25, 20.5)), ["scrobble"]);
    }

    #[test]
    fn slow_polling_still_counts_real_play_time() {
        let mut sim = Sim::new();
        let t = track("a", 100);
        sim.step(&t, PlaybackStatus::Playing, 0.0);
        let mut events = Vec::new();
        for i in 1..=12 {
            // Hidden window: polled every 5 s.
            sim.wait(5.0);
            events.extend(sim.step(&t, PlaybackStatus::Playing, i as f64 * 5.0));
        }
        assert_eq!(kinds(&events), ["scrobble"]);
    }

    #[test]
    fn new_track_and_replay_start_new_plays() {
        let mut sim = Sim::new();
        let a = track("a", 60);
        let b = track("b", 60);
        assert_eq!(kinds(&sim.play(&a, 0.0, 31.0)), ["now", "scrobble"]);
        sim.wait(0.25);
        // Switching tracks: new now-playing, nothing carried over.
        assert_eq!(kinds(&sim.play(&b, 0.0, 20.0)), ["now"]);
        sim.wait(0.25);
        assert_eq!(kinds(&sim.play(&a, 0.0, 20.0)), ["now"]);

        // Ended, then replayed: a fresh play that can scrobble again.
        sim.play(&a, 20.25, 39.0);
        sim.wait(0.25);
        sim.step(&a, PlaybackStatus::Ended, 60.0);
        sim.wait(1.0);
        assert_eq!(kinds(&sim.play(&a, 0.0, 31.0)), ["now", "scrobble"]);
    }

    #[test]
    fn previews_do_not_scrobble() {
        let mut sim = Sim::new();
        // A 7-minute track of which only ~60 s could be fetched.
        let t = track("preview", 430);
        assert_eq!(kinds(&sim.play(&t, 0.0, 60.0)), ["now"]);
        sim.wait(0.25);
        assert!(sim.step(&t, PlaybackStatus::Ended, 60.0).is_empty());
    }
}
