//! Discord Rich Presence ("Listening to …") over Discord's local IPC socket.
//!
//! Ported from Hummingbird `src/services/mmb/discord.rs` (upstream commit
//! e15e8ba0ef6ff5445294030d373e3c8ccc7d780c). Copyright Hummingbird contributors,
//! Apache-2.0, modified: driven by [`PlaybackState`] snapshots, IPC on a worker
//! thread with update coalescing instead of async + `watch` channels, caller
//! supplies the Discord application id and name, YouTube Music artwork URL.
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use music_core::{IosClientResolver, Player};
//! use music_core::discord::{DiscordConfig, DiscordPresence};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let player = Player::new(Arc::new(IosClientResolver))?;
//! // Create an application at https://discord.com/developers/applications.
//! let mut presence = DiscordPresence::new(DiscordConfig::new("YOUR_APPLICATION_ID"));
//! // From the UI's existing ~250 ms poll; never blocks.
//! presence.update(&player.state());
//! # Ok(()) }
//! ```
//!
//! If Discord isn't running the worker retries at most every 10 s;
//! [`DiscordPresence::status`] reports the connection for a settings screen.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use discord_rich_presence::activity::{
    Activity, ActivityType, Assets, Button, StatusDisplayType, Timestamps,
};
use discord_rich_presence::{DiscordIpc, DiscordIpcClient};

use crate::{PlaybackState, PlaybackStatus};

const RECONNECT_COOLDOWN: Duration = Duration::from_secs(10);
/// Discord allows 5 activity updates per 20 s; stay under it.
const MIN_UPDATE_INTERVAL: Duration = Duration::from_secs(4);
/// A position this far from the extrapolated one is a seek (new timestamps needed).
const SEEK_TOLERANCE: Duration = Duration::from_secs(2);
const ARTWORK_SIZE: u32 = 512;

#[derive(Debug, Clone, PartialEq)]
pub struct DiscordConfig {
    /// Discord application id ("client id"); its name is what "Listening to" shows
    /// unless [`Self::activity_name`] overrides it.
    pub client_id: String,
    pub activity_name: Option<String>,
    /// Image key (uploaded to the Discord app's Rich Presence assets) or URL used
    /// when a track has no artwork.
    pub fallback_image: Option<String>,
    /// "Listening to" (default), "Playing", "Watching" or "Competing in".
    pub activity_kind: ActivityKind,
    /// First line under the name. Placeholders: `{title}`, `{artist}`, `{album}`.
    pub details: String,
    /// Second line (same placeholders).
    pub state: String,
    /// Tooltip of the large image (same placeholders; empty = none).
    pub large_text: String,
    pub large_image: PresenceImage,
    pub small_image: PresenceImage,
    /// Tooltip of the small image (same placeholders; while paused "Paused" is shown).
    pub small_text: String,
    /// Up to two link buttons (Discord shows them to other users only).
    pub buttons: Vec<PresenceButton>,
    /// Keep the activity (without the running time) while paused instead of clearing it.
    pub show_when_paused: bool,
}

impl DiscordConfig {
    pub fn new(client_id: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            activity_name: None,
            fallback_image: None,
            activity_kind: ActivityKind::Listening,
            details: "{title}".into(),
            state: "by {artist}".into(),
            large_text: "{album}".into(),
            large_image: PresenceImage::Artwork,
            small_image: PresenceImage::None,
            small_text: String::new(),
            buttons: Vec::new(),
            show_when_paused: false,
        }
    }
}

/// Discord activity type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActivityKind {
    #[default]
    Listening,
    Playing,
    Watching,
    Competing,
}

/// Where an activity image comes from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PresenceImage {
    /// The track's artwork (YouTube Music thumbnail, 512 px).
    #[default]
    Artwork,
    /// An asset key of the Discord application or an image URL.
    Custom(String),
    None,
}

/// A link button on the activity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenceButton {
    /// At most 32 characters (longer labels are cut).
    pub label: String,
    pub url: ButtonUrl,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ButtonUrl {
    /// `https://music.youtube.com/watch?v=<id>` of the playing track.
    YouTubeMusic,
    Custom(String),
}

/// Fills `{title}`, `{artist}` and `{album}` in a presence template.
pub fn expand_template(template: &str, title: &str, artist: &str, album: Option<&str>) -> String {
    template
        .replace("{title}", title)
        .replace("{artist}", artist)
        .replace("{album}", album.unwrap_or(""))
        .trim()
        .to_owned()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DiscordStatus {
    /// Disabled, or no update requested yet.
    #[default]
    Idle,
    Connected,
    Disconnected {
        error: String,
    },
}

/// What should be shown; `None` clears the activity.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Presence {
    video_id: String,
    /// Shown while paused (`show_when_paused`): no running time.
    paused: bool,
    title: String,
    artists: String,
    album: Option<String>,
    artwork: Option<String>,
    /// Unix seconds when the track (virtually) started / will end.
    start: i64,
    end: Option<i64>,
}

/// Decides when the activity has to change. Pure, so it can be tested.
#[derive(Default)]
pub(crate) struct PresenceTracker {
    shown: Option<(Option<String>, PlaybackStatus)>,
    anchor: Option<(Duration, Instant)>,
    /// Paused tracks keep a (static) activity instead of clearing it.
    pub(crate) show_when_paused: bool,
}

impl PresenceTracker {
    /// `Some(update)` when Discord needs to be told something new.
    pub(crate) fn update(
        &mut self,
        state: &PlaybackState,
        now: Instant,
        wall: SystemTime,
    ) -> Option<Option<Presence>> {
        let playing = state.status == PlaybackStatus::Playing && state.track.is_some();
        let paused = !playing
            && self.show_when_paused
            && state.track.is_some()
            && matches!(
                state.status,
                PlaybackStatus::Paused | PlaybackStatus::Loading
            );
        let key = (
            state.track.as_ref().map(|t| t.video_id.clone()),
            if playing {
                PlaybackStatus::Playing
            } else if paused {
                PlaybackStatus::Paused
            } else {
                PlaybackStatus::Idle
            },
        );
        let seeked = playing
            && self.anchor.is_some_and(|(pos, at)| {
                state
                    .position
                    .abs_diff(pos + now.saturating_duration_since(at))
                    > SEEK_TOLERANCE
            });
        let changed = self.shown.as_ref() != Some(&key) || seeked;
        self.anchor = playing.then_some((state.position, now));
        if !changed {
            return None;
        }
        self.shown = Some(key);
        if !playing && !paused {
            return Some(None);
        }
        let track = state.track.as_ref()?;
        let unix_now = wall
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let start = (unix_now - state.position.as_secs_f64()).round() as i64;
        let duration = state
            .duration
            .or(track.duration_secs.map(|s| Duration::from_secs(s.into())));
        Some(Some(Presence {
            video_id: track.video_id.clone(),
            paused,
            title: track.title.clone(),
            artists: track.artists.clone(),
            album: track.album.clone(),
            artwork: track.artwork_url(ARTWORK_SIZE),
            start,
            end: duration.map(|d| start + d.as_secs() as i64),
        }))
    }
}

/// Discord Rich Presence fed from [`PlaybackState`] snapshots.
pub struct DiscordPresence {
    tracker: PresenceTracker,
    tx: Sender<Message>,
    status: Arc<Mutex<DiscordStatus>>,
    enabled: bool,
}

enum Message {
    Show(Option<Presence>),
    Configure(Box<DiscordConfig>),
}

impl DiscordPresence {
    /// Starts the worker thread; nothing connects until the first update.
    pub fn new(config: DiscordConfig) -> Self {
        let (tx, rx) = mpsc::channel();
        let status = Arc::new(Mutex::new(DiscordStatus::Idle));
        let tracker = PresenceTracker {
            show_when_paused: config.show_when_paused,
            ..PresenceTracker::default()
        };
        let worker = Worker {
            client: None,
            config,
            status: status.clone(),
            last_attempt: None,
        };
        if let Err(e) = thread::Builder::new()
            .name("music-core-discord".into())
            .spawn(move || worker.run(rx))
        {
            log::warn!("could not start the Discord thread: {e}");
        }
        Self {
            tracker,
            tx,
            status,
            enabled: true,
        }
    }

    /// Applies new activity settings; the activity is shown again with them on
    /// the next update. A different `client_id` reconnects.
    pub fn set_config(&mut self, config: DiscordConfig) {
        self.tracker = PresenceTracker {
            show_when_paused: config.show_when_paused,
            ..PresenceTracker::default()
        };
        let _ = self.tx.send(Message::Configure(Box::new(config)));
    }

    /// Feeds one snapshot; any IPC happens on the worker thread.
    pub fn update(&mut self, state: &PlaybackState) {
        if !self.enabled {
            return;
        }
        if let Some(presence) = self
            .tracker
            .update(state, Instant::now(), SystemTime::now())
        {
            let _ = self.tx.send(Message::Show(presence));
        }
    }

    /// Disabling clears the activity; re-enabling shows the next update.
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        self.tracker = PresenceTracker {
            show_when_paused: self.tracker.show_when_paused,
            ..PresenceTracker::default()
        };
        if !enabled {
            let _ = self.tx.send(Message::Show(None));
        }
    }

    pub fn status(&self) -> DiscordStatus {
        self.status.lock().unwrap().clone()
    }
}

impl Drop for DiscordPresence {
    fn drop(&mut self) {
        let _ = self.tx.send(Message::Show(None));
    }
}

struct Worker {
    client: Option<DiscordIpcClient>,
    config: DiscordConfig,
    status: Arc<Mutex<DiscordStatus>>,
    last_attempt: Option<Instant>,
}

impl Worker {
    fn run(mut self, rx: Receiver<Message>) {
        let mut pending: Option<Option<Presence>> = None;
        let mut last_sent: Option<Instant> = None;
        loop {
            // Coalesce: keep only the newest wanted presence until we may send.
            let wait = last_sent.map_or(Duration::ZERO, |t| {
                MIN_UPDATE_INTERVAL.saturating_sub(t.elapsed())
            });
            let received = if pending.is_some() {
                rx.recv_timeout(wait)
            } else {
                rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
            };
            match received {
                Ok(Message::Show(presence)) => pending = Some(presence),
                Ok(Message::Configure(config)) => {
                    if config.client_id != self.config.client_id
                        && let Some(mut client) = self.client.take()
                    {
                        let _ = client.clear_activity();
                        let _ = client.close();
                        self.last_attempt = None;
                    }
                    self.config = *config;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    if let Some(presence) = pending.take() {
                        self.send(presence.as_ref());
                    }
                    if let Some(mut client) = self.client.take() {
                        let _ = client.close();
                    }
                    return;
                }
            }
            if pending.is_some() && last_sent.is_none_or(|t| t.elapsed() >= MIN_UPDATE_INTERVAL) {
                let presence = pending.take().expect("checked");
                if self.send(presence.as_ref()) {
                    last_sent = Some(Instant::now());
                } else if presence.is_some() {
                    // Not connected: keep the newest presence and retry (connecting
                    // is itself rate-limited by RECONNECT_COOLDOWN).
                    pending = Some(presence);
                    last_sent = Some(Instant::now());
                }
            }
        }
    }

    fn set_status(&self, status: DiscordStatus) {
        *self.status.lock().unwrap() = status;
    }

    fn ensure_connected(&mut self) -> bool {
        if self.client.is_none() {
            if self
                .last_attempt
                .is_some_and(|t| t.elapsed() < RECONNECT_COOLDOWN)
            {
                return false;
            }
            self.last_attempt = Some(Instant::now());
            let mut client = DiscordIpcClient::new(&self.config.client_id);
            match client.connect() {
                Ok(()) => {
                    self.set_status(DiscordStatus::Connected);
                    self.client = Some(client);
                }
                Err(e) => {
                    log::debug!("Discord IPC connect failed: {e}");
                    self.set_status(DiscordStatus::Disconnected {
                        error: e.to_string(),
                    });
                    return false;
                }
            }
        }
        true
    }

    /// True when Discord accepted the update.
    fn send(&mut self, presence: Option<&Presence>) -> bool {
        if !self.ensure_connected() {
            return false;
        }
        let Some(client) = self.client.as_mut() else {
            return false;
        };
        let result = match presence {
            Some(p) => client.set_activity(activity(&self.config, p)),
            None => client.clear_activity(),
        };
        match result {
            Ok(()) => true,
            Err(e) => {
                // The socket is probably gone (Discord restarted); reconnect next time.
                log::debug!("Discord IPC update failed: {e}");
                if let Some(mut client) = self.client.take() {
                    let _ = client.close();
                }
                self.last_attempt = None;
                self.set_status(DiscordStatus::Disconnected {
                    error: e.to_string(),
                });
                false
            }
        }
    }
}

fn activity<'a>(config: &'a DiscordConfig, p: &'a Presence) -> Activity<'a> {
    let fill = |template: &str| expand_template(template, &p.title, &p.artists, p.album.as_deref());
    let image = |source: &'a PresenceImage| -> Option<&'a str> {
        match source {
            PresenceImage::Artwork => p.artwork.as_deref().or(config.fallback_image.as_deref()),
            PresenceImage::Custom(key) => Some(key.as_str()).filter(|k| !k.trim().is_empty()),
            PresenceImage::None => None,
        }
    };
    let mut assets = Assets::new();
    if let Some(large) = image(&config.large_image) {
        assets = assets.large_image(large);
        let text = fill(&config.large_text);
        if text.chars().count() >= 2 {
            assets = assets.large_text(text);
        }
    }
    if let Some(small) = image(&config.small_image) {
        assets = assets.small_image(small);
        let text = if p.paused {
            "Paused".to_owned()
        } else {
            fill(&config.small_text)
        };
        if text.chars().count() >= 2 {
            assets = assets.small_text(text);
        }
    }
    // Discord rejects fields shorter than 2 characters.
    let line = |template: &str, fallback: &str| {
        let text = fill(template);
        if text.chars().count() >= 2 {
            text
        } else {
            fallback.to_owned()
        }
    };
    let details = line(&config.details, &p.title);
    let state = line(&config.state, &format!("by {}", p.artists));
    let kind = match config.activity_kind {
        ActivityKind::Listening => ActivityType::Listening,
        ActivityKind::Playing => ActivityType::Playing,
        ActivityKind::Watching => ActivityType::Watching,
        ActivityKind::Competing => ActivityType::Competing,
    };
    let mut activity = Activity::new()
        .activity_type(kind)
        .status_display_type(StatusDisplayType::Details)
        .details(truncate(details, 128))
        .state(truncate(state, 128))
        .assets(assets);
    if !p.paused {
        let mut timestamps = Timestamps::new().start(p.start);
        if let Some(end) = p.end {
            timestamps = timestamps.end(end);
        }
        activity = activity.timestamps(timestamps);
    }
    let buttons: Vec<Button> = config
        .buttons
        .iter()
        .filter_map(|b| {
            let url = match &b.url {
                ButtonUrl::YouTubeMusic => {
                    format!("https://music.youtube.com/watch?v={}", p.video_id)
                }
                ButtonUrl::Custom(url) => url.trim().to_owned(),
            };
            let label = truncate(b.label.trim().to_owned(), 32);
            (!label.is_empty() && url.starts_with("http")).then(|| Button::new(label, url))
        })
        .take(2)
        .collect();
    if !buttons.is_empty() {
        activity = activity.buttons(buttons);
    }
    if let Some(name) = &config.activity_name {
        activity = activity.name(name.as_str());
    }
    activity
}

fn truncate(mut text: String, max_chars: usize) -> String {
    if let Some((idx, _)) = text.char_indices().nth(max_chars) {
        text.truncate(idx);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Track;

    fn state(id: &str, status: PlaybackStatus, position: u64) -> PlaybackState {
        PlaybackState {
            track: Some(Track {
                video_id: id.into(),
                title: "Song".into(),
                artists: "Artist".into(),
                album: Some("Album".into()),
                duration_secs: Some(200),
                thumbnail_url: Some("https://lh3.googleusercontent.com/x=w120-h120".into()),
                ..Default::default()
            }),
            status,
            position: Duration::from_secs(position),
            ..PlaybackState::default()
        }
    }

    #[test]
    fn updates_only_on_track_status_or_seek_changes() {
        let mut tracker = PresenceTracker::default();
        let t0 = Instant::now();
        let wall = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let at = |s: u64| (t0 + Duration::from_secs(s), wall + Duration::from_secs(s));

        let (now, w) = at(0);
        let shown = tracker
            .update(&state("a", PlaybackStatus::Playing, 0), now, w)
            .unwrap()
            .unwrap();
        assert_eq!((shown.start, shown.end), (1_000_000, Some(1_000_200)));
        assert_eq!(
            shown.artwork.as_deref(),
            Some("https://lh3.googleusercontent.com/x=w512-h512-l90-rj")
        );

        // Steady playback: Discord counts on its own.
        let (now, w) = at(10);
        assert_eq!(
            tracker.update(&state("a", PlaybackStatus::Playing, 10), now, w),
            None
        );

        // Seek to 100 s: new timestamps.
        let (now, w) = at(11);
        let shown = tracker
            .update(&state("a", PlaybackStatus::Playing, 100), now, w)
            .unwrap()
            .unwrap();
        assert_eq!(shown.start, 1_000_011 - 100);

        // Pause clears; resume shows again.
        let (now, w) = at(12);
        assert_eq!(
            tracker.update(&state("a", PlaybackStatus::Paused, 101), now, w),
            Some(None)
        );
        let (now, w) = at(20);
        assert_eq!(
            tracker.update(&state("a", PlaybackStatus::Paused, 101), now, w),
            None
        );
        let (now, w) = at(21);
        assert!(matches!(
            tracker.update(&state("a", PlaybackStatus::Playing, 101), now, w),
            Some(Some(_))
        ));

        // Another track.
        let (now, w) = at(22);
        assert!(matches!(
            tracker.update(&state("b", PlaybackStatus::Playing, 0), now, w),
            Some(Some(_))
        ));
    }

    #[test]
    fn paused_presence_is_kept_when_configured() {
        let mut tracker = PresenceTracker {
            show_when_paused: true,
            ..PresenceTracker::default()
        };
        let t0 = Instant::now();
        let wall = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let shown = tracker
            .update(&state("a", PlaybackStatus::Playing, 0), t0, wall)
            .unwrap()
            .unwrap();
        assert!(!shown.paused);
        let paused = tracker
            .update(
                &state("a", PlaybackStatus::Paused, 5),
                t0 + Duration::from_secs(5),
                wall,
            )
            .unwrap()
            .unwrap();
        assert!(paused.paused);
        assert_eq!(paused.video_id, "a");
        // Stopped/ended still clears.
        assert_eq!(
            tracker.update(
                &state("a", PlaybackStatus::Ended, 200),
                t0 + Duration::from_secs(9),
                wall
            ),
            Some(None)
        );
    }

    #[test]
    fn templates_fill_placeholders() {
        assert_eq!(
            expand_template("{title} — {artist}", "Song", "Artist", None),
            "Song — Artist"
        );
        assert_eq!(
            expand_template(" from {album} ", "S", "A", Some("LP")),
            "from LP"
        );
        assert_eq!(expand_template("{album}", "S", "A", None), "");
        assert_eq!(truncate("abcdef".into(), 3), "abc");
        assert_eq!(truncate("äöü".into(), 5), "äöü");
        let config = DiscordConfig::new("1");
        assert_eq!(config.details, "{title}");
        assert_eq!(config.activity_kind, ActivityKind::Listening);
    }

    #[test]
    fn disconnected_worker_reports_status_without_blocking_updates() {
        // No Discord in the test environment (or an invalid id): updates must still return
        // immediately and the status must become Disconnected or Connected, never hang.
        let mut presence = DiscordPresence::new(DiscordConfig::new("0"));
        let started = Instant::now();
        presence.update(&state("a", PlaybackStatus::Playing, 0));
        assert!(started.elapsed() < Duration::from_millis(50));
        for _ in 0..50 {
            if presence.status() != DiscordStatus::Idle {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert_ne!(presence.status(), DiscordStatus::Idle);
    }
}
