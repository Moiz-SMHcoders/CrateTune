//! The settings page: owns the persisted [`Settings`] and the secrets, and
//! renders the section list plus the selected section (each section is a file
//! next to this one, registered in [`super::SECTIONS`]).
//!
//! The panel is a long-lived entity; the `Settings { section }` route shows it
//! as a page. It never touches the player or the services directly: it emits
//! [`SettingsEvent`]s and `MusicApp` applies them, and `MusicApp` pushes
//! scrobble errors / Discord state back in from its poll.
//!
//! Changes are saved 500 ms after the last edit (and on quit). Network calls
//! (Last.fm auth, ListenBrainz token validation) and keychain access run on
//! worker threads.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Icon, Sizable as _, h_flex,
    input::{InputEvent, InputState},
    scroll::ScrollableElement as _,
    slider::{SliderEvent, SliderState},
    v_flex,
};
use music_core::discord::DiscordStatus;
use music_core::scrobble::{LastFm, ListenBrainz};
use music_core::{EqPreset, EqualizerSettings};

use super::file::{self, EQ_RANGE_DB, Settings, WindowGeometry};
use super::prefs_ui::PrefsUi;
use super::reveal;
use super::search_index::{Entry, Kind};
use super::search_ui::{SearchEvent, SettingsSearch};
use super::{SECTIONS, SectionId};
use crate::queue::Repeat;
use crate::secrets::{self, Backend, Secret, Stored};
use crate::services::ScrobbleConfig;
use crate::theme::ThemeChoice;
use crate::util::off_thread;

const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);
const MAX_ERRORS: usize = 5;

pub enum SettingsEvent {
    /// Apply to the player now (`None` = equalizer off).
    Equalizer(Option<EqualizerSettings>),
    /// Credentials or Discord settings changed: rebuild the services.
    Integrations,
    /// Theme or font scale changed.
    Appearance,
    /// The UI language setting changed (`MusicApp::apply_language`).
    Language,
    /// The user picked another section (the app keeps the route in sync).
    Section(SectionId),
    /// Shuffle / repeat changed on the settings page (apply to the queue).
    PlaybackModes,
    /// Library / privacy / storage / account preferences or the YouTube cookie changed.
    Library,
    /// Content language / country, filters or autoplay changed.
    Content,
    /// The proxy was (re)applied; online pages should reload.
    Network,
    /// Engine options (quality, crossfade, speed, device, …) changed.
    Audio,
    /// Lyrics providers / keys changed: rebuild the lyrics service.
    Lyrics,
    /// Now Playing / lyrics display options changed (re-render only).
    NowPlaying,
    /// The stream quality changed (after `Audio`): switch the playing song to it.
    Quality,
}

pub(super) enum LastFmFlow {
    Idle,
    Busy(&'static str),
    /// Token issued; the user approves it in the browser, then presses Finish.
    Waiting {
        key: String,
        secret: String,
        token: String,
        error: Option<String>,
    },
    Failed(String),
}

pub(super) enum Validation {
    Idle,
    Busy,
    Failed(String),
}

/// Where secrets go / came from; translated when shown (`Display`).
#[derive(Clone)]
pub(super) enum SecretStore {
    /// The keychain (`file_too`: some secrets are in the settings file as well).
    Backend {
        backend: Arc<dyn Backend>,
        file_too: bool,
    },
    /// Only the settings file.
    File(FileReason),
}

/// Why secrets are only in the settings file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FileReason {
    KeychainDisabled,
    ReadFailed,
    Unavailable,
}

impl std::fmt::Display for SecretStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretStore::Backend {
                backend,
                file_too: false,
            } => f.write_str(backend.label()),
            SecretStore::Backend {
                backend,
                file_too: true,
            } => f.write_str(&t!(
                "settings.panel.secrets.backend_and_file",
                backend = backend.label()
            )),
            SecretStore::File(FileReason::KeychainDisabled) => {
                f.write_str(t!("settings.panel.secrets.file_keychain_disabled"))
            }
            SecretStore::File(FileReason::ReadFailed) => {
                f.write_str(t!("settings.panel.secrets.file_keychain_read_failed"))
            }
            SecretStore::File(FileReason::Unavailable) => {
                f.write_str(t!("settings.panel.secrets.file_keychain_unavailable"))
            }
        }
    }
}

pub struct SettingsPanel {
    pub(super) settings: Settings,
    pub(super) path: Option<PathBuf>,
    /// Load/save problems, shown in the Storage and Integrations sections.
    pub(super) file_error: Option<String>,
    save_task: Option<Task<()>>,
    save_pending: bool,
    /// Emit `Integrations` when the pending save fires (typed ids/keys).
    integrations_dirty: bool,

    backend: Option<Arc<dyn Backend>>,
    pub(super) secrets: BTreeMap<Secret, String>,
    pub(super) secrets_loaded: bool,
    /// Where secrets go / came from, for the status line.
    pub(super) secret_store: SecretStore,

    pub section: SectionId,
    scroll: ScrollHandle,
    pub(super) eq_sliders: Vec<Entity<SliderState>>,
    pub(super) lastfm_key: Entity<InputState>,
    pub(super) lastfm_secret: Entity<InputState>,
    pub(super) lastfm: LastFmFlow,
    pub(super) lb_token: Entity<InputState>,
    pub(super) lb: Validation,
    pub(super) discord_id: Entity<InputState>,
    pub(super) discord_status: DiscordStatus,
    pub(super) scrobble_errors: VecDeque<String>,
    /// Content & region section (language / country pickers).
    pub(super) content: super::content::ContentState,
    /// UI language picker (Appearance, onboarding).
    pub(super) language: super::language::LanguagePicker,
    /// Network section (proxy draft fields, status).
    pub(super) network: super::network::NetworkState,
    /// Player / lyrics / Now Playing / activity widgets (`prefs_ui.rs`).
    pub(super) prefs: PrefsUi,
    /// Modules section (inputs, review, open settings form, status).
    pub(super) modules: super::modules::ModulesState,
    /// SponsorBlock / Music Together / Spotify inputs (`integrations_ui.rs`).
    pub(super) extras: super::integrations_ui::ExtrasUi,
    /// Customize theme widgets, pending settings / theme import (`theme_ui.rs`).
    pub(super) theme_ui: super::theme_ui::ThemeUi,
    /// Appearance / behaviour profiles (`profiles.rs`).
    pub(super) profiles: super::profiles::ProfileStore,
    /// Keyboard shortcuts section (recording, conflict prompt).
    pub(super) shortcuts_ui: super::shortcuts_ui::ShortcutsUi,
    /// The search field at the top of the sidebar (`search_ui.rs`).
    search: Entity<SettingsSearch>,
    /// A tab stop put on a revealed row, so focus can move on to its control.
    reveal_focus: FocusHandle,
    reveal_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SettingsEvent> for SettingsPanel {}

impl SettingsPanel {
    /// Loads `settings.json` synchronously (small file) and the secrets on a worker thread.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let path = file::settings_path();
        // No settings file yet: a fresh install, show the first-run guide.
        let first_run = path.as_ref().is_some_and(|p| !p.exists());
        let (mut settings, file_error) = match &path {
            Some(path) => Settings::load(path),
            None => (
                Settings::default(),
                Some(t!("settings.file.no_config_dir").into()),
            ),
        };
        if let Some(e) = &file_error {
            eprintln!("music-gpui: settings: {e}");
        }
        if first_run {
            settings.onboarded = false;
            // YouTube Music content in the UI language (and the system's region)
            // until the user picks something else (Content & region).
            let locale = settings.appearance.ui_locale();
            settings.content.language = crate::i18n::content_language(locale).to_owned();
            if let Some(region) = crate::i18n::system_region()
                && super::content::COUNTRIES.iter().any(|(c, _)| *c == region)
            {
                settings.content.country = region;
            }
        }

        let gains = settings.equalizer.gains();
        let eq_sliders: Vec<_> = gains
            .iter()
            .map(|&gain| {
                cx.new(|_| {
                    SliderState::new()
                        .min(-EQ_RANGE_DB as f32)
                        .max(EQ_RANGE_DB as f32)
                        .step(0.5)
                        .default_value(gain as f32)
                })
            })
            .collect();
        let input = |placeholder: crate::i18n::Key,
                     value: &str,
                     masked: bool,
                     window: &mut Window,
                     cx: &mut Context<Self>| {
            let value = value.to_owned();
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(masked)
                    .default_value(value)
            });
            crate::i18n::track_placeholder(&input, placeholder, window, cx);
            input
        };
        let lastfm_key = input(
            k!("settings.panel.placeholder.api_key"),
            &settings.lastfm.api_key,
            false,
            window,
            cx,
        );
        let lastfm_secret = input(
            k!("settings.panel.placeholder.shared_secret"),
            "",
            true,
            window,
            cx,
        );
        let lb_token = input(
            k!("settings.panel.placeholder.user_token"),
            "",
            true,
            window,
            cx,
        );
        let discord_id = input(
            k!("settings.panel.placeholder.application_id"),
            &settings.discord.client_id,
            false,
            window,
            cx,
        );

        let mut subscriptions: Vec<Subscription> = eq_sliders
            .iter()
            .enumerate()
            .map(|(band, slider)| {
                cx.subscribe(slider, move |this, _, event: &SliderEvent, cx| {
                    let value = match event {
                        SliderEvent::Change(v) | SliderEvent::Release(v) => v.start(),
                    };
                    this.set_band(band, value as f64, cx);
                })
            })
            .collect();
        for input in [&lastfm_key, &discord_id] {
            subscriptions.push(cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.sync_inputs(cx);
                }
            }));
        }
        let (content, content_subscriptions) =
            super::content::ContentState::new(&settings, window, cx);
        subscriptions.extend(content_subscriptions);
        let (language, language_subscription) =
            super::language::LanguagePicker::new(&settings, window, cx);
        subscriptions.push(language_subscription);
        let network = super::network::NetworkState::new(&settings, window, cx);
        let modules = super::modules::ModulesState::new(window, cx);
        let (prefs, prefs_subscriptions) = PrefsUi::new(&settings, window, cx);
        subscriptions.extend(prefs_subscriptions);
        let (extras, extras_subscriptions) =
            super::integrations_ui::ExtrasUi::new(&settings, window, cx);
        subscriptions.extend(extras_subscriptions);
        let profiles = super::profiles_ui::load_store(&settings);
        let (theme_ui, theme_subscriptions) =
            super::theme_ui::ThemeUi::new(&settings.custom_theme, window, cx);
        subscriptions.extend(theme_subscriptions);
        let search = cx.new(|cx| SettingsSearch::new(window, cx));
        subscriptions.push(cx.subscribe_in(
            &search,
            window,
            |this, _, event: &SearchEvent, window, cx| match event {
                SearchEvent::Pick(entry) => this.reveal(entry, window, cx),
            },
        ));
        subscriptions.push(cx.on_app_quit(|this, _| {
            this.flush();
            async {}
        }));

        let backend: Option<Arc<dyn Backend>> = secrets::default_backend().map(Arc::from);
        let secret_store = match &backend {
            Some(b) => SecretStore::Backend {
                backend: b.clone(),
                file_too: false,
            },
            None => SecretStore::File(FileReason::KeychainDisabled),
        };
        let mut this = Self {
            settings,
            path,
            file_error,
            save_task: None,
            save_pending: false,
            integrations_dirty: false,
            backend,
            secrets: BTreeMap::new(),
            secrets_loaded: false,
            secret_store,
            section: SectionId::default(),
            scroll: ScrollHandle::new(),
            eq_sliders,
            lastfm_key,
            lastfm_secret,
            lastfm: LastFmFlow::Idle,
            lb_token,
            lb: Validation::Idle,
            discord_id,
            discord_status: DiscordStatus::Idle,
            scrobble_errors: VecDeque::new(),
            content,
            language,
            network,
            prefs,
            modules,
            extras,
            theme_ui,
            profiles,
            shortcuts_ui: Default::default(),
            search,
            reveal_focus: cx.focus_handle().tab_stop(true),
            reveal_task: None,
            _subscriptions: subscriptions,
        };
        // Before any page loads (the password, if any, follows once read).
        if let Err(e) = this.apply_proxy() {
            eprintln!("music-gpui: proxy not applied: {e}");
        }
        this.load_secrets(window, cx);
        this
    }

    fn load_secrets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        let file = self.settings.secrets.0.clone();
        // Only what the settings say exists, so the keychain isn't touched at all
        // until an account has been set up.
        let s = &self.settings;
        let wanted: Vec<Secret> = [
            (Secret::LastFmShared, !s.lastfm.api_key.is_empty()),
            (Secret::LastFmSession, s.lastfm.user.is_some()),
            (Secret::ListenBrainzToken, s.listenbrainz.user.is_some()),
            (Secret::YouTubeCookie, s.library.account.is_some()),
            (Secret::ProxyPassword, s.network.has_password),
            (Secret::PaxsenixKey, s.lyrics.paxsenix_key_set),
            (Secret::BetterLyricsKey, s.lyrics.betterlyrics_key_set),
            (Secret::AiTranslateKey, s.lyrics.ai_key_set),
            (Secret::SpotifyRefreshToken, s.spotify.user.is_some()),
            (Secret::AiFilterKey, s.ai_filter.key_set),
        ]
        .into_iter()
        .filter_map(|(secret, wanted)| wanted.then_some(secret))
        .collect();
        cx.spawn_in(window, async move |this, cx| {
            let loaded =
                off_thread(move || secrets::load_all(backend.as_deref(), &file, &wanted)).await;
            this.update_in(cx, |this, window, cx| {
                for e in &loaded.errors {
                    eprintln!("music-gpui: keychain read failed ({e}); using the settings file");
                }
                if !loaded.errors.is_empty() {
                    this.secret_store = SecretStore::File(FileReason::ReadFailed);
                } else if !this.settings.secrets.is_empty()
                    && let SecretStore::Backend { file_too, .. } = &mut this.secret_store
                {
                    *file_too = true;
                }
                if std::env::var_os("MUSIC_TRACE").is_some() {
                    eprintln!(
                        "music-gpui: secrets loaded: {} present, {} from the keychain",
                        loaded.values.len(),
                        loaded.in_keychain
                    );
                }
                this.secrets = loaded.values;
                this.secrets_loaded = true;
                for (secret, input) in [
                    (Secret::LastFmShared, this.lastfm_secret.clone()),
                    (Secret::ListenBrainzToken, this.lb_token.clone()),
                ] {
                    if let Some(value) = this.secrets.get(&secret).cloned() {
                        input.update(cx, |input, cx| input.set_value(value, window, cx));
                    }
                }
                if let Some(password) = this.secrets.get(&Secret::ProxyPassword).cloned() {
                    this.network.show_password(password, window, cx);
                    if let Err(e) = this.apply_proxy() {
                        eprintln!("music-gpui: proxy not applied: {e}");
                    }
                    cx.emit(SettingsEvent::Network);
                }
                this.fill_secret_inputs(window, cx);
                cx.emit(SettingsEvent::Integrations);
                cx.emit(SettingsEvent::Library);
                cx.emit(SettingsEvent::Lyrics);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A loaded secret (after the keychain read finished).
    pub(super) fn secret(&self, secret: Secret) -> Option<String> {
        self.secrets.get(&secret).cloned()
    }

    // ---- what MusicApp reads -------------------------------------------------

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn equalizer(&self) -> Option<EqualizerSettings> {
        self.settings.equalizer.player_settings()
    }

    pub fn scrobble_config(&self) -> ScrobbleConfig {
        let s = &self.settings;
        let secret = |k| self.secrets.get(&k).cloned();
        ScrobbleConfig {
            lastfm: s
                .lastfm
                .user
                .as_ref()
                .filter(|_| !s.lastfm.api_key.is_empty())
                .and_then(|_| {
                    Some((
                        s.lastfm.api_key.clone(),
                        secret(Secret::LastFmShared)?,
                        secret(Secret::LastFmSession)?,
                    ))
                }),
            listenbrainz_token: s
                .listenbrainz
                .user
                .as_ref()
                .and_then(|_| secret(Secret::ListenBrainzToken)),
        }
    }

    pub fn show_section(&mut self, section: SectionId, cx: &mut Context<Self>) {
        if self.section != section {
            self.section = section;
            self.scroll.set_offset(gpui::point(px(0.), px(0.)));
            cx.notify();
        }
    }

    pub fn scroll_handle(&self) -> ScrollHandle {
        self.scroll.clone()
    }

    // ---- search ------------------------------------------------------------------

    /// The settings search field has the keyboard focus.
    pub fn search_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.search.read(cx).is_focused(window, cx)
    }

    /// ⌘F / Ctrl+F while the settings page shows.
    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.focus(window, cx));
    }

    /// `@settings-search=QUERY`: the ranked results as text.
    pub fn script_search(
        &mut self,
        query: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<String> {
        self.search
            .update(cx, |s, cx| s.script_query(query, window, cx))
    }

    /// `@settings-search-pick=N`.
    pub fn script_search_pick(&mut self, index: usize, cx: &mut Context<Self>) -> bool {
        self.search.update(cx, |s, cx| s.script_pick(index, cx))
    }

    /// `@settings-anchors`: shows every section in turn and prints the index
    /// entries whose rows it didn't draw (and the anchor a reveal falls back to).
    pub fn audit_anchors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let executor = cx.background_executor().clone();
        let start = self.section;
        cx.spawn_in(window, async move |this, cx| {
            let (mut drawn_total, mut hidden_total) = (0, 0);
            for section in SECTIONS.iter().map(|s| s.id) {
                let ok = this.update(cx, |this, cx| {
                    this.show_section(section, cx);
                    // The whole section, not just what's in view.
                    reveal::start_audit();
                    cx.notify();
                });
                if ok.is_err() {
                    return;
                }
                executor.timer(Duration::from_millis(250)).await;
                let drawn = reveal::take_audit();
                let mut hidden = Vec::new();
                let entries = super::search_index::all()
                    .iter()
                    .filter(|e| e.section == section && e.kind == Kind::Setting);
                let mut count = 0;
                for entry in entries {
                    count += 1;
                    if drawn.contains(&entry.anchor) {
                        continue;
                    }
                    let mut fallback = entry.anchor;
                    while let Some((parent, _)) = fallback.rsplit_once('.') {
                        fallback = parent;
                        if drawn.contains(&fallback) {
                            break;
                        }
                    }
                    hidden.push(format!("{} → {fallback}", entry.anchor));
                }
                drawn_total += count - hidden.len();
                hidden_total += hidden.len();
                eprintln!(
                    "music-gpui: anchors {}: {}/{count} drawn{}",
                    section.key(),
                    count - hidden.len(),
                    if hidden.is_empty() {
                        String::new()
                    } else {
                        format!("; not shown now: {}", hidden.join(", "))
                    }
                );
            }
            eprintln!("music-gpui: anchors: {drawn_total} drawn, {hidden_total} not shown now");
            this.update(cx, |this, cx| {
                this.show_section(start, cx);
                cx.emit(SettingsEvent::Section(start));
            })
            .ok();
        })
        .detach();
    }

    /// Opens a search result's section, scrolls its row into view, flashes it
    /// and moves the focus to its control (see `reveal.rs`).
    fn reveal(&mut self, entry: &'static Entry, window: &mut Window, cx: &mut Context<Self>) {
        self.show_section(entry.section, cx);
        cx.emit(SettingsEvent::Section(entry.section));
        if entry.kind == Kind::Section {
            reveal::cancel();
            self.scroll.set_offset(gpui::point(px(0.), px(0.)));
            cx.notify();
            return;
        }
        let color = crate::theme::AppColors::get(cx).accent.alpha(0.2);
        let animate = crate::effects::motion() == crate::effects::Motion::Full;
        reveal::start(
            entry.anchor,
            self.scroll.clone(),
            self.reveal_focus.clone(),
            color,
            animate,
        );
        cx.notify();
        let executor = cx.background_executor().clone();
        self.reveal_task = Some(cx.spawn_in(window, async move |this, cx| {
            // Wait for the row to be drawn (a frame or two after the section
            // switch, whenever frames come); a row that isn't drawn hands over to
            // its parent anchor.
            let mut found = false;
            for _ in 0..200 {
                executor.timer(Duration::from_millis(16)).await;
                let done = this
                    .update_in(cx, |_, window, cx| {
                        if reveal::take_revealed().is_some() {
                            if let Some(handle) = reveal::focus_handle() {
                                window.focus(&handle, cx);
                                window.focus_next(cx);
                                let control = handle.contains_focused(window, cx)
                                    && !handle.is_focused(window);
                                if !control {
                                    window.focus(&handle, cx);
                                }
                                if std::env::var_os("MUSIC_TRACE").is_some() {
                                    eprintln!(
                                        "music-gpui: revealed {}, focus on {}",
                                        reveal::flashing().unwrap_or("?"),
                                        if control { "its control" } else { "the row" }
                                    );
                                }
                            }
                            cx.notify();
                            return true;
                        }
                        cx.notify();
                        false
                    })
                    .unwrap_or(true);
                if done {
                    found = true;
                    break;
                }
                // Drawn twice without the target: it isn't on the page now.
                if reveal::renders_since_target() >= 2 {
                    match reveal::pending().and_then(|id| id.rsplit_once('.')) {
                        Some((parent, _)) => reveal::retarget(parent),
                        None => break,
                    }
                }
            }
            if !found {
                reveal::cancel();
                return;
            }
            executor
                .timer(reveal::FLASH + Duration::from_millis(50))
                .await;
            this.update_in(cx, |_, window, cx| {
                reveal::end_flash();
                // The row stays a tab stop only while it holds the focus itself.
                if reveal::focus_handle().is_some_and(|h| !h.is_focused(window)) {
                    reveal::cancel();
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn set_theme(&mut self, theme: ThemeChoice, cx: &mut Context<Self>) {
        if self.settings.appearance.theme != theme {
            self.settings.appearance.theme = theme;
            cx.emit(SettingsEvent::Appearance);
            self.changed(false, cx);
        }
    }

    /// `code` is a catalog code or [`file::SYSTEM_LANGUAGE`].
    pub fn set_language(&mut self, code: &str, cx: &mut Context<Self>) {
        if self.settings.appearance.language != code {
            self.settings.appearance.language = code.to_owned();
            cx.emit(SettingsEvent::Language);
            self.changed(false, cx);
            if !self.settings.onboarded {
                // First run: YouTube Music content follows the UI language.
                let hl = crate::i18n::content_language(self.settings.appearance.ui_locale());
                self.set_content(|c| c.language = hl.to_owned(), cx);
            }
        }
    }

    pub fn set_font_scale(&mut self, scale: f32, cx: &mut Context<Self>) {
        if (self.settings.appearance.font_scale - scale).abs() > f32::EPSILON {
            self.settings.appearance.font_scale = scale;
            self.settings.normalize();
            cx.emit(SettingsEvent::Appearance);
            self.changed(false, cx);
        }
    }

    /// Settings → Storage → "Artwork in memory" (MiB of decoded covers).
    pub fn set_artwork_memory(&mut self, mb: u32, cx: &mut Context<Self>) {
        if self.settings.appearance.artwork_memory_mb != mb {
            self.settings.appearance.artwork_memory_mb = mb;
            self.settings.normalize();
            cx.emit(SettingsEvent::Appearance);
            self.changed(false, cx);
        }
    }

    /// From the settings page: store and tell the app to apply.
    pub fn choose_playback_modes(&mut self, shuffle: bool, repeat: Repeat, cx: &mut Context<Self>) {
        self.set_playback_modes(shuffle, repeat, cx);
        cx.emit(SettingsEvent::PlaybackModes);
    }

    /// From the app (bar / queue panel / shortcuts): store only.
    pub fn set_playback_modes(&mut self, shuffle: bool, repeat: Repeat, cx: &mut Context<Self>) {
        let p = &mut self.settings.playback;
        if p.shuffle != shuffle || p.repeat != repeat {
            p.shuffle = shuffle;
            p.repeat = repeat;
            self.changed(false, cx);
        }
    }

    pub fn set_window_geometry(&mut self, geometry: WindowGeometry, cx: &mut Context<Self>) {
        if self.settings.window != Some(geometry) && geometry.is_sane() {
            self.settings.window = Some(geometry);
            self.changed(false, cx);
        }
    }

    pub fn set_mini_player_geometry(&mut self, geometry: WindowGeometry, cx: &mut Context<Self>) {
        if self.settings.mini_player != Some(geometry) && geometry.is_sane() {
            self.settings.mini_player = Some(geometry);
            self.changed(false, cx);
        }
    }

    pub fn set_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        if (self.settings.volume - volume).abs() > f32::EPSILON {
            self.settings.volume = volume;
            self.changed(false, cx);
        }
    }

    pub fn push_scrobble_errors(&mut self, errors: Vec<String>, cx: &mut Context<Self>) {
        self.scrobble_errors.extend(errors);
        while self.scrobble_errors.len() > MAX_ERRORS {
            self.scrobble_errors.pop_front();
        }
        cx.notify();
    }

    pub fn set_discord_status(&mut self, status: DiscordStatus, cx: &mut Context<Self>) {
        self.discord_status = status;
        cx.notify();
    }

    // ---- persistence ---------------------------------------------------------

    /// Schedules a save (and, if `integrations`, an `Integrations` event) after
    /// [`SAVE_DEBOUNCE`] without further changes.
    pub(super) fn changed(&mut self, integrations: bool, cx: &mut Context<Self>) {
        self.integrations_dirty |= integrations;
        self.save_pending = true;
        // Edited, imported or restored shortcuts take effect at once.
        crate::shortcuts::sync(&self.settings.shortcuts, cx);
        let executor = cx.background_executor().clone();
        self.save_task = Some(cx.spawn(async move |this, cx| {
            executor.timer(SAVE_DEBOUNCE).await;
            this.update(cx, |this, cx| {
                this.flush();
                if std::mem::take(&mut this.integrations_dirty) {
                    cx.emit(SettingsEvent::Integrations);
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Writes the file now if a save is pending.
    pub fn flush(&mut self) {
        if !std::mem::take(&mut self.save_pending) {
            return;
        }
        let Some(path) = &self.path else { return };
        match self.settings.save(path) {
            Ok(()) => {
                self.file_error = None;
                // Edits belong to the active profile.
                self.profiles.sync(&self.settings);
                if std::env::var_os("MUSIC_TRACE").is_some() {
                    eprintln!("music-gpui: saved {}", path.display());
                }
            }
            Err(e) => {
                eprintln!("music-gpui: saving {} failed: {e}", path.display());
                self.file_error = Some(t!("settings.file.save_failed", error = e));
            }
        }
    }

    /// Stores (`Some`) or deletes (`None`) a secret off the UI thread, then
    /// records whether the settings file has to hold it.
    pub(super) fn put_secret(
        &mut self,
        secret: Secret,
        value: Option<String>,
        cx: &mut Context<Self>,
    ) {
        match &value {
            Some(v) => self.secrets.insert(secret, v.clone()),
            None => self.secrets.remove(&secret),
        };
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let (stored, value) = off_thread(move || {
                let stored = match &value {
                    Some(v) => secrets::store(backend.as_deref(), secret, v),
                    None => match secrets::delete(backend.as_deref(), secret) {
                        Ok(()) => Stored::Keychain,
                        Err(e) => Stored::File(e),
                    },
                };
                (stored, value)
            })
            .await;
            this.update(cx, |this, cx| {
                if let Stored::File(reason) = &stored {
                    eprintln!("music-gpui: {}: keychain not used ({reason})", secret.key());
                    if value.is_some() && this.backend.is_some() {
                        this.secret_store = SecretStore::File(FileReason::Unavailable);
                    }
                }
                secrets::apply(
                    &mut this.settings.secrets.0,
                    secret,
                    value.as_deref(),
                    &stored,
                );
                this.changed(false, cx);
            })
            .ok();
        })
        .detach();
    }

    // ---- equalizer -------------------------------------------------------------

    fn set_band(&mut self, band: usize, gain: f64, cx: &mut Context<Self>) {
        let eq = &mut self.settings.equalizer;
        if (eq.gains_db[band] - gain).abs() < 1e-6 {
            return;
        }
        eq.gains_db[band] = gain;
        eq.enabled = true;
        cx.emit(SettingsEvent::Equalizer(self.equalizer()));
        self.changed(false, cx);
    }

    pub fn apply_preset(&mut self, preset: EqPreset, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.equalizer.apply_preset(preset);
        for (slider, gain) in self.eq_sliders.iter().zip(preset.gains_db()) {
            slider.update(cx, |s, cx| s.set_value(gain as f32, window, cx));
        }
        cx.emit(SettingsEvent::Equalizer(self.equalizer()));
        self.changed(false, cx);
    }

    pub fn set_eq_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.settings.equalizer.enabled = enabled;
        cx.emit(SettingsEvent::Equalizer(self.equalizer()));
        self.changed(false, cx);
    }

    // ---- accounts --------------------------------------------------------------

    /// Copies the typed API key / Discord id into the settings (saved debounced).
    fn sync_inputs(&mut self, cx: &mut Context<Self>) {
        let key = self.lastfm_key.read(cx).value().trim().to_owned();
        let id = self.discord_id.read(cx).value().trim().to_owned();
        let s = &mut self.settings;
        if key != s.lastfm.api_key || id != s.discord.client_id {
            s.lastfm.api_key = key;
            s.discord.client_id = id;
            self.changed(true, cx);
        }
    }

    /// Script hook: types into one of the account fields.
    pub fn set_field(
        &mut self,
        field: &str,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = match field {
            "lastfm-key" => &self.lastfm_key,
            "lastfm-secret" => &self.lastfm_secret,
            "lb-token" => &self.lb_token,
            "discord-id" => &self.discord_id,
            _ => {
                eprintln!("music-gpui: unknown settings field {field}");
                return;
            }
        };
        let value = value.to_owned();
        input.update(cx, |input, cx| input.set_value(value, window, cx));
        // `set_value` doesn't emit `InputEvent::Change` like typing does.
        self.sync_inputs(cx);
    }

    /// Step 1–2 of Last.fm's desktop auth: get a token, open the approval page.
    pub fn lastfm_connect(&mut self, cx: &mut Context<Self>) {
        let key = self.lastfm_key.read(cx).value().trim().to_owned();
        let secret = self.lastfm_secret.read(cx).value().trim().to_owned();
        if key.is_empty() || secret.is_empty() {
            self.lastfm = LastFmFlow::Failed(t!("settings.panel.lastfm_missing").into());
            cx.notify();
            return;
        }
        self.settings.lastfm.api_key = key.clone();
        if self.secrets.get(&Secret::LastFmShared) != Some(&secret) {
            self.put_secret(Secret::LastFmShared, Some(secret.clone()), cx);
        }
        self.changed(false, cx);
        self.lastfm = LastFmFlow::Busy(t!("settings.panel.lastfm_requesting"));
        cx.spawn(async move |this, cx| {
            let client = LastFm::new(key.clone(), secret.clone());
            let result = off_thread(move || {
                let token = client.get_token()?;
                Ok::<_, music_core::Error>((client.auth_url(&token), token))
            })
            .await;
            this.update(cx, |this, cx| {
                this.lastfm = match result {
                    Ok((url, token)) => {
                        cx.open_url(&url);
                        LastFmFlow::Waiting {
                            key,
                            secret,
                            token,
                            error: None,
                        }
                    }
                    Err(e) => LastFmFlow::Failed(e.to_string()),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Step 3: exchange the approved token for a session key.
    pub fn lastfm_finish(&mut self, cx: &mut Context<Self>) {
        let LastFmFlow::Waiting {
            key, secret, token, ..
        } = std::mem::replace(
            &mut self.lastfm,
            LastFmFlow::Busy(t!("settings.panel.lastfm_finishing")),
        )
        else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let (k, s, t) = (key.clone(), secret.clone(), token.clone());
            let result = off_thread(move || LastFm::new(k, s).get_session(&t)).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(session) => {
                        this.settings.lastfm.user = Some(session.name);
                        this.put_secret(Secret::LastFmSession, Some(session.key), cx);
                        this.lastfm = LastFmFlow::Idle;
                        this.changed(true, cx);
                    }
                    // Usually "not authorized yet": keep the token so Finish can be retried.
                    Err(e) => {
                        this.lastfm = LastFmFlow::Waiting {
                            key,
                            secret,
                            token,
                            error: Some(e.to_string()),
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn lastfm_disconnect(&mut self, cx: &mut Context<Self>) {
        self.settings.lastfm.user = None;
        self.lastfm = LastFmFlow::Idle;
        self.put_secret(Secret::LastFmSession, None, cx);
        self.changed(true, cx);
    }

    /// Forgets the API key and secret too.
    pub fn lastfm_clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.lastfm_disconnect(cx);
        self.put_secret(Secret::LastFmShared, None, cx);
        self.settings.lastfm.api_key.clear();
        for input in [self.lastfm_key.clone(), self.lastfm_secret.clone()] {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    pub fn lb_validate(&mut self, cx: &mut Context<Self>) {
        let token = self.lb_token.read(cx).value().trim().to_owned();
        if token.is_empty() {
            self.lb = Validation::Failed(t!("settings.panel.lb_missing").into());
            cx.notify();
            return;
        }
        self.lb = Validation::Busy;
        cx.spawn(async move |this, cx| {
            let t = token.clone();
            let result = off_thread(move || ListenBrainz::new(t).validate_token()).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(user) => {
                        this.settings.listenbrainz.user = Some(user);
                        this.put_secret(Secret::ListenBrainzToken, Some(token), cx);
                        this.lb = Validation::Idle;
                        this.changed(true, cx);
                    }
                    Err(e) => this.lb = Validation::Failed(e.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn lb_disconnect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.listenbrainz.user = None;
        self.lb = Validation::Idle;
        self.put_secret(Secret::ListenBrainzToken, None, cx);
        self.lb_token
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.changed(true, cx);
    }

    pub fn set_discord_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.settings.discord.enabled = enabled;
        cx.emit(SettingsEvent::Integrations);
        self.changed(false, cx);
    }

    pub fn clear_errors(&mut self, cx: &mut Context<Self>) {
        self.scrobble_errors.clear();
        cx.notify();
    }

    // ---- rendering -------------------------------------------------------------

    fn render_nav(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let (active_bg, hover_bg) = (theme.sidebar_accent, theme.list_hover);
        let (fg, muted) = (theme.foreground, theme.muted_foreground);
        let list = v_flex()
            .id("settings-nav")
            .flex_1()
            .min_h_0()
            // 18 sections don't fit a short window.
            .overflow_y_scroll()
            .pb_4()
            .px_2()
            .gap_0p5()
            .children(SECTIONS.iter().map(|section| {
                let id = section.id;
                let active = self.section == id;
                h_flex()
                    .id(SharedString::from(format!("settings-nav-{}", id.key())))
                    .flex_none()
                    .px_3()
                    .py_1p5()
                    .gap_2p5()
                    .rounded(crate::theme::radius(6.))
                    .text_sm()
                    .cursor_pointer()
                    .text_color(if active { fg } else { muted })
                    .when(active, |this| this.bg(active_bg))
                    .when(!active, |this| this.hover(move |s| s.bg(hover_bg)))
                    .child(div().flex_none().child(Icon::new(section.icon).small()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(crate::i18n::text(section.title)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.show_section(id, cx);
                        cx.emit(SettingsEvent::Section(id));
                    }))
            }));
        v_flex()
            .w(px(232.))
            .flex_none()
            .h_full()
            .border_r_1()
            .border_color(theme.border)
            .child(
                v_flex()
                    .flex_none()
                    .px_2()
                    .pt_4()
                    .pb_3()
                    .gap_3()
                    .child(
                        div()
                            .px_3()
                            .text_xl()
                            .font_weight(FontWeight::BOLD)
                            .truncate()
                            .child(t!("nav.settings")),
                    )
                    // Outside the scrolling list, so its dropdown isn't clipped.
                    .child(div().px_1().child(self.search.clone())),
            )
            .child(list)
    }
}

impl Render for SettingsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        reveal::rendering();
        let nav = self.render_nav(cx);
        let section = self.section.section();
        let body = (section.render)(self, window, cx);
        h_flex().size_full().items_start().child(nav).child(
            div()
                .id("settings-scroll")
                .flex_1()
                .min_w_0()
                .h_full()
                .relative()
                .child(
                    div()
                        .id("settings-content")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .child(
                            v_flex()
                                .max_w(px(720.))
                                .px_8()
                                .py_6()
                                .gap_5()
                                .child(
                                    div()
                                        .text_2xl()
                                        .font_weight(FontWeight::BOLD)
                                        .child(crate::i18n::text(section.title)),
                                )
                                .child(body),
                        ),
                )
                .vertical_scrollbar(&self.scroll),
        )
    }
}
