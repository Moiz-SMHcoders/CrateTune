//! Everything that hangs off the 250 ms poll besides the UI itself: OS media
//! controls, Last.fm/ListenBrainz scrobbling and Discord Rich Presence.
//!
//! All three are `music-core` types that never block on `update`; this module
//! only owns them, rebuilds them when settings change, and collects what the
//! settings panel shows (scrobble errors, Discord connection state).

use std::sync::mpsc::{Receiver, Sender, channel};

use music_core::PlaybackState;
use music_core::discord::{DiscordConfig, DiscordPresence, DiscordStatus};
use music_core::media_controls::{MediaControlEvent, MediaControls, MediaControlsConfig};
use music_core::scrobble::{LastFm, ListenBrainz, ScrobbleRules, ScrobbleService, Scrobbler};

use crate::clock::Engine;

/// Scrobbling credentials, as assembled from settings + secrets.
#[derive(Clone, Default, PartialEq)]
pub struct ScrobbleConfig {
    /// (api key, api secret, session key)
    pub lastfm: Option<(String, String, String)>,
    pub listenbrainz_token: Option<String>,
}

impl std::fmt::Debug for ScrobbleConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScrobbleConfig")
            .field("lastfm", &self.lastfm.is_some())
            .field("listenbrainz", &self.listenbrainz_token.is_some())
            .finish()
    }
}

pub struct Services {
    controls: Option<MediaControls>,
    /// Media-key events the UI has to handle (next/previous; everything in demo mode).
    events_tx: Sender<MediaControlEvent>,
    events_rx: Receiver<MediaControlEvent>,
    /// Video id whose downloaded artwork was handed to the OS.
    artwork_sent: Option<String>,
    scrobbler: Option<Scrobbler>,
    scrobble_config: ScrobbleConfig,
    scrobble_rules: ScrobbleRules,
    /// The presence and the config it was last given.
    discord: Option<(DiscordConfig, DiscordPresence)>,
    discord_status: DiscordStatus,
    /// Scrobbling and Discord stay off with the demo clock (nothing is really playing).
    real_audio: bool,
}

impl Default for Services {
    fn default() -> Self {
        let (events_tx, events_rx) = channel();
        Self {
            controls: None,
            events_tx,
            events_rx,
            artwork_sent: None,
            scrobbler: None,
            scrobble_config: ScrobbleConfig::default(),
            scrobble_rules: ScrobbleRules::default(),
            discord: None,
            discord_status: DiscordStatus::Idle,
            real_audio: false,
        }
    }
}

/// What changed during [`Services::update`] that the settings panel shows.
#[derive(Default)]
pub struct Report {
    pub scrobble_errors: Vec<String>,
    pub discord_status: Option<DiscordStatus>,
}

impl Services {
    /// Registers with the OS media controls. Call once, on the main thread, after
    /// the window exists (macOS needs the running `NSApplication`; Windows needs
    /// `hwnd`). With real audio, play/pause/seek/volume go straight to the
    /// `Player` and only next/previous reach the UI; with the demo clock every
    /// event goes to the UI.
    pub fn start_media_controls(&mut self, engine: &Engine, hwnd: Option<std::num::NonZeroIsize>) {
        let mut config =
            MediaControlsConfig::new(crate::identity::bus_name(), crate::identity::APP_NAME);
        config.hwnd = hwnd;
        let tx = self.events_tx.clone();
        let forward = move |event| {
            let _ = tx.send(event);
        };
        let result = match engine {
            Engine::Audio(player) => MediaControls::for_player(config, player.clone(), forward),
            Engine::Demo(_) => MediaControls::new(config, forward),
        };
        match result {
            Ok(controls) => self.controls = Some(controls),
            Err(e) => eprintln!("music-gpui: {e}"),
        }
        self.real_audio = !engine.is_demo();
    }

    /// Media-key events since the last call.
    pub fn take_events(&self) -> Vec<MediaControlEvent> {
        self.events_rx.try_iter().collect()
    }

    /// Feeds one poll snapshot to everything. `artwork` = the current track's
    /// downloaded hi-res art, once available.
    pub fn update(&mut self, state: &PlaybackState, artwork: Option<&[u8]>) -> Report {
        let mut report = Report::default();
        if let Some(controls) = &mut self.controls {
            controls.update(state);
            let id = state.track.as_ref().map(|t| &t.video_id);
            if let (Some(id), Some(bytes)) = (id, artwork)
                && self.artwork_sent.as_ref() != Some(id)
            {
                controls.set_artwork(bytes);
                self.artwork_sent = Some(id.clone());
            }
        }
        if !self.real_audio {
            return report;
        }
        if let Some(scrobbler) = &mut self.scrobbler {
            scrobbler.update(state);
            report.scrobble_errors = scrobbler.take_errors();
        }
        if let Some((_, discord)) = &mut self.discord {
            discord.update(state);
            let status = discord.status();
            if status != self.discord_status {
                self.discord_status = status.clone();
                report.discord_status = Some(status);
            }
        }
        report
    }

    /// Rebuilds the scrobbler when the credentials changed; applies the rules
    /// (percent, minimum length, delay) to it.
    pub fn configure_scrobbling(&mut self, config: ScrobbleConfig, rules: ScrobbleRules) {
        if rules != self.scrobble_rules {
            self.scrobble_rules = rules;
            if let Some(scrobbler) = &mut self.scrobbler {
                scrobbler.set_rules(rules);
            }
        }
        if config == self.scrobble_config {
            return;
        }
        let mut services: Vec<Box<dyn ScrobbleService>> = Vec::new();
        if let Some((key, secret, session)) = &config.lastfm {
            services.push(Box::new(
                LastFm::new(key.clone(), secret.clone()).with_session(session.clone()),
            ));
        }
        if let Some(token) = &config.listenbrainz_token {
            services.push(Box::new(
                ListenBrainz::new(token.clone())
                    .with_client(crate::identity::APP_ID, env!("CARGO_PKG_VERSION")),
            ));
        }
        // Dropping the old one lets its queued requests finish in the background.
        self.scrobbler = (!services.is_empty()).then(|| {
            let mut scrobbler = Scrobbler::new(services);
            scrobbler.set_rules(rules);
            scrobbler
        });
        self.scrobble_config = config;
    }

    /// `Some(config)` shows presence through that Discord application; a new
    /// application id reconnects, other changes apply to the running presence.
    pub fn configure_discord(&mut self, config: Option<DiscordConfig>) -> DiscordStatus {
        match (&mut self.discord, config) {
            (Some((current, _)), Some(config)) if *current == config => {}
            (Some((current, presence)), Some(config)) if current.client_id == config.client_id => {
                presence.set_config(config.clone());
                *current = config;
            }
            (slot, config) => {
                // Dropping the old presence clears it.
                *slot = config.map(|c| (c.clone(), DiscordPresence::new(c)));
                self.discord_status = DiscordStatus::Idle;
            }
        }
        self.discord_status.clone()
    }
}

/// The main window's HWND for the Windows SMTC (GPUI's `Window` implements
/// `raw-window-handle`'s `HasWindowHandle`). Untested: only built on macOS so far.
#[cfg(target_os = "windows")]
pub fn hwnd(window: &gpui::Window) -> Option<std::num::NonZeroIsize> {
    use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd),
        _ => None,
    }
}

#[cfg(not(target_os = "windows"))]
pub fn hwnd(_window: &gpui::Window) -> Option<std::num::NonZeroIsize> {
    None
}
