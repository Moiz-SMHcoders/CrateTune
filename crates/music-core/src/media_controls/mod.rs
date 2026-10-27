//! OS media controls: "Now Playing" / media keys on macOS, the System Media
//! Transport Controls on Windows and MPRIS on Linux.
//!
//! Ported from Hummingbird `src/services/controllers.rs` and
//! `src/services/controllers/{macos,windows,mpris}.rs` (upstream commit
//! e15e8ba0ef6ff5445294030d373e3c8ccc7d780c). Copyright Hummingbird contributors,
//! Apache-2.0, modified: GPUI/tokio/async-trait removed, driven by
//! [`PlaybackState`] snapshots, artwork by URL or bytes, events go to a callback.
//!
//! # Wiring
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use music_core::{IosClientResolver, Player};
//! use music_core::media_controls::{MediaControlEvent, MediaControls, MediaControlsConfig};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let player = Player::new(Arc::new(IosClientResolver))?;
//! // On the UI thread, once the event loop / main window exists (see below).
//! let mut controls = MediaControls::for_player(
//!     MediaControlsConfig::new("music_macos", "Music"),
//!     player.clone(),
//!     |event| match event {
//!         MediaControlEvent::Next => { /* UI queue: play next */ }
//!         MediaControlEvent::Previous => { /* UI queue: play previous */ }
//!         _ => {}
//!     },
//! )?;
//! // Then from the UI's existing ~250 ms poll of `Player::state()`:
//! controls.update(&player.state());
//! # Ok(()) }
//! ```
//!
//! [`MediaControls::update`] only talks to the OS when something changed (new
//! track, play/pause, a seek, or every few seconds to resync the timeline), so
//! calling it on every poll is cheap. Play/pause/toggle/stop/seek/volume are
//! routed to the [`Player`] by [`MediaControls::for_player`]; next/previous have
//! no queue in music-core, so they go to the UI's callback. Use
//! [`MediaControls::new`] to receive every event yourself instead.
//!
//! # Threading and run-loop requirements
//!
//! The event handler must be `Send + Sync`, return quickly and never block: it
//! may run on the main thread (macOS) or an OS/D-Bus thread (Windows, Linux).
//! The routing done by [`MediaControls::for_player`] is just channel sends.
//! Keep the [`MediaControls`] value on the thread that created it; dropping it
//! unregisters the handlers and clears the system's now-playing info.
//!
//! * **macOS** (`MPRemoteCommandCenter` + `MPNowPlayingInfoCenter`): create it
//!   on the **main thread after the UI's event loop / `NSApplication` exists**
//!   (GPUI: inside `Application::run`; Slint: after the window is created, e.g.
//!   from `slint::invoke_from_event_loop`, or right before `run()`). Commands are
//!   delivered on the main run loop, so the main thread must keep running it —
//!   every UI toolkit does. macOS only shows a now-playing entry for a process
//!   that plays audio and runs as a regular GUI app; a bare CLI binary or an
//!   unbundled executable may not show up in Control Center. Artwork URLs are
//!   fetched on a short-lived background thread (macOS needs image bytes).
//! * **Windows** (SMTC): needs the `HWND` of the app's top-level window
//!   ([`MediaControlsConfig::hwnd`], e.g. `raw_window_handle`'s
//!   `Win32WindowHandle::hwnd`); create it on that window's UI thread. Button
//!   presses arrive on a WinRT thread-pool thread. Artwork URLs are handed to
//!   Windows, which fetches them itself.
//! * **Linux** (MPRIS over D-Bus via `mpris-server`/`zbus`): no thread
//!   requirement; connects to the session bus as
//!   `org.mpris.MediaPlayer2.<dbus_name>` and serves calls on zbus' internal
//!   thread. Artwork is published as `mpris:artUrl` (URLs as-is; bytes are
//!   written to a temp file).
//!
//! On other platforms, or with the `media-controls` cargo feature disabled,
//! [`MediaControls::new`] returns an error and nothing else changes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{PlaybackState, PlaybackStatus, Player, Track};

#[cfg(all(feature = "media-controls", target_os = "macos"))]
mod macos;
#[cfg(all(feature = "media-controls", target_os = "linux"))]
mod mpris;
#[cfg(all(feature = "media-controls", target_os = "windows"))]
mod windows;

/// A command from the OS (media keys, Control Center, taskbar flyout, MPRIS client).
#[derive(Debug, Clone, PartialEq)]
pub enum MediaControlEvent {
    Play,
    Pause,
    Toggle,
    Stop,
    Next,
    Previous,
    /// Absolute position.
    SeekTo(Duration),
    /// Relative seek in seconds (negative = backwards), e.g. MPRIS `Seek`.
    SeekBy(f64),
    /// 0.0..=1.0 (MPRIS clients can set the volume).
    SetVolume(f64),
}

/// How the app registers with the OS.
#[derive(Debug, Clone)]
pub struct MediaControlsConfig {
    /// Linux: MPRIS bus-name suffix (`org.mpris.MediaPlayer2.<dbus_name>`). Only
    /// `[A-Za-z0-9_-]`, not starting with a digit.
    pub dbus_name: String,
    /// Human-readable player name (MPRIS `Identity`).
    pub display_name: String,
    /// Linux: `.desktop` file name without the extension, if installed.
    pub desktop_entry: Option<String>,
    /// Windows: the top-level window's `HWND`. Required there, ignored elsewhere.
    pub hwnd: Option<std::num::NonZeroIsize>,
}

impl MediaControlsConfig {
    pub fn new(dbus_name: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            dbus_name: dbus_name.into(),
            display_name: display_name.into(),
            desktop_entry: None,
            hwnd: None,
        }
    }
}

/// What the OS shows for the current track.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MediaMetadata {
    pub title: String,
    pub artists: String,
    pub album: Option<String>,
    pub duration: Option<Duration>,
}

impl MediaMetadata {
    pub fn from_track(track: &Track, duration: Option<Duration>) -> Self {
        Self {
            title: track.title.clone(),
            artists: track.artists.clone(),
            album: track.album.clone(),
            duration: duration.or(track.duration_secs.map(|s| Duration::from_secs(s.into()))),
        }
    }
}

/// Playback status as the OS understands it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaPlayback {
    Stopped,
    Paused,
    Playing,
}

/// Error from [`MediaControls::new`] / [`MediaControls::for_player`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaControlsError(pub String);

impl std::fmt::Display for MediaControlsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "media controls: {}", self.0)
    }
}

impl std::error::Error for MediaControlsError {}

pub(crate) type BackendResult = std::result::Result<(), MediaControlsError>;
type Handler = Arc<dyn Fn(MediaControlEvent) + Send + Sync>;

#[cfg_attr(not(feature = "media-controls"), allow(dead_code))]
pub(crate) enum Artwork<'a> {
    Url(&'a str),
    Bytes(&'a [u8]),
}

/// One OS integration. Calls come from the thread owning [`MediaControls`].
pub(crate) trait Backend {
    /// New track (or new details for it): replaces everything shown, including artwork.
    fn set_metadata(&mut self, meta: Option<&MediaMetadata>) -> BackendResult;
    fn set_artwork(&mut self, art: Artwork<'_>) -> BackendResult;
    /// Status plus the position at this instant; the OS extrapolates while playing.
    fn set_playback(&mut self, playback: MediaPlayback, position: Duration) -> BackendResult;
    /// Latest position on every update, for backends that are queried (MPRIS).
    fn position_hint(&mut self, _position: Duration) {}
}

/// Square artwork size requested from YouTube Music's thumbnail server.
const ARTWORK_SIZE: u32 = 544;
/// A position differing this much from the extrapolated one is reported as a seek.
const SEEK_TOLERANCE: Duration = Duration::from_millis(1500);
/// Timeline resync interval while nothing else changes.
const RESYNC_EVERY: Duration = Duration::from_secs(5);

/// Handle to the OS media controls. See the [module docs](self).
pub struct MediaControls {
    backend: Box<dyn Backend>,
    track: Option<Track>,
    duration: Option<Duration>,
    /// Last published status/position and when it was published.
    published: Option<(MediaPlayback, Duration, Instant)>,
}

impl MediaControls {
    /// Registers with the OS; every event is passed to `handler`.
    pub fn new(
        config: MediaControlsConfig,
        handler: impl Fn(MediaControlEvent) + Send + Sync + 'static,
    ) -> Result<Self, MediaControlsError> {
        let handler: Handler = Arc::new(handler);
        Ok(Self::with_backend(platform_backend(&config, handler)?))
    }

    /// Like [`Self::new`], but play/pause/toggle/stop/seek/volume control
    /// `player` directly; the rest (next/previous) goes to `on_other`.
    pub fn for_player(
        config: MediaControlsConfig,
        player: Player,
        on_other: impl Fn(MediaControlEvent) + Send + Sync + 'static,
    ) -> Result<Self, MediaControlsError> {
        Self::new(config, player_handler(player, on_other))
    }

    fn with_backend(backend: Box<dyn Backend>) -> Self {
        Self {
            backend,
            track: None,
            duration: None,
            published: None,
        }
    }

    /// Publishes whatever changed since the last call. Call it with every
    /// [`Player::state`] snapshot the UI polls.
    pub fn update(&mut self, state: &PlaybackState) {
        let now = Instant::now();
        let same_track = match (&self.track, &state.track) {
            (Some(a), Some(b)) => a.video_id == b.video_id,
            (None, None) => true,
            _ => false,
        };
        if !same_track {
            self.track = state.track.clone();
            self.duration = state.duration;
            let meta = self
                .track
                .as_ref()
                .map(|t| MediaMetadata::from_track(t, state.duration));
            report(self.backend.set_metadata(meta.as_ref()));
            if let Some(url) = self
                .track
                .as_ref()
                .and_then(|t| t.artwork_url(ARTWORK_SIZE))
            {
                report(self.backend.set_artwork(Artwork::Url(&url)));
            }
            self.published = None;
        } else if state.duration.is_some() && state.duration != self.duration {
            // The decoder's duration replaced the search result's estimate.
            self.duration = state.duration;
            if let Some(track) = &self.track {
                let meta = MediaMetadata::from_track(track, state.duration);
                report(self.backend.set_metadata(Some(&meta)));
                self.published = None;
            }
        }

        let playback = match (&state.status, &self.track) {
            (_, None) => MediaPlayback::Stopped,
            (PlaybackStatus::Playing, _) => MediaPlayback::Playing,
            (PlaybackStatus::Paused | PlaybackStatus::Loading | PlaybackStatus::Ended, _) => {
                MediaPlayback::Paused
            }
            (PlaybackStatus::Idle | PlaybackStatus::Error(_), _) => MediaPlayback::Stopped,
        };
        self.backend.position_hint(state.position);
        let publish = match self.published {
            None => true,
            Some((last, pos, at)) => {
                let expected = match last {
                    MediaPlayback::Playing => pos + now.duration_since(at),
                    _ => pos,
                };
                last != playback
                    || state.position.abs_diff(expected) > SEEK_TOLERANCE
                    || now.duration_since(at) >= RESYNC_EVERY
            }
        };
        if publish {
            report(self.backend.set_playback(playback, state.position));
            self.published = Some((playback, state.position, now));
        }
    }

    /// Replaces the current track's artwork with already-downloaded image bytes
    /// (JPEG/PNG), e.g. the art the UI shows anyway. Call after [`Self::update`]
    /// saw the track.
    pub fn set_artwork(&mut self, image: &[u8]) {
        let result = self.backend.set_artwork(Artwork::Bytes(image));
        report(result);
    }
}

fn report(result: BackendResult) {
    if let Err(e) = result {
        log::warn!("{e}");
    }
}

/// Routes events to `player`; next/previous (and anything else) go to `on_other`.
pub fn player_handler(
    player: Player,
    on_other: impl Fn(MediaControlEvent) + Send + Sync + 'static,
) -> impl Fn(MediaControlEvent) + Send + Sync + 'static {
    move |event| match event {
        MediaControlEvent::Play => player.resume(),
        MediaControlEvent::Pause | MediaControlEvent::Stop => player.pause(),
        MediaControlEvent::Toggle => player.toggle_pause(),
        MediaControlEvent::SeekTo(position) => player.seek(position),
        MediaControlEvent::SeekBy(secs) => {
            let position = player.state().position.as_secs_f64() + secs;
            player.seek(Duration::from_secs_f64(position.max(0.0)));
        }
        MediaControlEvent::SetVolume(volume) => player.set_volume(volume as f32),
        other => on_other(other),
    }
}

#[cfg(all(feature = "media-controls", target_os = "macos"))]
fn platform_backend(
    _config: &MediaControlsConfig,
    handler: Handler,
) -> Result<Box<dyn Backend>, MediaControlsError> {
    Ok(Box::new(macos::MacControls::new(handler)?))
}

#[cfg(all(feature = "media-controls", target_os = "windows"))]
fn platform_backend(
    config: &MediaControlsConfig,
    handler: Handler,
) -> Result<Box<dyn Backend>, MediaControlsError> {
    Ok(Box::new(windows::WindowsControls::new(config, handler)?))
}

#[cfg(all(feature = "media-controls", target_os = "linux"))]
fn platform_backend(
    config: &MediaControlsConfig,
    handler: Handler,
) -> Result<Box<dyn Backend>, MediaControlsError> {
    Ok(Box::new(mpris::MprisControls::new(config, handler)?))
}

#[cfg(not(all(
    feature = "media-controls",
    any(target_os = "macos", target_os = "windows", target_os = "linux")
)))]
fn platform_backend(
    _config: &MediaControlsConfig,
    _handler: Handler,
) -> Result<Box<dyn Backend>, MediaControlsError> {
    Err(MediaControlsError(
        "not supported on this platform/build".into(),
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    enum Call {
        Metadata(Option<MediaMetadata>),
        ArtUrl(String),
        Playback(MediaPlayback, Duration),
    }

    struct Recorder(Arc<Mutex<Vec<Call>>>);

    impl Backend for Recorder {
        fn set_metadata(&mut self, meta: Option<&MediaMetadata>) -> BackendResult {
            self.0.lock().unwrap().push(Call::Metadata(meta.cloned()));
            Ok(())
        }
        fn set_artwork(&mut self, art: Artwork<'_>) -> BackendResult {
            if let Artwork::Url(url) = art {
                self.0.lock().unwrap().push(Call::ArtUrl(url.into()));
            }
            Ok(())
        }
        fn set_playback(&mut self, playback: MediaPlayback, position: Duration) -> BackendResult {
            self.0
                .lock()
                .unwrap()
                .push(Call::Playback(playback, position));
            Ok(())
        }
    }

    fn controls() -> (MediaControls, Arc<Mutex<Vec<Call>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let controls = MediaControls::with_backend(Box::new(Recorder(calls.clone())));
        (controls, calls)
    }

    fn take(calls: &Arc<Mutex<Vec<Call>>>) -> Vec<Call> {
        std::mem::take(&mut *calls.lock().unwrap())
    }

    fn track(id: &str) -> Track {
        Track {
            video_id: id.into(),
            title: format!("Song {id}"),
            artists: "Artist".into(),
            album: Some("Album".into()),
            duration_secs: Some(200),
            thumbnail_url: Some("https://lh3.googleusercontent.com/x=w120-h120-l90-rj".into()),
            ..Default::default()
        }
    }

    fn state(track: Option<Track>, status: PlaybackStatus, position_ms: u64) -> PlaybackState {
        PlaybackState {
            track,
            status,
            position: Duration::from_millis(position_ms),
            ..PlaybackState::default()
        }
    }

    #[test]
    fn publishes_only_what_changed() {
        let (mut controls, calls) = controls();
        let t = track("a");
        controls.update(&state(Some(t.clone()), PlaybackStatus::Loading, 0));
        assert_eq!(
            take(&calls),
            [
                Call::Metadata(Some(MediaMetadata {
                    title: "Song a".into(),
                    artists: "Artist".into(),
                    album: Some("Album".into()),
                    duration: Some(Duration::from_secs(200)),
                })),
                Call::ArtUrl("https://lh3.googleusercontent.com/x=w544-h544-l90-rj".into()),
                Call::Playback(MediaPlayback::Paused, Duration::ZERO),
            ]
        );

        controls.update(&state(Some(t.clone()), PlaybackStatus::Playing, 0));
        assert_eq!(
            take(&calls),
            [Call::Playback(MediaPlayback::Playing, Duration::ZERO)]
        );

        // Steady playback: the OS extrapolates, nothing to send.
        controls.update(&state(Some(t.clone()), PlaybackStatus::Playing, 250));
        assert!(take(&calls).is_empty());

        // A seek shows up as a position jump.
        controls.update(&state(Some(t.clone()), PlaybackStatus::Playing, 30_000));
        assert_eq!(
            take(&calls),
            [Call::Playback(
                MediaPlayback::Playing,
                Duration::from_secs(30)
            )]
        );

        controls.update(&state(Some(t.clone()), PlaybackStatus::Paused, 30_100));
        assert_eq!(
            take(&calls),
            [Call::Playback(
                MediaPlayback::Paused,
                Duration::from_millis(30_100)
            )]
        );

        // The decoder's exact duration refines the metadata.
        let mut s = state(Some(t), PlaybackStatus::Paused, 30_100);
        s.duration = Some(Duration::from_millis(199_500));
        controls.update(&s);
        let calls_now = take(&calls);
        assert!(matches!(&calls_now[0], Call::Metadata(Some(m)) if m.duration == s.duration));

        controls.update(&state(None, PlaybackStatus::Idle, 0));
        assert_eq!(
            take(&calls),
            [
                Call::Metadata(None),
                Call::Playback(MediaPlayback::Stopped, Duration::ZERO)
            ]
        );
    }

    #[test]
    fn new_track_republishes_everything() {
        let (mut controls, calls) = controls();
        controls.update(&state(Some(track("a")), PlaybackStatus::Playing, 5_000));
        take(&calls);
        controls.update(&state(Some(track("b")), PlaybackStatus::Loading, 0));
        let calls = take(&calls);
        assert_eq!(calls.len(), 3);
        assert!(matches!(&calls[0], Call::Metadata(Some(m)) if m.title == "Song b"));
        assert_eq!(
            calls[2],
            Call::Playback(MediaPlayback::Paused, Duration::ZERO)
        );
    }
}
