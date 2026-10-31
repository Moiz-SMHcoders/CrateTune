//! The root view (`MusicApp`): sidebar + header + routed page + optional queue
//! panel, the player bar, and the Now Playing overlay. It owns playback state
//! (engine, queue, services — see `playback.rs`), the router and the settings
//! entity.
//!
//! Blocking `music-core` calls run off the UI thread; a foreground task polls
//! `Player::state()` every 250 ms, feeds the OS media controls / scrobbler /
//! Discord (`services.rs`) and only re-renders when the snapshot changed. The
//! Now Playing screen (`now_playing.rs`) renders at display rate while it is
//! visible and something moves (see `anim::Frame`).
//!
//! Pages and components talk to the app through [`update_app`] / [`read_app`]
//! (a global weak handle), keyboard shortcuts and menus through `actions.rs`.

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, FocusHandle, Focusable as _, Global,
    InteractiveElement as _, IntoElement, MouseButton, NavigationDirection, ParentElement as _,
    Render, ScrollHandle, Styled as _, Subscription, Task, WeakEntity, Window, WindowBounds, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::InputState,
    menu::DropdownMenu as _,
    slider::SliderState,
    v_flex,
};
use music_core::{InnerTube, PlaybackState, Player, Track};

use crate::anim::{Frame, RenderRate};
use crate::browse::ContentFilter;
use crate::clock::{Engine, SmoothClock};
use crate::lyrics::LyricsHub;
use crate::now_playing::{HiRes, NowPlaying};
use crate::pages::{self, PageContext, PageView};
use crate::queue::Queue;
use crate::radio::RadioState;
use crate::router::{Route, Router};
use crate::search_box::SearchBox;
use crate::services::{self, Services};
use crate::settings::{SectionId, SettingsEvent, SettingsPanel, WindowGeometry};

pub const POLL_INTERVAL: Duration = Duration::from_millis(250);

struct AppHandle(WeakEntity<MusicApp>);
impl Global for AppHandle {}

/// Runs `f` on the app (from a page, component or menu handler). `None` if the
/// app is gone. Must not be called while the app itself is being updated.
pub fn update_app<R>(
    cx: &mut App,
    f: impl FnOnce(&mut MusicApp, &mut Context<MusicApp>) -> R,
) -> Option<R> {
    let app = cx.try_global::<AppHandle>()?.0.upgrade()?;
    Some(app.update(cx, f))
}

pub fn app_entity(cx: &App) -> Option<Entity<MusicApp>> {
    cx.try_global::<AppHandle>()?.0.upgrade()
}

/// Reads app state (current track, queue, …) from a page's render.
pub fn read_app<R>(cx: &App, f: impl FnOnce(&MusicApp, &App) -> R) -> Option<R> {
    let app = cx.try_global::<AppHandle>()?.0.upgrade()?;
    Some(f(app.read(cx), cx))
}

pub struct MusicApp {
    pub(crate) focus: FocusHandle,
    pub(crate) main_window: Option<AnyWindowHandle>,
    /// Wheel notches glide (Windows, Linux; `smooth_scroll.rs`).
    wheel: crate::smooth_scroll::Wheel,

    // ---- navigation
    pub(crate) router: Router<PageView>,
    pub(crate) search_input: Entity<InputState>,
    /// Trimmed text of the search field as last handled.
    pub(crate) search_text: String,
    /// Suggestions / recent-searches dropdown (`search_box.rs`).
    pub(crate) search_box: SearchBox,
    /// Results of the latest search (`@play=N`, autoplay).
    pub(crate) last_results: Rc<Vec<Track>>,
    /// Play the first result of the next search (`MUSIC_AUTOOPEN` / `MUSIC_GPUI_AUTOPLAY`).
    pub(crate) autoplay: bool,
    /// ...and open the Now Playing screen once it is playing (`MUSIC_AUTOOPEN`).
    pub(crate) autoopen: bool,
    /// YouTube Music client with the content language / country from settings
    /// (`browse.rs`); pages clone it for their requests.
    pub(crate) innertube: InnerTube,
    /// Hide explicit / hide videos (Content & region settings).
    pub(crate) content_filter: ContentFilter,
    /// Append radio songs when the queue's last song plays (`radio.rs`).
    pub(crate) autoplay_similar: bool,
    pub(crate) radio: RadioState,

    // ---- playback (playback.rs)
    /// Real audio, or the demo wall clock (no audio device / `MUSIC_DEMO_CLOCK=1`).
    pub(crate) engine: Option<Engine>,
    /// A track picked before the engine finished starting.
    pub(crate) pending_play: Option<Track>,
    pub(crate) audio_error: Option<String>,
    pub(crate) playback: PlaybackState,
    /// `playback.position` extrapolated between polls, for display-rate lyrics.
    pub(crate) clock: SmoothClock,
    pub(crate) progress: Entity<SliderState>,
    /// Seconds under the pointer while the user drags the progress slider.
    pub(crate) scrub: Option<f32>,
    pub(crate) queue: Queue,
    /// Saved queues side by side; the playing one is `queue` (`queue_tabs.rs`).
    pub(crate) queue_tabs: crate::queue_tabs::QueueTabs,
    /// Media controls, scrobbling, Discord.
    pub(crate) services: Services,
    pub(crate) settings: Entity<SettingsPanel>,
    pub(crate) volume_slider: Entity<SliderState>,
    pub(crate) volume: f32,
    /// Restored by the mute button.
    pub(crate) unmuted_volume: f32,
    /// `MUSIC_TRACE=1`: log playback transitions (launch time).
    pub(crate) trace: Option<Instant>,
    /// Video id last given to the engine's `set_next` (`None`: must be sent again).
    pub(crate) next_sent: Option<Option<String>>,
    /// `PlaybackState::advance_count` already handled (gapless switches, auto-skips).
    pub(crate) seen_advance: u64,
    /// Title of the last track that failed to load and when (shown in the bar).
    pub(crate) last_failure: Option<(String, Instant)>,

    // ---- overlays & windows
    pub(crate) np: NowPlaying,
    /// Who put the window in full screen (`np_fullscreen.rs`).
    pub(crate) fullscreen: crate::np_fullscreen::FullScreen,
    /// Re-checks a full-screen transition in flight.
    pub(crate) fullscreen_task: Option<Task<()>>,
    pub(crate) queue_open: bool,
    pub(crate) queue_scroll: ScrollHandle,
    pub(crate) mini: Option<AnyWindowHandle>,
    /// Menu-bar item (macOS; `MUSIC_TRAY=0` disables it).
    pub(crate) tray: Option<crate::tray::Tray>,
    /// `MUSIC_TIMINGS=1`: renders/s and frame intervals once a second.
    render_rate: Option<RenderRate>,
    /// Lyrics service, per-track lyrics and translations (`lyrics.rs`).
    pub(crate) lyrics: LyricsHub,
    /// Per video id, for the last few tracks (see `now_playing::ensure_hires`).
    pub(crate) hires: HashMap<String, HiRes>,
    pub(crate) hires_order: VecDeque<String>,
    /// Applied theme, the current artwork colours, the colour transition
    /// (`theme_state.rs`).
    pub(crate) theme_state: crate::theme_state::ThemeState,
    /// App-wide artwork look and Motion & effects (`look_state.rs`).
    pub(crate) look: crate::look_state::LookState,
    /// Library database, downloads, account (`store.rs`).
    pub(crate) store: crate::store::Store,
    /// SponsorBlock, Music Together, playlist import (`integrations/`).
    pub(crate) integrations: crate::integrations::Integrations,
    /// First-run guide overlay (`settings/onboarding.rs`).
    pub(crate) onboarding: Option<Entity<crate::settings::onboarding::Onboarding>>,
    /// The player bar view (`components/player_bar.rs`).
    pub(crate) bar: Option<Entity<crate::components::player_bar::PlayerBar>>,
    /// Everything above the bar as one region: cached while the bar animates.
    shell: Option<Entity<crate::components::player_bar::AppRegion>>,
    /// Last time a visualizer asked for the live spectrum (the tap is turned
    /// off by the poll a while after nobody does).
    pub(crate) spectrum_wanted: std::cell::Cell<Option<Instant>>,
    /// The current track's waveform as the player last reported it (polled;
    /// only computed while the Waveform progress style is selected).
    pub(crate) waveform: music_core::WaveformStatus,
    /// What `Player::set_waveform_enabled` was last told.
    waveform_sent: Option<bool>,
    /// Now Playing's spectrum strip (`components/np_visualizer.rs`).
    pub(crate) np_viz: Option<Entity<crate::components::np_visualizer::NpVisualizer>>,
    /// The custom top bar's cached region (`titlebar.rs`).
    titlebar: Option<Entity<crate::np_region::Region>>,
    /// Where the bar's parts were laid out (`@titlebar-status`).
    pub(crate) titlebar_layout: crate::titlebar::Layout,
    /// The left button went down on the empty bar: the next move drags the window.
    pub(crate) titlebar_drag: bool,
    /// `@titlebar-account=NAME`: a fake signed-in account shown in the bar
    /// (screenshots; nothing else sees it).
    pub(crate) titlebar_fake_account: Option<String>,

    _poll: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl MusicApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let handle = AppHandle(cx.entity().downgrade());
        cx.set_global(handle);
        let search_input = cx.new(|cx| InputState::new(window, cx));
        crate::i18n::track_placeholder(&search_input, k!("app.search_placeholder"), window, cx);
        let progress = cx.new(|_| SliderState::new().min(0.).max(1.).step(0.25));
        let settings = cx.new(|cx| SettingsPanel::new(window, cx));
        let saved = settings.read(cx).settings().clone();
        // `MUSIC_VOLUME` (e.g. 0.0 for muted real-device tests) is for this session only.
        let volume = std::env::var("MUSIC_VOLUME")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| v.is_finite())
            .map(|v| v.clamp(0., 1.))
            .unwrap_or(saved.volume);
        let volume_slider = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max(1.)
                .step(0.01)
                .default_value(volume)
        });
        let mut queue = Queue::default();
        queue.set_shuffle(saved.playback.shuffle);
        queue.set_repeat(saved.playback.repeat);

        let subscriptions = vec![
            cx.subscribe_in(&search_input, window, Self::on_search_input),
            cx.subscribe_in(&progress, window, Self::on_progress_event),
            cx.subscribe_in(&volume_slider, window, Self::on_volume_event),
            cx.subscribe_in(&settings, window, Self::on_settings_event),
            // Downloaded artwork shows up on the next frame.
            cx.observe(
                &crate::components::artwork::ArtworkCache::global(cx),
                |_, _, cx| cx.notify(),
            ),
            cx.observe_window_bounds(window, |this, window, cx| {
                this.save_window_bounds(window, cx);
                this.sync_fullscreen(window, cx);
            }),
            // The top bar's controls dim in an inactive window.
            cx.observe_window_activation(window, |_, _, cx| cx.notify()),
            cx.on_app_quit(|this, cx| {
                // A rotated YouTube cookie not saved yet (throttled) goes in now.
                this.persist_session_cookie(true, cx);
                this.flush_listening();
                this.save_queue_now();
                this.flush_searches();
                // "Install updates automatically": a verified download waiting for a
                // restart goes in now (`update_check.rs`).
                this.install_update_on_quit(cx);
                async {}
            }),
        ];

        // Opening the audio device can block briefly (and fails in sandboxes), so keep
        // it off the main thread; search works regardless. Without audio, a demo
        // wall clock stands in so the Now Playing screen can still be exercised.
        let force_demo = std::env::var("MUSIC_DEMO_CLOCK").is_ok_and(|v| v == "1");
        // Provider modules come first (when enabled), YouTube after them.
        let module_service = crate::module_service::ModuleService::open(
            saved.audio.stream_source,
            saved.audio.player_fixes,
        );
        let resolver = module_service.resolver.clone();
        cx.set_global(module_service);
        crate::module_service::ModuleService::prewarm(cx);
        let downloads_resolver = resolver.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = if force_demo {
                Err(None)
            } else {
                cx.background_spawn(async move { Player::new(resolver) })
                    .await
                    .map_err(Some)
            };
            this.update_in(cx, |this, window, cx| {
                let mut engine = match result {
                    Ok(player) => Engine::Audio(player),
                    Err(e) => {
                        this.audio_error = e.map(|e| t!("app.audio_demo_clock", error = e));
                        Engine::Demo(Box::default())
                    }
                };
                // Volume and EQ before anything can play (MUSIC_VOLUME=0.0 stays silent).
                engine.set_volume(this.volume);
                if let Engine::Audio(player) = &engine {
                    // Finished waveforms (a few hundred bytes per track).
                    player.set_waveform_cache(
                        crate::settings::file::config_dir()
                            .map(|d| d.join("cache").join("waveforms")),
                    );
                }
                engine.set_equalizer(this.settings.read(cx).equalizer());
                engine.apply_audio(&this.settings.read(cx).settings().audio);
                // Main thread, window exists: what the media-controls docs require.
                this.services
                    .start_media_controls(&engine, services::hwnd(window));
                this.engine = Some(engine);
                this.apply_integrations(cx);
                if let Some(track) = this.pending_play.take() {
                    this.play(track, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();

        let executor = cx.background_executor().clone();
        let poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                executor.timer(POLL_INTERVAL).await;
                if this
                    .update_in(cx, |this, window, cx| {
                        this.poll_player(window, cx);
                        this.poll_log_viewer(cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        if std::env::var_os("MUSIC_TRACE").is_some() {
            eprintln!(
                "music-gpui: window bounds {:?}, displays {:?}",
                window.window_bounds(),
                cx.displays().iter().map(|d| d.bounds()).collect::<Vec<_>>()
            );
        }
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let np = NowPlaying::new(cx.focus_handle());
        let mut this = Self {
            focus,
            main_window: Some(window.window_handle()),
            wheel: Default::default(),
            router: Router::new(Route::Home),
            search_input,
            search_text: String::new(),
            search_box: SearchBox::default(),
            last_results: Rc::default(),
            autoplay: false,
            autoopen: false,
            innertube: crate::browse::innertube_for(&saved.content),
            content_filter: ContentFilter::from_settings(&saved.content),
            autoplay_similar: saved.content.autoplay_similar,
            radio: RadioState::default(),
            engine: None,
            pending_play: None,
            audio_error: None,
            playback: PlaybackState::default(),
            clock: SmoothClock::default(),
            progress,
            scrub: None,
            queue,
            queue_tabs: Default::default(),
            services: Services::default(),
            settings,
            volume_slider,
            volume,
            unmuted_volume: if volume > 0. { volume } else { 1. },
            trace: std::env::var_os("MUSIC_TRACE").map(|_| Instant::now()),
            next_sent: None,
            seen_advance: 0,
            last_failure: None,
            np,
            fullscreen: crate::np_fullscreen::FullScreen::new(window.is_fullscreen()),
            fullscreen_task: None,
            queue_open: false,
            queue_scroll: ScrollHandle::new(),
            mini: None,
            tray: std::env::var("MUSIC_TRAY")
                .map_or(true, |v| v != "0")
                .then(crate::tray::Tray::new)
                .flatten(),
            render_rate: RenderRate::from_env("app"),
            lyrics: LyricsHub::default(),
            hires: HashMap::new(),
            hires_order: VecDeque::new(),
            theme_state: Default::default(),
            look: Default::default(),
            store: crate::store::Store::open(saved.library.clone(), downloads_resolver),
            integrations: Default::default(),
            onboarding: None,
            bar: None,
            shell: None,
            spectrum_wanted: std::cell::Cell::new(None),
            waveform: Default::default(),
            waveform_sent: None,
            np_viz: None,
            titlebar: None,
            titlebar_layout: Default::default(),
            titlebar_drag: false,
            titlebar_fake_account: None,
            _poll: poll,
            _subscriptions: subscriptions,
        };
        crate::effects::poll_os_reduce_motion();
        this.apply_theme(cx);
        this.apply_artwork_budget(cx);
        this.sync_browse_client(cx);
        this.start_radio_watch(window, cx);
        this.start_session_keeper(cx);
        this.start_youtube_sync_watch(window, cx);
        this.rebuild_lyrics_service(cx);
        this.use_library_lyrics_cache(cx);
        this.restore_queue();
        this.load_queue_tabs();
        this.load_recent_searches(cx);
        this.queue.set_shuffle(saved.playback.shuffle);
        this.ensure_page(window, cx);
        this
    }

    /// `MUSIC_QUERY` pre-fills and runs a search; `MUSIC_AUTOOPEN=1` then plays the first
    /// result and opens Now Playing. Lets RSS/CPU be measured without clicking around.
    /// (`MUSIC_GPUI_QUERY` / `MUSIC_GPUI_AUTOPLAY` from the first round still work.)
    ///
    /// `MUSIC_OPEN_SETTINGS=1|<section>` opens the settings page, `MUSIC_ROUTE=<route>`
    /// any page (see `Route::parse`), `MUSIC_EQ_PRESET=Bass` applies (and saves) a preset.
    pub fn apply_startup_env(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        self.maybe_open_onboarding(window, cx);
        self.start_update_checks(cx);
        if let Some(name) = env("MUSIC_EQ_PRESET") {
            self.script_action("eq", Some(&name), window, cx);
        }
        if let Some(route) = env("MUSIC_ROUTE").and_then(|r| Route::parse(&r)) {
            self.navigate(route, window, cx);
        }
        if let Some(tab) = env("MUSIC_OPEN_SETTINGS").filter(|v| v != "0") {
            self.script_action("settings", Some(&tab), window, cx);
        }
        let Some(query) = env("MUSIC_QUERY").or_else(|| env("MUSIC_GPUI_QUERY")) else {
            return;
        };
        self.autoopen = env("MUSIC_AUTOOPEN").is_some_and(|v| v != "0");
        self.autoplay = self.autoopen || env("MUSIC_GPUI_AUTOPLAY").is_some();
        self.search_now(query.trim(), window, cx);
    }

    // ---- shared state ---------------------------------------------------------

    pub fn current_track(&self) -> Option<&Track> {
        self.playback.track.as_ref()
    }

    pub fn current_video_id(&self) -> Option<&str> {
        self.current_track().map(|t| t.video_id.as_str())
    }

    // ---- navigation -----------------------------------------------------------

    /// Opens `route` as a new history entry. Search, settings sections and library
    /// tabs replace the current entry when it shows the same page.
    pub fn navigate(&mut self, route: Route, window: &mut Window, cx: &mut Context<Self>) {
        if self.np.open {
            self.close_now_playing(window, cx);
        }
        let replace = matches!(
            route,
            Route::Search { .. } | Route::Settings { .. } | Route::Library { .. }
        ) && route.same_page(self.router.route());
        if replace {
            self.navigate_replace(route, window, cx);
            return;
        }
        self.save_scroll();
        if self.router.push(route) {
            self.start_page_transition();
            self.on_route_changed(window, cx);
        }
    }

    /// Swaps the current entry (no new history entry).
    pub fn navigate_replace(&mut self, route: Route, window: &mut Window, cx: &mut Context<Self>) {
        // Settings sections share one page entity: just switch the section.
        if let (Route::Settings { section }, Route::Settings { .. }) = (&route, self.router.route())
        {
            let section = *section;
            self.router.replace(route);
            self.settings
                .update(cx, |panel, cx| panel.show_section(section, cx));
        } else if !self.router.replace(route) {
            return;
        }
        self.on_route_changed(window, cx);
    }

    pub fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_scroll();
        if self.router.back() {
            self.start_page_transition();
            self.on_route_changed(window, cx);
        }
    }

    pub fn go_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_scroll();
        if self.router.forward() {
            self.start_page_transition();
            self.on_route_changed(window, cx);
        }
    }

    pub(crate) fn save_scroll(&mut self) {
        let entry = self.router.current_mut();
        if let Some(offset) = entry.page.as_ref().and_then(|p| p.scroll_offset()) {
            entry.scroll = offset;
        }
    }

    /// Switches the UI to the language in the settings, live: menus are rebuilt,
    /// tracked input placeholders reset, every window re-rendered (cached views too)
    /// and the menu-bar item relabelled on the next poll.
    pub fn apply_language(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let locale = self.settings.read(cx).settings().appearance.ui_locale();
        if locale == crate::i18n::locale() {
            return;
        }
        crate::i18n::set_locale(locale);
        crate::actions::set_menus(cx);
        crate::i18n::relocalize(window, cx);
        self.update_window_title(window);
        cx.refresh_windows();
        cx.notify();
    }

    fn update_window_title(&self, window: &mut Window) {
        let title = self.router.route().title();
        window.set_window_title(&t!("app.window_title", page = title));
    }

    fn on_route_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A song selection belongs to the page it was made on.
        crate::selection::clear(window);
        self.ensure_page(window, cx);
        let title = self.router.route().title();
        if let Some(started) = self.trace {
            eprintln!(
                "music-gpui: [{:6.2}s] route {title}",
                started.elapsed().as_secs_f64()
            );
        }
        self.update_window_title(window);
        match self.router.route().clone() {
            // Keep the search field in sync when going back/forward between searches.
            Route::Search { query } if query != self.search_text => {
                self.search_text = query.clone();
                self.search_input
                    .update(cx, |input, cx| input.set_value(query, window, cx));
            }
            Route::Settings { section } => {
                self.settings
                    .update(cx, |panel, cx| panel.show_section(section, cx));
            }
            _ => {}
        }
        cx.notify();
    }

    /// Builds the current entry's page if it has none (first visit, or dropped
    /// from the live set) and re-applies its saved scroll offset.
    pub(crate) fn ensure_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.router.current().page.is_some() {
            return;
        }
        let route = self.router.route().clone();
        let settings = self.settings.clone();
        let page = pages::build(
            &route,
            PageContext {
                settings: &settings,
            },
            window,
            cx,
        );
        let entry = self.router.current_mut();
        page.restore_scroll(entry.scroll);
        entry.page = Some(page);
    }

    pub fn open_settings(
        &mut self,
        section: Option<SectionId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let section = section.unwrap_or_else(|| match self.router.route() {
            Route::Settings { section } => *section,
            _ => self.settings.read(cx).section,
        });
        self.navigate(Route::Settings { section }, window, cx);
    }

    /// Opens settings `section` and reveals the row with search-index anchor `anchor`
    /// (scrolled into view, flashed, focused), as a settings search pick does.
    pub fn open_settings_at(
        &mut self,
        section: SectionId,
        anchor: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_settings(Some(section), window, cx);
        self.settings
            .update(cx, |panel, cx| panel.reveal_anchor(anchor, window, cx));
    }

    // ---- search -----------------------------------------------------------------

    /// Fills the search field and searches right away (startup env, scripts).
    pub fn search_now(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        let query = query.trim().to_owned();
        self.search_box_reset();
        self.search_text = query.clone();
        self.search_input
            .update(cx, |input, cx| input.set_value(query.clone(), window, cx));
        self.navigate(Route::Search { query }, window, cx);
    }

    /// ⌘F / Ctrl+F: the music search; on the settings page the settings search
    /// first (pressed again there, the music search).
    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.np.open {
            self.close_now_playing(window, cx);
        }
        if matches!(self.router.route(), Route::Settings { .. })
            && self.onboarding.is_none()
            && !self.settings.read(cx).search_focused(window, cx)
        {
            self.settings
                .update(cx, |panel, cx| panel.focus_search(window, cx));
            return;
        }
        let handle = self.search_input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        self.search_input
            .update(cx, |input, cx| input.select_all(window, cx));
        cx.notify();
    }

    /// Called by the search page when results arrive.
    pub fn on_search_results(&mut self, tracks: Rc<Vec<Track>>, cx: &mut Context<Self>) {
        self.last_results = tracks;
        if std::mem::take(&mut self.autoplay) {
            self.play_list(self.last_results.clone(), 0, cx);
        }
    }

    // ---- overlays -------------------------------------------------------------

    pub fn toggle_queue_panel(&mut self, cx: &mut Context<Self>) {
        self.queue_open = !self.queue_open;
        // The panel opens on the playing tab.
        self.queue_tabs.viewed = self.queue_tabs.playing;
        cx.notify();
    }

    pub fn toggle_now_playing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.np.open {
            self.close_now_playing(window, cx);
        } else {
            self.open_now_playing(window, cx);
        }
    }

    /// ⌘A / Ctrl+A outside text fields: every song of the list with a selection, else
    /// of the list under the pointer (`selection.rs`).
    pub fn select_all_rows(&mut self, window: &mut Window, _: &mut Context<Self>) {
        if self.np.open {
            return;
        }
        crate::selection::update(window, |s| s.select_all());
    }

    /// Esc (menus and popovers handle it first): close a dialog, else drop a song
    /// selection, else end a lyrics
    /// card selection, else leave the full screen entered from Now Playing's button
    /// (`np_fullscreen.rs`), else Now Playing, else the queue panel, else go back.
    pub fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            window.close_dialog(cx);
        } else if !self.np.open && crate::selection::clear(window) {
            // Dropped a song selection (`selection.rs`).
        } else if self.onboarding.is_some() {
            self.close_onboarding(cx);
        } else if self.np.open && self.np.list.selecting {
            // Picking lines for a lyrics card.
            self.set_lyrics_selecting(false, cx);
        } else if self.np.open && self.fullscreen_escape(window, cx) {
            // Left the full screen the Now Playing button entered; Esc again closes.
        } else if self.np.open {
            self.close_now_playing(window, cx);
        } else if self.queue_open {
            self.toggle_queue_panel(cx);
        } else {
            self.go_back(window, cx);
        }
    }

    // ---- settings & theme -------------------------------------------------------

    fn on_settings_event(
        &mut self,
        _: &Entity<SettingsPanel>,
        event: &SettingsEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SettingsEvent::Equalizer(eq) => {
                if let Some(engine) = &mut self.engine {
                    engine.set_equalizer(eq.clone());
                }
            }
            SettingsEvent::Integrations => self.apply_integrations(cx),
            SettingsEvent::Audio => {
                let audio = self.settings.read(cx).settings().audio.clone();
                crate::module_service::ModuleService::set_stream_source(cx, audio.stream_source);
                crate::module_service::ModuleService::set_player_fixes(cx, audio.player_fixes);
                if let Some(engine) = &mut self.engine {
                    engine.apply_audio(&audio);
                }
                self.refresh_soon(cx);
                cx.notify();
            }
            SettingsEvent::Quality => self.switch_stream_quality(cx),
            SettingsEvent::Lyrics => self.rebuild_lyrics_service(cx),
            SettingsEvent::NowPlaying => {
                // The bar's background effect may need the artwork colours now.
                self.ensure_art_colors(cx);
                cx.notify();
            }
            SettingsEvent::Appearance => {
                self.apply_theme(cx);
                self.apply_artwork_budget(cx);
                // The window background and app icon follow on the next render.
                cx.notify();
            }
            SettingsEvent::Language => self.apply_language(window, cx),
            SettingsEvent::PlaybackModes => {
                let modes = self.settings.read(cx).settings().playback.clone();
                self.queue.set_shuffle(modes.shuffle);
                self.queue.set_repeat(modes.repeat);
                cx.notify();
            }
            SettingsEvent::Library => self.apply_library_settings(cx),
            SettingsEvent::Content => self.apply_content(window, cx),
            SettingsEvent::Network => self.refresh_pages(window, cx),
            SettingsEvent::Section(section) => {
                let route = Route::Settings { section: *section };
                if matches!(self.router.route(), Route::Settings { .. }) {
                    self.router.replace(route);
                    // The page entity is shared; keep it on the (replaced) entry.
                    self.ensure_page(window, cx);
                    cx.notify();
                }
            }
        }
    }

    /// (Re)builds the scrobbler and Discord presence from the current settings.
    /// Settings → Storage → "Artwork in memory" (part of the appearance profile).
    pub(crate) fn apply_artwork_budget(&mut self, cx: &mut Context<Self>) {
        let mb = self
            .settings
            .read(cx)
            .settings()
            .appearance
            .artwork_memory_mb;
        crate::components::artwork::ArtworkCache::global(cx)
            .update(cx, |cache, cx| cache.set_budget_mb(mb, cx));
    }

    pub(crate) fn apply_integrations(&mut self, cx: &mut Context<Self>) {
        let panel = self.settings.read(cx);
        let scrobble = panel.scrobble_config();
        let rules = panel.settings().scrobble_rules.to_core();
        let discord = panel.discord_config();
        self.services.configure_scrobbling(scrobble, rules);
        let status = self.services.configure_discord(discord);
        self.settings
            .update(cx, |panel, cx| panel.set_discord_status(status, cx));
    }

    fn save_window_bounds(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (bounds, maximized) = match window.window_bounds() {
            WindowBounds::Windowed(b) => (b, false),
            WindowBounds::Maximized(b) => (b, true),
            // Keep the windowed geometry: the next launch opens a normal window.
            WindowBounds::Fullscreen(_) => return,
        };
        // `open_window` takes the content size; `window_bounds` reports the frame
        // (with the title bar), so store the viewport size to avoid drift.
        let content = window.viewport_size();
        let geometry = WindowGeometry {
            x: bounds.origin.x.into(),
            y: bounds.origin.y.into(),
            width: if maximized {
                bounds.size.width
            } else {
                content.width
            }
            .into(),
            height: if maximized {
                bounds.size.height
            } else {
                content.height
            }
            .into(),
            maximized,
        };
        self.settings
            .update(cx, |panel, cx| panel.set_window_geometry(geometry, cx));
    }

    // ---- rendering ----------------------------------------------------------------

    fn render_header(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let search = self.render_search_box(cx);
        let theme = cx.theme();
        h_flex()
            .h(px(56.))
            .flex_none()
            .px_4()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("nav-back")
                    .ghost()
                    .small()
                    .icon(IconName::ChevronLeft)
                    .tooltip(t!("app.tooltip.back"))
                    .disabled(!self.router.can_go_back())
                    .on_click(cx.listener(|this, _, window, cx| this.go_back(window, cx))),
            )
            .child(
                Button::new("nav-forward")
                    .ghost()
                    .small()
                    .icon(IconName::ChevronRight)
                    .tooltip(t!("app.tooltip.forward"))
                    .disabled(!self.router.can_go_forward())
                    .on_click(cx.listener(|this, _, window, cx| this.go_forward(window, cx))),
            )
            .child(search)
            .child(div().flex_1())
            // No menu bar outside macOS: Settings, shortcuts, updates, About and Quit
            // live here.
            .when(!cfg!(target_os = "macos"), |this| {
                this.child(
                    Button::new("app-menu")
                        .ghost()
                        .small()
                        .icon(IconName::Ellipsis)
                        .tooltip(t!("track.tooltip.more"))
                        .dropdown_menu_with_anchor(gpui::Anchor::TopRight, |menu, _, _| {
                            crate::actions::app_menu(menu)
                        }),
                )
            })
    }
}

impl MusicApp {
    /// Sidebar | header + page | queue panel: everything above the player bar.
    fn render_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        self.ensure_page(window, cx);
        let page = self.router.current().page.as_ref().map(|p| p.view.clone());
        let sidebar = self.render_sidebar(cx);
        // The custom top bar has back / forward / search; the old header only
        // comes with the system title bar.
        let header = (!crate::titlebar::custom()).then(|| self.render_header(cx));
        let queue_panel = self.queue_open.then(|| self.render_queue_panel(cx));
        // A new page fades in (and slides up a little with full motion): ~180 ms
        // of frames once per navigation, then static.
        let mut frame = Frame::new();
        let t = frame.get(&self.look.page_in);
        let page_box = div().flex_1().min_h_0().children(page);
        let page_box = if t < 1. {
            let slide = if crate::effects::ambient_motion() {
                crate::look_state::PAGE_SLIDE * (1. - t)
            } else {
                0.
            };
            page_box.relative().top(px(slide)).opacity(t.max(0.))
        } else {
            page_box
        };
        if frame.finish(window).is_some() {
            // (Tweens only ask for frames, never timed wakes.)
        }
        h_flex()
            .size_full()
            .items_start()
            .child(sidebar)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .children(header)
                    .child(page_box),
            )
            .children(queue_panel)
            .into_any_element()
    }

    /// The audio engine's player (not the demo clock; not while casting).
    pub(crate) fn audio_player(&self) -> Option<&Player> {
        match &self.engine {
            Some(Engine::Audio(player)) => Some(player),
            _ => None,
        }
    }

    /// Settings → About shows the log: redraw it when lines came in (only then).
    fn poll_log_viewer(&mut self, cx: &mut Context<Self>) {
        if !matches!(
            self.router.route(),
            Route::Settings {
                section: SectionId::About
            }
        ) || self.np.open
        {
            return;
        }
        let generation = crate::logs::generation();
        if self.settings.read(cx).log_seen != generation {
            self.settings.update(cx, |panel, cx| {
                panel.log_seen = generation;
                cx.notify();
            });
        }
    }

    /// A visualizer is on screen: keep the spectrum tap on (see `poll_spectrum`).
    pub(crate) fn want_spectrum(&self) {
        self.spectrum_wanted.set(Some(Instant::now()));
        if let Some(player) = self.audio_player()
            && !player.spectrum_enabled()
        {
            player.set_spectrum_enabled(true);
        }
    }

    /// From the poll: tells the player whether to compute waveforms (the
    /// Waveform style is selected; the mini player follows the bar's style)
    /// and picks up the current track's. Notifies only when it changed.
    pub(crate) fn poll_waveform(&mut self, cx: &mut Context<Self>) {
        let wanted = self
            .settings
            .read(cx)
            .settings()
            .player_bar
            .wants_waveform();
        let status = match self.audio_player().cloned() {
            Some(player) => {
                if self.waveform_sent != Some(wanted) {
                    player.set_waveform_enabled(wanted);
                    self.waveform_sent = Some(wanted);
                }
                player.waveform()
            }
            None => Default::default(),
        };
        if status != self.waveform {
            self.waveform = status;
            cx.notify();
        }
    }

    /// From the poll: turns the tap off 2 s after the last visualizer stopped asking.
    pub(crate) fn poll_spectrum(&mut self) {
        let stale = self
            .spectrum_wanted
            .get()
            .is_some_and(|t| t.elapsed() > Duration::from_secs(2));
        if stale {
            self.spectrum_wanted.set(None);
            if let Some(player) = self.audio_player() {
                player.set_spectrum_enabled(false);
            }
        }
    }
}

impl Render for MusicApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut frame = Frame::new();
        let reveal = frame.get(&self.np.reveal);
        // Once the Now Playing screen fully covers the window, skip building the
        // shell underneath, so per-frame lyric renders stay cheap.
        let covered = reveal >= 1.;
        self.sync_window_background(window, cx);
        let translucent = self.translucent_sidebar(cx);
        let see_through = self.window_see_through(cx).filter(|_| !covered);
        let mut root = div()
            .id("music-app")
            .key_context("MusicApp")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            // With the translucent sidebar the window itself is see-through
            // (blurred); everything but the sidebar paints its own background.
            // A see-through window paints its background at the readable opacity.
            .when(!translucent || covered, |this| {
                this.bg(cx.theme().background.opacity(see_through.unwrap_or(1.)))
            })
            .text_color(cx.theme().foreground)
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Back),
                cx.listener(|this, _, window, cx| this.go_back(window, cx)),
            )
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Forward),
                cx.listener(|this, _, window, cx| this.go_forward(window, cx)),
            );
        // The custom top bar (`titlebar.rs`): a cached region, above Now Playing
        // except in full screen (where Now Playing covers the whole window).
        let custom = crate::titlebar::custom();
        let bar_h = px(crate::titlebar::top_inset());
        let titlebar = (custom && !(covered && window.is_fullscreen())).then(|| {
            let key = self.titlebar_key(window, cx);
            crate::np_region::sync(
                &mut self.titlebar,
                key,
                Self::render_titlebar,
                &mut frame,
                cx,
            )
            .cached(
                gpui::StyleRefinement::default()
                    .w_full()
                    .h(px(crate::titlebar::HEIGHT))
                    .flex_none(),
            )
        });
        if !covered {
            // While the bar animates (or the pointer is on one of its tracks) the
            // shell is a cached view: a bar frame then doesn't rebuild the page.
            // So is the app-wide backdrop (and then the bar too).
            let bar_live = self.bar_is_live(cx);
            let backdrop_live = self.backdrop_is_live(cx);
            let cached = bar_live || backdrop_live;
            let shell = crate::components::player_bar::region(
                &mut self.shell,
                Self::render_shell,
                cached,
                cx,
            );
            let backdrop = self.app_backdrop(cx);
            // Songs selected in a list: their actions float above the player bar.
            let selection_bar = self.render_selection_bar(cx).map(|bar| {
                v_flex()
                    .absolute()
                    .bottom_3()
                    .left_0()
                    .right_0()
                    .px_4()
                    .items_center()
                    .child(bar)
            });
            let bar = self.player_bar_view(cx);
            let bar = if backdrop_live && !bar_live {
                bar.cached(
                    gpui::StyleRefinement::default()
                        .w_full()
                        .h(px(crate::components::player_bar::BAR_HEIGHT))
                        .flex_none(),
                )
                .into_any_element()
            } else {
                bar.into_any_element()
            };
            // With the translucent sidebar the top bar is see-through like the
            // sidebar: the opaque base and the backdrop start below it.
            let under_bar = if translucent { bar_h } else { px(0.) };
            let base = translucent.then(|| {
                div()
                    .absolute()
                    .top(under_bar)
                    .bottom_0()
                    .right_0()
                    .left(px(crate::components::sidebar::SIDEBAR_WIDTH))
                    .bg(cx.theme().background)
            });
            root = root.child(
                v_flex()
                    .size_full()
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .children(base)
                            .children(backdrop.map(|b| {
                                // Behind the content area only, with the desktop blur.
                                div()
                                    .absolute()
                                    .top(under_bar)
                                    .bottom_0()
                                    .right_0()
                                    .left(px(if translucent {
                                        crate::components::sidebar::SIDEBAR_WIDTH
                                    } else {
                                        0.
                                    }))
                                    .child(b)
                            }))
                            .child(
                                v_flex()
                                    .relative()
                                    .size_full()
                                    .children(titlebar)
                                    .child(div().flex_1().min_h_0().w_full().child(shell)),
                            )
                            .children(selection_bar),
                    )
                    .child(bar),
            );
        } else if let Some(titlebar) = titlebar {
            root = root.child(titlebar);
        }
        if reveal > 0. {
            root = root.child(self.render_now_playing_screen(reveal, &mut frame, window, cx));
        }
        if let Some(onboarding) = &self.onboarding {
            // Below the top bar, so the window controls stay reachable.
            root = root.child(
                div()
                    .absolute()
                    .top(bar_h)
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .child(onboarding.clone()),
            );
        }
        if custom && let Some(edges) = self.resize_edges(window) {
            root = root.child(edges);
        }
        if let Some(rate) = &mut self.render_rate {
            rate.record(frame.now, frame.is_animating() || self.wheel.gliding());
        }
        if let Some(at) = frame.finish(window) {
            self.schedule_np_wake(at, cx);
        }
        // Last, so its capture listener runs after the wheel controls' claims.
        root.children(self.wheel.listener())
    }
}

impl gpui::Focusable for MusicApp {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Lets `WindowExt` notifications be pushed from app methods.
pub(crate) fn notify(window: &mut Window, cx: &mut App, message: impl Into<gpui::SharedString>) {
    window.push_notification(message.into().to_string(), cx);
}
