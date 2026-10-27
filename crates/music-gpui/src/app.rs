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
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, FocusHandle, Focusable as _, Global,
    Hsla, InteractiveElement as _, IntoElement, MouseButton, NavigationDirection,
    ParentElement as _, Render, ScrollHandle, Styled as _, Subscription, Task, WeakEntity, Window,
    WindowBounds, div, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    slider::SliderState,
    v_flex,
};
use music_core::{IosClientResolver, PlaybackState, Player, Track};

use crate::anim::Frame;
use crate::clock::{Engine, SmoothClock};
use crate::now_playing::{HiRes, LyricsState, NowPlaying};
use crate::pages::{self, PageContext, PageView};
use crate::queue::Queue;
use crate::router::{Route, Router};
use crate::services::{self, Services};
use crate::settings::{SectionId, SettingsEvent, SettingsPanel, WindowGeometry};
use crate::theme::{self, ThemeChoice};

pub const POLL_INTERVAL: Duration = Duration::from_millis(250);
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(400);

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

    // ---- navigation
    pub(crate) router: Router<PageView>,
    pub(crate) search_input: Entity<InputState>,
    /// Trimmed text of the search field as last handled.
    search_text: String,
    /// Replacing this cancels a pending (debounced) search navigation.
    search_debounce: Option<Task<()>>,
    /// Results of the latest search (`@play=N`, autoplay).
    pub(crate) last_results: Rc<Vec<Track>>,
    /// Play the first result of the next search (`MUSIC_AUTOOPEN` / `MUSIC_GPUI_AUTOPLAY`).
    pub(crate) autoplay: bool,
    /// ...and open the Now Playing screen once it is playing (`MUSIC_AUTOOPEN`).
    pub(crate) autoopen: bool,

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
    /// Media controls, scrobbling, Discord.
    pub(crate) services: Services,
    pub(crate) settings: Entity<SettingsPanel>,
    pub(crate) volume_slider: Entity<SliderState>,
    pub(crate) volume: f32,
    /// Restored by the mute button.
    pub(crate) unmuted_volume: f32,
    /// `MUSIC_TRACE=1`: log playback transitions (launch time).
    pub(crate) trace: Option<Instant>,

    // ---- overlays & windows
    pub(crate) np: NowPlaying,
    pub(crate) queue_open: bool,
    pub(crate) queue_scroll: ScrollHandle,
    pub(crate) mini: Option<AnyWindowHandle>,
    /// `MUSIC_TIMINGS=1`: prints renders/s and the worst frame gap once a second.
    frame_stats: Option<FrameStats>,
    /// Per video id; never evicted (small).
    pub(crate) lyrics: HashMap<String, LyricsState>,
    /// Per video id, for the last few tracks (see `now_playing::ensure_hires`).
    pub(crate) hires: HashMap<String, HiRes>,
    pub(crate) hires_order: VecDeque<String>,
    /// Theme currently applied (choice, font scale, accent).
    applied_theme: Option<(ThemeChoice, f32, Option<Hsla>)>,
    /// Last accent extracted for a playing track (dynamic theme).
    pub(crate) accent: Option<Hsla>,

    _poll: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl MusicApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let handle = AppHandle(cx.entity().downgrade());
        cx.set_global(handle);
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search songs, artists, albums"));
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
            cx.subscribe_in(&search_input, window, Self::on_search_event),
            cx.subscribe_in(&progress, window, Self::on_progress_event),
            cx.subscribe_in(&volume_slider, window, Self::on_volume_event),
            cx.subscribe_in(&settings, window, Self::on_settings_event),
            // Downloaded artwork shows up on the next frame.
            cx.observe(
                &crate::components::artwork::ArtworkCache::global(cx),
                |_, _, cx| cx.notify(),
            ),
            cx.observe_window_bounds(window, |this, window, cx| {
                this.save_window_bounds(window, cx)
            }),
        ];

        // Opening the audio device can block briefly (and fails in sandboxes), so keep
        // it off the main thread; search works regardless. Without audio, a demo
        // wall clock stands in so the Now Playing screen can still be exercised.
        let force_demo = std::env::var("MUSIC_DEMO_CLOCK").is_ok_and(|v| v == "1");
        cx.spawn_in(window, async move |this, cx| {
            let result = if force_demo {
                Err(None)
            } else {
                cx.background_spawn(async { Player::new(Arc::new(IosClientResolver)) })
                    .await
                    .map_err(Some)
            };
            this.update_in(cx, |this, window, cx| {
                let mut engine = match result {
                    Ok(player) => Engine::Audio(player),
                    Err(e) => {
                        this.audio_error = e.map(|e| format!("{e} (using a demo clock)"));
                        Engine::Demo(Box::default())
                    }
                };
                // Volume and EQ before anything can play (MUSIC_VOLUME=0.0 stays silent).
                engine.set_volume(this.volume);
                engine.set_equalizer(this.settings.read(cx).equalizer());
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
                    .update_in(cx, |this, window, cx| this.poll_player(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });

        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let np = NowPlaying::new(cx.focus_handle());
        let mut this = Self {
            focus,
            main_window: Some(window.window_handle()),
            router: Router::new(Route::Home),
            search_input,
            search_text: String::new(),
            search_debounce: None,
            last_results: Rc::default(),
            autoplay: false,
            autoopen: false,
            engine: None,
            pending_play: None,
            audio_error: None,
            playback: PlaybackState::default(),
            clock: SmoothClock::default(),
            progress,
            scrub: None,
            queue,
            services: Services::default(),
            settings,
            volume_slider,
            volume,
            unmuted_volume: if volume > 0. { volume } else { 1. },
            trace: std::env::var_os("MUSIC_TRACE").map(|_| Instant::now()),
            np,
            queue_open: false,
            queue_scroll: ScrollHandle::new(),
            mini: None,
            frame_stats: std::env::var_os("MUSIC_TIMINGS").map(|_| FrameStats::default()),
            lyrics: HashMap::new(),
            hires: HashMap::new(),
            hires_order: VecDeque::new(),
            applied_theme: None,
            accent: None,
            _poll: poll,
            _subscriptions: subscriptions,
        };
        this.apply_theme(cx);
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
            self.on_route_changed(window, cx);
        }
    }

    pub fn go_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_scroll();
        if self.router.forward() {
            self.on_route_changed(window, cx);
        }
    }

    fn save_scroll(&mut self) {
        let entry = self.router.current_mut();
        if let Some(offset) = entry.page.as_ref().and_then(|p| p.scroll_offset()) {
            entry.scroll = offset;
        }
    }

    fn on_route_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_page(window, cx);
        window.set_window_title(&format!("{} — Music", self.router.route().title()));
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
    fn ensure_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    // ---- search -----------------------------------------------------------------

    fn on_search_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let query = input.read(cx).value().trim().to_owned();
        match event {
            InputEvent::Change if query != self.search_text => {
                self.search_text = query.clone();
                let executor = cx.background_executor().clone();
                self.search_debounce = Some(cx.spawn_in(window, async move |this, cx| {
                    executor.timer(SEARCH_DEBOUNCE).await;
                    this.update_in(cx, |this, window, cx| this.show_search(query, window, cx))
                        .ok();
                }));
            }
            InputEvent::PressEnter { .. } => {
                self.search_debounce = None;
                self.search_text = query.clone();
                self.show_search(query, window, cx);
            }
            _ => {}
        }
    }

    /// Shows the results page for `query` (replacing a search page that is open).
    fn show_search(&mut self, query: String, window: &mut Window, cx: &mut Context<Self>) {
        if query.is_empty() && !matches!(self.router.route(), Route::Search { .. }) {
            return;
        }
        self.navigate(Route::Search { query }, window, cx);
    }

    /// Fills the search field and searches right away (startup env, scripts).
    pub fn search_now(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        let query = query.trim().to_owned();
        self.search_debounce = None;
        self.search_text = query.clone();
        self.search_input
            .update(cx, |input, cx| input.set_value(query.clone(), window, cx));
        self.navigate(Route::Search { query }, window, cx);
    }

    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.np.open {
            self.close_now_playing(window, cx);
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
        cx.notify();
    }

    pub fn toggle_now_playing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.np.open {
            self.close_now_playing(window, cx);
        } else {
            self.open_now_playing(window, cx);
        }
    }

    /// Esc: close Now Playing, else the queue panel, else go back.
    pub fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.np.open {
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
            SettingsEvent::Appearance => self.apply_theme(cx),
            SettingsEvent::PlaybackModes => {
                let modes = self.settings.read(cx).settings().playback.clone();
                self.queue.set_shuffle(modes.shuffle);
                self.queue.set_repeat(modes.repeat);
                cx.notify();
            }
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
    pub(crate) fn apply_integrations(&mut self, cx: &mut Context<Self>) {
        let panel = self.settings.read(cx);
        let scrobble = panel.scrobble_config();
        let discord = panel.discord_client_id();
        self.services.configure_scrobbling(scrobble);
        let status = self.services.configure_discord(discord);
        self.settings
            .update(cx, |panel, cx| panel.set_discord_status(status, cx));
    }

    /// Applies the theme from settings (and the current accent for "dynamic")
    /// when it differs from what is applied.
    pub(crate) fn apply_theme(&mut self, cx: &mut Context<Self>) {
        let appearance = self.settings.read(cx).settings().appearance.clone();
        let accent = (appearance.theme == ThemeChoice::Dynamic)
            .then_some(self.accent)
            .flatten();
        let wanted = (appearance.theme, appearance.font_scale, accent);
        if self.applied_theme != Some(wanted) {
            theme::apply(appearance.theme, appearance.font_scale, accent, cx);
            self.applied_theme = Some(wanted);
            cx.notify();
        }
    }

    /// The current track's artwork accent became known (`now_playing::ensure_hires`).
    pub(crate) fn update_accent(&mut self, cx: &mut Context<Self>) {
        let accent = self
            .current_video_id()
            .and_then(|id| match self.hires.get(id) {
                Some(HiRes::Ready { accent, .. }) => *accent,
                _ => None,
            });
        if let Some(accent) = accent {
            self.accent = Some(accent);
            self.apply_theme(cx);
        }
    }

    fn save_window_bounds(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (bounds, maximized) = match window.window_bounds() {
            WindowBounds::Windowed(b) => (b, false),
            WindowBounds::Maximized(b) | WindowBounds::Fullscreen(b) => (b, true),
        };
        let geometry = WindowGeometry {
            x: bounds.origin.x.into(),
            y: bounds.origin.y.into(),
            width: bounds.size.width.into(),
            height: bounds.size.height.into(),
            maximized,
        };
        self.settings
            .update(cx, |panel, cx| panel.set_window_geometry(geometry, cx));
    }

    // ---- rendering ----------------------------------------------------------------

    fn render_header(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
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
                    .tooltip("Back (⌘[)")
                    .disabled(!self.router.can_go_back())
                    .on_click(cx.listener(|this, _, window, cx| this.go_back(window, cx))),
            )
            .child(
                Button::new("nav-forward")
                    .ghost()
                    .small()
                    .icon(IconName::ChevronRight)
                    .tooltip("Forward (⌘])")
                    .disabled(!self.router.can_go_forward())
                    .on_click(cx.listener(|this, _, window, cx| this.go_forward(window, cx))),
            )
            .child(
                div().flex_1().max_w(px(520.)).ml_2().child(
                    Input::new(&self.search_input)
                        .prefix(IconName::Search)
                        .cleanable(true),
                ),
            )
            .child(div().flex_1())
    }
}

impl Render for MusicApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut frame = Frame::new();
        let reveal = frame.get(&self.np.reveal);
        // Once the Now Playing screen fully covers the window, skip building the
        // shell underneath, so per-frame lyric renders stay cheap.
        let covered = reveal >= 1.;
        let mut root = div()
            .id("music-app")
            .key_context("MusicApp")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Back),
                cx.listener(|this, _, window, cx| this.go_back(window, cx)),
            )
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Forward),
                cx.listener(|this, _, window, cx| this.go_forward(window, cx)),
            );
        if !covered {
            self.ensure_page(window, cx);
            let page = self.router.current().page.as_ref().map(|p| p.view.clone());
            let sidebar = self.render_sidebar(cx);
            let header = self.render_header(cx);
            let queue_panel = self.queue_open.then(|| self.render_queue_panel(cx));
            let bar = self.render_player_bar(cx);
            root = root.child(
                v_flex()
                    .size_full()
                    .child(
                        h_flex()
                            .flex_1()
                            .min_h_0()
                            .items_start()
                            .child(sidebar)
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .h_full()
                                    .child(header)
                                    .child(div().flex_1().min_h_0().children(page)),
                            )
                            .children(queue_panel),
                    )
                    .child(bar),
            );
        }
        if reveal > 0. {
            root = root.child(self.render_now_playing_screen(reveal, &mut frame, window, cx));
        }
        if let Some(stats) = &mut self.frame_stats {
            stats.record(frame.now);
        }
        frame.finish(window);
        root
    }
}

impl gpui::Focusable for MusicApp {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

#[derive(Default)]
struct FrameStats {
    window_start: Option<Instant>,
    last: Option<Instant>,
    frames: u32,
    worst_gap: Duration,
}

impl FrameStats {
    fn record(&mut self, now: Instant) {
        if let Some(last) = self.last {
            self.worst_gap = self.worst_gap.max(now - last);
        }
        self.last = Some(now);
        self.frames += 1;
        let start = *self.window_start.get_or_insert(now);
        if now - start >= Duration::from_secs(1) {
            eprintln!(
                "music-gpui: {} renders/s, worst gap {:.1} ms",
                self.frames,
                self.worst_gap.as_secs_f64() * 1e3
            );
            *self = Self {
                last: Some(now),
                ..Self::default()
            };
        }
    }
}

/// Lets `WindowExt` notifications be pushed from app methods.
pub(crate) fn notify(window: &mut Window, cx: &mut App, message: impl Into<gpui::SharedString>) {
    window.push_notification(message.into().to_string(), cx);
}
