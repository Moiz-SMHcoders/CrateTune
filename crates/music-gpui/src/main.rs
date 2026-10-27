//! YouTube Music spike UI on GPUI + gpui-component.
//!
//! One root view (`MusicApp`) owns the playback state. Blocking `music-core` calls
//! run on GPUI's background executor; a foreground task polls `Player::state()`
//! every 250 ms, feeds the OS media controls / scrobbler / Discord (`services.rs`)
//! and only re-renders when the snapshot changed. The Now Playing screen
//! (`now_playing.rs`) additionally renders at display rate while it is visible
//! and something moves (see `anim::Frame`). Settings live in their own entity
//! (`settings_panel.rs`), shown in a gpui-component `Sheet`.

mod anim;
mod backdrop;
mod clock;
mod icons;
mod karaoke;
mod now_playing;
mod queue;
mod script;
mod secrets;
mod services;
mod settings;
mod settings_panel;

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    App, AppContext as _, Bounds, ClickEvent, Context, Entity, Focusable as _, FontWeight, Hsla,
    Image, ImageFormat, InteractiveElement as _, IntoElement, KeyBinding, ObjectFit,
    ParentElement as _, Pixels, Render, SharedString, Size, StatefulInteractiveElement as _,
    Styled as _, StyledImage as _, Subscription, Task, TitlebarOptions, Window, WindowBounds,
    WindowOptions, div, img, prelude::FluentBuilder as _, px, size,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconName, Root, Sizable as _, Theme, ThemeMode,
    VirtualListScrollHandle, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement as _,
    slider::{Slider, SliderEvent, SliderState},
    spinner::Spinner,
    tag::Tag,
    v_flex, v_virtual_list,
};
use music_core::media_controls::MediaControlEvent;
use music_core::{
    InnerTube, IosClientResolver, PlaybackState, PlaybackStatus, Player, Track, format_duration,
};

use crate::anim::Frame;
use crate::clock::{DemoPlayer, Engine, SmoothClock};
use crate::icons::AppIcon;
use crate::now_playing::{CloseNowPlaying, HiRes, LyricsState, NowPlaying, TogglePlayback};
use crate::queue::{Previous, Queue};
use crate::services::Services;
use crate::settings_panel::{SettingsEvent, SettingsPanel, Tab};

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(400);
const ROW_HEIGHT: f32 = 64.;
const ART_SIZE: u32 = 120;

enum Artwork {
    Loading,
    Ready(Arc<Image>),
    Failed,
}

enum SearchStatus {
    Idle,
    Searching,
    Done,
    Failed(String),
}

struct MusicApp {
    search_input: Entity<InputState>,
    query: String,
    search_status: SearchStatus,
    /// Replacing this cancels an in-flight (debounced) search.
    search_task: Option<Task<()>>,
    tracks: Rc<Vec<Track>>,
    row_sizes: Rc<Vec<Size<Pixels>>>,
    scroll: VirtualListScrollHandle,
    /// Per video id; entries are never evicted (fine for a spike).
    artwork: HashMap<String, Artwork>,

    /// Real audio, or the demo wall clock (no audio device / `MUSIC_DEMO_CLOCK=1`).
    engine: Option<Engine>,
    /// A track picked before the engine finished starting.
    pending_play: Option<Track>,
    audio_error: Option<String>,
    playback: PlaybackState,
    /// `playback.position` extrapolated between polls, for display-rate lyrics.
    clock: SmoothClock,
    progress: Entity<SliderState>,
    /// Seconds under the pointer while the user drags the progress slider.
    scrub: Option<f32>,
    /// Play the first result of the next search (`MUSIC_AUTOOPEN` / `MUSIC_GPUI_AUTOPLAY`).
    autoplay: bool,
    /// ...and open the Now Playing screen once it is playing (`MUSIC_AUTOOPEN`).
    autoopen: bool,
    /// The result list the current track was started from.
    queue: Queue,
    /// Media controls, scrobbling, Discord.
    services: Services,
    settings: Entity<SettingsPanel>,
    volume_slider: Entity<SliderState>,
    volume: f32,
    /// Restored by the mute button.
    unmuted_volume: f32,
    /// `MUSIC_TRACE=1`: log playback transitions (launch time).
    trace: Option<Instant>,

    np: NowPlaying,
    /// `MUSIC_TIMINGS=1`: prints renders/s and the worst frame gap once a second.
    frame_stats: Option<FrameStats>,
    /// Per video id; never evicted.
    lyrics: HashMap<String, LyricsState>,
    hires: HashMap<String, HiRes>,

    _poll: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl MusicApp {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search songs, artists, albums"));
        let progress = cx.new(|_| SliderState::new().min(0.).max(1.).step(0.25));
        let settings = cx.new(|cx| SettingsPanel::new(window, cx));
        // `MUSIC_VOLUME` (e.g. 0.0 for muted real-device tests) is for this session only.
        let volume = std::env::var("MUSIC_VOLUME")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| v.is_finite())
            .map(|v| v.clamp(0., 1.))
            .unwrap_or_else(|| settings.read(cx).settings().volume);
        let volume_slider = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max(1.)
                .step(0.01)
                .default_value(volume)
        });

        let subscriptions = vec![
            cx.subscribe_in(&search_input, window, Self::on_search_event),
            cx.subscribe_in(&progress, window, Self::on_progress_event),
            cx.subscribe_in(&volume_slider, window, Self::on_volume_event),
            cx.subscribe_in(&settings, window, Self::on_settings_event),
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
                        Engine::Demo(DemoPlayer::default())
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

        window.focus(&search_input.focus_handle(cx), cx);

        Self {
            search_input,
            query: String::new(),
            search_status: SearchStatus::Idle,
            search_task: None,
            tracks: Rc::default(),
            row_sizes: Rc::default(),
            scroll: VirtualListScrollHandle::new(),
            artwork: HashMap::new(),
            engine: None,
            pending_play: None,
            audio_error: None,
            playback: PlaybackState::default(),
            clock: SmoothClock::default(),
            progress,
            scrub: None,
            autoplay: false,
            autoopen: false,
            queue: Queue::default(),
            services: Services::default(),
            settings,
            volume_slider,
            volume,
            unmuted_volume: if volume > 0. { volume } else { 1. },
            trace: std::env::var_os("MUSIC_TRACE").map(|_| Instant::now()),
            np: NowPlaying::new(cx.focus_handle()),
            frame_stats: std::env::var_os("MUSIC_TIMINGS").map(|_| FrameStats::default()),
            lyrics: HashMap::new(),
            hires: HashMap::new(),
            _poll: poll,
            _subscriptions: subscriptions,
        }
    }

    /// `MUSIC_QUERY` pre-fills and runs a search; `MUSIC_AUTOOPEN=1` then plays the first
    /// result and opens Now Playing. Lets RSS/CPU be measured without clicking around.
    /// (`MUSIC_GPUI_QUERY` / `MUSIC_GPUI_AUTOPLAY` from the first round still work.)
    ///
    /// `MUSIC_OPEN_SETTINGS=1|eq|accounts` opens the settings sheet,
    /// `MUSIC_EQ_PRESET=Bass` applies (and saves) an EQ preset.
    fn apply_startup_env(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        if let Some(name) = env("MUSIC_EQ_PRESET") {
            self.script_action("eq", Some(&name), window, cx);
        }
        if let Some(tab) = env("MUSIC_OPEN_SETTINGS").filter(|v| v != "0") {
            self.script_action("settings", Some(&tab), window, cx);
        }
        let Some(query) = env("MUSIC_QUERY").or_else(|| env("MUSIC_GPUI_QUERY")) else {
            return;
        };
        self.autoopen = env("MUSIC_AUTOOPEN").is_some_and(|v| v != "0");
        self.autoplay = self.autoopen || env("MUSIC_GPUI_AUTOPLAY").is_some();
        self.search_input
            .update(cx, |input, cx| input.set_value(query.clone(), window, cx));
        self.start_search(query.trim().to_owned(), None, cx);
    }

    // ---- search -------------------------------------------------------------

    fn on_search_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let query = input.read(cx).value().trim().to_owned();
        match event {
            InputEvent::Change if query != self.query => {
                self.start_search(query, Some(SEARCH_DEBOUNCE), cx)
            }
            InputEvent::PressEnter { .. } => self.start_search(query, None, cx),
            _ => {}
        }
    }

    fn start_search(&mut self, query: String, debounce: Option<Duration>, cx: &mut Context<Self>) {
        self.query = query.clone();
        if query.is_empty() {
            self.search_task = None;
            self.search_status = SearchStatus::Idle;
            cx.notify();
            return;
        }
        let executor = cx.background_executor().clone();
        self.search_task = Some(cx.spawn(async move |this, cx| {
            if let Some(delay) = debounce {
                executor.timer(delay).await;
            }
            this.update(cx, |this, cx| {
                this.search_status = SearchStatus::Searching;
                cx.notify();
            })
            .ok();
            let q = query.clone();
            let result = cx
                .background_spawn(async move { InnerTube::default().search_songs(&q) })
                .await;
            this.update(cx, |this, cx| {
                if this.query != query {
                    return;
                }
                match result {
                    Ok(tracks) => {
                        this.row_sizes = Rc::new(
                            tracks
                                .iter()
                                .map(|_| size(px(0.), px(ROW_HEIGHT)))
                                .collect(),
                        );
                        this.tracks = Rc::new(tracks);
                        this.search_status = SearchStatus::Done;
                        this.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
                        if std::mem::take(&mut this.autoplay) {
                            this.play_result(0, cx);
                        }
                    }
                    Err(e) => this.search_status = SearchStatus::Failed(e.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    // ---- artwork ------------------------------------------------------------

    /// Starts a background fetch for `track`'s artwork unless it is cached or in flight.
    fn ensure_artwork(&mut self, track: &Track, cx: &mut Context<Self>) {
        if self.artwork.contains_key(&track.video_id) {
            return;
        }
        let Some(url) = track.artwork_url(ART_SIZE) else {
            self.artwork.insert(track.video_id.clone(), Artwork::Failed);
            return;
        };
        let id = track.video_id.clone();
        self.artwork.insert(id.clone(), Artwork::Loading);
        cx.spawn(async move |this, cx| {
            let bytes = cx
                .background_spawn(async move { music_core::fetch_bytes(&url) })
                .await;
            this.update(cx, |this, cx| {
                let art = match bytes {
                    Ok(bytes) => {
                        Artwork::Ready(Arc::new(Image::from_bytes(sniff_format(&bytes), bytes)))
                    }
                    Err(e) => {
                        eprintln!("music-gpui: artwork {id}: {e}");
                        Artwork::Failed
                    }
                };
                this.artwork.insert(id, art);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn artwork_element(
        &mut self,
        track: Option<&Track>,
        side: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        if let Some(track) = track {
            self.ensure_artwork(track, cx);
        }
        let image = track
            .and_then(|t| self.artwork.get(&t.video_id))
            .and_then(|a| match a {
                Artwork::Ready(image) => Some(image.clone()),
                Artwork::Loading | Artwork::Failed => None,
            });
        div()
            .size(side)
            .flex_none()
            .rounded(px(6.))
            .overflow_hidden()
            .bg(cx.theme().muted)
            .when_some(image, |this, image| {
                this.child(img(image).size_full().object_fit(ObjectFit::Cover))
            })
    }

    // ---- playback -----------------------------------------------------------

    /// Plays result `ix` with the whole result list as the queue.
    fn play_result(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(track) = self.queue.replace(self.tracks.clone(), ix) {
            self.play(track, cx);
        }
    }

    pub fn next_track(&mut self, cx: &mut Context<Self>) {
        if let Some(track) = self.queue.advance() {
            self.play(track, cx);
        }
    }

    /// Restarts the track past 3 s, else plays the previous one.
    pub fn previous_track(&mut self, cx: &mut Context<Self>) {
        if self.playback.track.is_none() {
            return;
        }
        match self.queue.previous(self.clock.position(Instant::now())) {
            Previous::Play(track) => self.play(track, cx),
            Previous::Restart => {
                if let Some(engine) = &mut self.engine {
                    engine.restart();
                    self.clock.seek(Duration::ZERO, Instant::now());
                    self.np.manual_until = None;
                    self.refresh_soon(cx);
                }
            }
        }
    }

    fn play(&mut self, track: Track, cx: &mut Context<Self>) {
        // Prefetch what the Now Playing screen needs while audio resolves.
        self.ensure_lyrics(&track, cx);
        self.ensure_hires(&track, cx);
        match &mut self.engine {
            Some(engine) => {
                engine.play(track);
                self.refresh_soon(cx);
            }
            None => {
                self.pending_play = Some(track);
                cx.notify();
            }
        }
    }

    fn toggle_pause(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_playback(cx);
    }

    fn toggle_playback(&mut self, cx: &mut Context<Self>) {
        if let Some(engine) = &mut self.engine {
            engine.toggle_pause();
            self.refresh_soon(cx);
        }
    }

    /// Re-reads the player state shortly after a command instead of waiting for the next tick.
    fn refresh_soon(&mut self, cx: &mut Context<Self>) {
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            executor.timer(Duration::from_millis(30)).await;
            this.update(cx, |this, cx| {
                if let Some(engine) = &this.engine {
                    let state = engine.state();
                    if this.ingest(state, cx) {
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Takes a new snapshot; returns whether it differs from the last one.
    /// Auto-advances when the queue's current track just ended (a preview ending
    /// early is an ordinary `Ended` too).
    fn ingest(&mut self, state: PlaybackState, cx: &mut Context<Self>) -> bool {
        self.clock.sync(&state, Instant::now());
        if same_snapshot(&self.playback, &state) {
            return false;
        }
        if let Some(started) = self.trace {
            trace(started, &self.playback, &state);
        }
        let just_ended =
            state.status == PlaybackStatus::Ended && self.playback.status != PlaybackStatus::Ended;
        self.playback = state;
        if just_ended
            && let Some(track) = &self.playback.track
            && self.queue.is_current(&track.video_id)
        {
            self.next_track(cx);
        }
        true
    }

    fn poll_player(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(engine) = &self.engine else { return };
        let state = engine.state();

        let artwork = state
            .track
            .as_ref()
            .and_then(|t| match self.hires.get(&t.video_id) {
                Some(HiRes::Ready { art, .. }) => Some(art.bytes()),
                _ => None,
            });
        let report = self.services.update(&state, artwork);
        if !report.scrobble_errors.is_empty() || report.discord_status.is_some() {
            self.settings.update(cx, |panel, cx| {
                if let Some(status) = report.discord_status {
                    panel.set_discord_status(status, cx);
                }
                panel.push_scrobble_errors(report.scrobble_errors, cx);
            });
        }
        for event in self.services.take_events() {
            self.on_media_event(event, window, cx);
        }

        if !self.ingest(state, cx) {
            return;
        }
        let state = &self.playback;
        let max = state.duration.map_or(1., |d| d.as_secs_f32().max(1.));
        let pos = state.position.as_secs_f32().min(max);
        if self.scrub.is_none() {
            self.progress.update(cx, |slider, cx| {
                if (slider.max_value() - max).abs() > f32::EPSILON {
                    *slider = SliderState::new().min(0.).max(max).step(0.25);
                }
                slider.set_value(pos, window, cx);
            });
        }
        if self.autoopen && !self.autoplay && self.playback.track.is_some() {
            self.autoopen = false;
            self.open_now_playing(window, cx);
        }
        cx.notify();
    }

    fn on_progress_event(
        &mut self,
        _: &Entity<SliderState>,
        event: &SliderEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SliderEvent::Change(value) => {
                self.scrub = Some(value.start());
                cx.notify();
            }
            SliderEvent::Release(value) => {
                self.scrub = None;
                self.seek_to(Duration::from_secs_f32(value.start().max(0.)), cx);
            }
        }
    }

    /// OS media keys / Control Center. With real audio only next/previous get
    /// here (`MediaControls::for_player` routes the rest to the `Player`).
    fn on_media_event(
        &mut self,
        event: MediaControlEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let playing = self.playback.status == PlaybackStatus::Playing;
        match event {
            MediaControlEvent::Next => self.next_track(cx),
            MediaControlEvent::Previous => self.previous_track(cx),
            MediaControlEvent::Toggle => self.toggle_playback(cx),
            MediaControlEvent::Play if !playing => self.toggle_playback(cx),
            MediaControlEvent::Pause | MediaControlEvent::Stop if playing => {
                self.toggle_playback(cx)
            }
            MediaControlEvent::Play | MediaControlEvent::Pause | MediaControlEvent::Stop => {}
            MediaControlEvent::SeekTo(position) => self.seek_to(position, cx),
            MediaControlEvent::SeekBy(secs) => {
                let now = self.clock.position(Instant::now()).as_secs_f64();
                self.seek_to(Duration::from_secs_f64((now + secs).max(0.)), cx);
            }
            MediaControlEvent::SetVolume(volume) => self.set_volume(volume as f32, window, cx),
        }
    }

    // ---- volume & settings ----------------------------------------------------

    fn on_volume_event(
        &mut self,
        _: &Entity<SliderState>,
        event: &SliderEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
        self.apply_volume(value.start(), cx);
    }

    /// Sets the volume (live + saved) and moves the slider.
    pub fn set_volume(&mut self, volume: f32, window: &mut Window, cx: &mut Context<Self>) {
        let volume = volume.clamp(0., 1.);
        self.volume_slider
            .update(cx, |slider, cx| slider.set_value(volume, window, cx));
        self.apply_volume(volume, cx);
    }

    fn apply_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        self.volume = volume;
        if volume > 0. {
            self.unmuted_volume = volume;
        }
        if let Some(engine) = &mut self.engine {
            engine.set_volume(volume);
        }
        self.settings
            .update(cx, |panel, cx| panel.set_volume(volume, cx));
        cx.notify();
    }

    fn toggle_mute(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let target = if self.volume > 0. {
            0.
        } else {
            self.unmuted_volume
        };
        self.set_volume(target, window, cx);
    }

    fn on_settings_event(
        &mut self,
        _: &Entity<SettingsPanel>,
        event: &SettingsEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SettingsEvent::Equalizer(eq) => {
                if let Some(engine) = &mut self.engine {
                    engine.set_equalizer(eq.clone());
                }
            }
            SettingsEvent::Integrations => self.apply_integrations(cx),
        }
    }

    /// (Re)builds the scrobbler and Discord presence from the current settings.
    fn apply_integrations(&mut self, cx: &mut Context<Self>) {
        let panel = self.settings.read(cx);
        let scrobble = panel.scrobble_config();
        let discord = panel.discord_client_id();
        self.services.configure_scrobbling(scrobble);
        let status = self.services.configure_discord(discord);
        self.settings
            .update(cx, |panel, cx| panel.set_discord_status(status, cx));
    }

    pub fn open_settings(&mut self, tab: Option<Tab>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = tab {
            self.settings.update(cx, |panel, cx| {
                panel.tab = tab;
                cx.notify();
            });
        }
        let panel = self.settings.clone();
        window.open_sheet(cx, move |sheet, _, _| {
            sheet
                .title("Settings")
                .size(px(480.))
                .resizable(false)
                .child(panel.clone())
        });
    }

    // ---- rendering ----------------------------------------------------------

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let status: Option<SharedString> = match &self.search_status {
            SearchStatus::Idle => None,
            SearchStatus::Searching => Some("Searching…".into()),
            SearchStatus::Done => Some(format!("{} songs", self.tracks.len()).into()),
            SearchStatus::Failed(_) => Some("Search failed".into()),
        };
        h_flex()
            .px_5()
            .py_3()
            .gap_4()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.foreground)
                    .child("Music"),
            )
            .child(
                div().flex_1().max_w(px(560.)).child(
                    Input::new(&self.search_input)
                        .prefix(IconName::Search)
                        .cleanable(true),
                ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .when(
                        matches!(self.search_status, SearchStatus::Searching),
                        |this| this.child(Spinner::new().small()),
                    )
                    .children(status),
            )
            .child(div().flex_1())
            .child(
                Button::new("open-settings")
                    .ghost()
                    .icon(IconName::Settings)
                    .tooltip("Settings: equalizer, scrobbling, Discord")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_settings(None, window, cx)),
                    ),
            )
    }

    fn render_results(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme();
        if self.tracks.is_empty() {
            let (title, detail): (SharedString, SharedString) = match &self.search_status {
                SearchStatus::Failed(e) => ("Search failed".into(), e.clone().into()),
                SearchStatus::Searching => ("Searching…".into(), "".into()),
                SearchStatus::Done => ("No songs found".into(), "Try another search.".into()),
                SearchStatus::Idle => (
                    "Search YouTube Music".into(),
                    "Type a song, artist or album above.".into(),
                ),
            };
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_1()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child(title),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(detail),
                )
                .into_any_element();
        }

        let current_id = self.playback.track.as_ref().map(|t| t.video_id.clone());
        let list = v_virtual_list(
            cx.entity(),
            "results",
            self.row_sizes.clone(),
            move |this, range, _window, cx| {
                let tracks = this.tracks.clone();
                range
                    .map(|ix| {
                        let track = &tracks[ix];
                        let active = current_id.as_deref() == Some(track.video_id.as_str());
                        this.render_row(ix, track, active, cx)
                    })
                    .collect::<Vec<_>>()
            },
        )
        .track_scroll(&self.scroll);

        div()
            .flex_1()
            .min_h_0()
            .relative()
            .overflow_hidden()
            .child(list)
            .vertical_scrollbar(&self.scroll)
            .into_any_element()
    }

    fn render_row(
        &mut self,
        ix: usize,
        track: &Track,
        active: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let art = self.artwork_element(Some(track), px(44.), cx);
        let theme = cx.theme();
        let subtitle = match &track.album {
            Some(album) => format!("{} · {album}", track.artists),
            None => track.artists.clone(),
        };
        let duration = track
            .duration_secs
            .map(|s| format_duration(s.into()))
            .unwrap_or_default();
        let hover_bg = theme.list_hover;
        let title_color = if active {
            theme.primary
        } else {
            theme.foreground
        };
        let index_color = if active {
            theme.primary
        } else {
            theme.muted_foreground
        };

        h_flex()
            .id(("row", ix))
            .w_full()
            .h(px(ROW_HEIGHT))
            .px_5()
            .gap_4()
            .cursor_pointer()
            .when(active, |this| this.bg(theme.list_active))
            .hover(move |style| style.bg(hover_bg))
            .on_click(cx.listener(move |this, _, _, cx| this.play_result(ix, cx)))
            .child(
                div()
                    .w(px(24.))
                    .flex_none()
                    .text_sm()
                    .text_right()
                    .text_color(index_color)
                    .child(format!("{}", ix + 1)),
            )
            .child(art)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        div()
                            .truncate()
                            .text_color(title_color)
                            .font_weight(FontWeight::MEDIUM)
                            .child(track.title.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(subtitle),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(duration),
            )
    }

    fn render_now_playing(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let track = self.playback.track.clone();
        let art = self.artwork_element(track.as_ref(), px(56.), cx);
        let has_next = self.queue.has_next();
        let theme = cx.theme();
        let state = &self.playback;

        let (title, artists): (SharedString, SharedString) = match &track {
            Some(t) => (t.title.clone().into(), t.artists.clone().into()),
            None => ("Not playing".into(), "Pick a song to start".into()),
        };

        let is_playing = state.status == PlaybackStatus::Playing;
        let can_toggle = self.engine.is_some()
            && matches!(
                state.status,
                PlaybackStatus::Playing | PlaybackStatus::Paused | PlaybackStatus::Ended
            );
        let loading = state.status == PlaybackStatus::Loading;

        let total = state.duration.map(|d| d.as_secs()).unwrap_or(0);
        let elapsed = self
            .scrub
            .map(|s| s as u64)
            .unwrap_or_else(|| state.position.as_secs());

        let message: Option<(SharedString, Hsla)> = if let Some(err) = &self.audio_error {
            Some((format!("Audio unavailable: {err}").into(), theme.danger))
        } else if let PlaybackStatus::Error(err) = &state.status {
            Some((err.clone().into(), theme.danger))
        } else if loading {
            Some(("Loading…".into(), theme.muted_foreground))
        } else {
            None
        };

        h_flex()
            .h(px(88.))
            .flex_none()
            .px_5()
            .gap_4()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            // Left: artwork + title; click to expand into Now Playing.
            .child(
                h_flex()
                    .id("np-bar-open")
                    .w(px(280.))
                    .flex_none()
                    .gap_3()
                    .p_1()
                    .rounded(px(8.))
                    .when(track.is_some(), |this| {
                        let hover = theme.list_hover;
                        this.cursor_pointer().hover(move |s| s.bg(hover)).on_click(
                            cx.listener(|this, _, window, cx| this.open_now_playing(window, cx)),
                        )
                    })
                    .child(art)
                    .child(
                        v_flex()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                div()
                                    .truncate()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(artists),
                            ),
                    ),
            )
            // Centre: transport + progress.
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .items_center()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("previous")
                                    .ghost()
                                    .rounded(px(999.))
                                    .icon(AppIcon::Previous)
                                    .tooltip("Previous (restarts after 3 s)")
                                    .disabled(track.is_none())
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.previous_track(cx)),
                                    ),
                            )
                            .child(
                                Button::new("play-pause")
                                    .primary()
                                    .rounded(px(999.))
                                    .icon(if is_playing {
                                        IconName::Pause
                                    } else {
                                        IconName::Play
                                    })
                                    .loading(loading)
                                    .disabled(!can_toggle)
                                    .on_click(cx.listener(Self::toggle_pause)),
                            )
                            .child(
                                Button::new("next")
                                    .ghost()
                                    .rounded(px(999.))
                                    .icon(AppIcon::Next)
                                    .tooltip("Next")
                                    .disabled(!has_next)
                                    .on_click(cx.listener(|this, _, _, cx| this.next_track(cx))),
                            )
                            .when(state.preview_only, |this| {
                                this.child(Tag::warning().small().rounded_full().child("Preview"))
                            }),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .max_w(px(560.))
                            .gap_3()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(
                                div()
                                    .w(px(40.))
                                    .text_right()
                                    .child(format_duration(elapsed)),
                            )
                            .child(
                                div().flex_1().child(
                                    Slider::new(&self.progress)
                                        .horizontal()
                                        .disabled(track.is_none() || total == 0),
                                ),
                            )
                            .child(div().w(px(40.)).child(if total > 0 {
                                format_duration(total)
                            } else {
                                "--:--".into()
                            })),
                    ),
            )
            // Right: status / errors, volume, expand button.
            .child(
                h_flex()
                    .w(px(300.))
                    .flex_none()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_right()
                            .line_clamp(3)
                            .when_some(message, |this, (text, color)| {
                                this.text_color(color).child(text)
                            }),
                    )
                    .child(
                        Button::new("mute")
                            .ghost()
                            .small()
                            .icon(if self.volume <= 0. {
                                AppIcon::Muted
                            } else if self.volume < 0.5 {
                                AppIcon::VolumeLow
                            } else {
                                AppIcon::Volume
                            })
                            .tooltip(format!("Volume {:.0} %", self.volume * 100.))
                            .on_click(cx.listener(Self::toggle_mute)),
                    )
                    .child(
                        div()
                            .w(px(84.))
                            .flex_none()
                            .child(Slider::new(&self.volume_slider).horizontal()),
                    )
                    .child(
                        Button::new("np-expand")
                            .ghost()
                            .icon(IconName::ChevronUp)
                            .tooltip("Now Playing & lyrics")
                            .disabled(track.is_none())
                            .on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.open_now_playing(window, cx)
                                }),
                            ),
                    ),
            )
    }
}

impl Render for MusicApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut frame = Frame::new();
        let reveal = frame.get(&self.np.reveal);
        // Once the Now Playing screen fully covers the window, skip building the
        // browse UI underneath, so per-frame lyric renders stay cheap.
        let covered = reveal >= 1.;
        let mut root = div()
            .relative()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground);
        if !covered {
            let header = self.render_header(cx);
            let results = self.render_results(cx);
            let now_playing = self.render_now_playing(cx);
            root = root.child(
                v_flex()
                    .size_full()
                    .child(header)
                    .child(results)
                    .child(now_playing),
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

/// Field-wise comparison (`PlaybackState` has no `PartialEq`); avoids redundant re-renders.
fn same_snapshot(a: &PlaybackState, b: &PlaybackState) -> bool {
    a.status == b.status
        && a.position == b.position
        && a.duration == b.duration
        && a.preview_only == b.preview_only
        && a.volume == b.volume
        && a.track.as_ref().map(|t| &t.video_id) == b.track.as_ref().map(|t| &t.video_id)
}

/// `MUSIC_TRACE=1`: one line per status/track change, seek, and every 5 s of position.
fn trace(started: Instant, old: &PlaybackState, new: &PlaybackState) {
    let id = |s: &PlaybackState| s.track.as_ref().map(|t| t.video_id.clone());
    let jumped = new.position.abs_diff(old.position) > Duration::from_millis(1500);
    let tick = new.position.as_secs() / 5 != old.position.as_secs() / 5;
    if old.status != new.status || id(old) != id(new) || jumped || tick {
        eprintln!(
            "music-gpui: [{:6.2}s] {:?} {} pos {:.1}s / {} vol {:.2}{}",
            started.elapsed().as_secs_f64(),
            new.status,
            id(new).unwrap_or_default(),
            new.position.as_secs_f64(),
            new.duration
                .map_or("?".into(), |d| format!("{:.1}s", d.as_secs_f64())),
            new.volume,
            if new.preview_only { " preview" } else { "" },
        );
    }
}

/// YouTube serves JPEG for `=w..-h..` URLs but may switch to WebP/PNG.
fn sniff_format(bytes: &[u8]) -> ImageFormat {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => ImageFormat::Png,
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => ImageFormat::Webp,
        [b'G', b'I', b'F', ..] => ImageFormat::Gif,
        _ => ImageFormat::Jpeg,
    }
}

fn main() {
    gpui_platform::application()
        .with_assets(icons::Assets)
        .run(|cx: &mut App| {
            gpui_component::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            // No custom title bar, so sheets can start at the top of the content.
            Theme::global_mut(cx).sheet.margin_top = px(0.);
            cx.bind_keys([
                KeyBinding::new("escape", CloseNowPlaying, Some("NowPlaying")),
                KeyBinding::new("space", TogglePlayback, Some("NowPlaying")),
            ]);

            let bounds = Bounds::centered(None, size(px(1100.), px(720.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(720.), px(480.))),
                // GPUI caps unfocused windows to ~30 fps by default; lyrics should stay
                // smooth while you work in another app (frames are only requested while
                // the Now Playing screen is visible and moving anyway).
                inactive_frame_interval: None,
                titlebar: Some(TitlebarOptions {
                    title: Some("Music".into()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let mut app = None;
            let window = cx
                .open_window(options, |window, cx| {
                    let view = cx.new(|cx| MusicApp::new(window, cx));
                    app = Some(view.clone());
                    cx.new(|cx| Root::new(view, window, cx))
                })
                .expect("failed to open window");
            let app = app.expect("window built");
            // After the Root exists, so the settings sheet can open from startup hooks.
            window
                .update(cx, |_, window, cx| {
                    app.update(cx, |app, cx| app.apply_startup_env(window, cx))
                })
                .ok();
            script::schedule(window.into(), app, cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
}
