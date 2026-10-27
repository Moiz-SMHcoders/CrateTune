//! YouTube Music spike UI on GPUI + gpui-component.
//!
//! One root view (`MusicApp`) owns all state. Blocking `music-core` calls run on
//! GPUI's background executor; a foreground task polls `Player::state()` every
//! 250 ms and only re-renders when the snapshot changed.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, AppContext as _, Bounds, ClickEvent, Context, Entity, Focusable as _, FontWeight, Hsla,
    Image, ImageFormat, InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _,
    Pixels, Render, SharedString, Size, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, Subscription, Task, TitlebarOptions, Window, WindowBounds, WindowOptions,
    div, img, prelude::FluentBuilder as _, px, size,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconName, Root, Sizable as _, Theme, ThemeMode,
    VirtualListScrollHandle,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement as _,
    slider::{Slider, SliderEvent, SliderState},
    spinner::Spinner,
    tag::Tag,
    v_flex, v_virtual_list,
};
use music_core::{
    InnerTube, IosClientResolver, PlaybackState, PlaybackStatus, Player, Track, format_duration,
};

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

    player: Option<Player>,
    audio_error: Option<String>,
    playback: PlaybackState,
    progress: Entity<SliderState>,
    /// Seconds under the pointer while the user drags the progress slider.
    scrub: Option<f32>,
    /// Play the first result of the next search (`MUSIC_GPUI_AUTOPLAY`, for measurements).
    autoplay: bool,

    _poll: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl MusicApp {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search songs, artists, albums"));
        let progress = cx.new(|_| SliderState::new().min(0.).max(1.).step(0.25));

        let subscriptions = vec![
            cx.subscribe_in(&search_input, window, Self::on_search_event),
            cx.subscribe_in(&progress, window, Self::on_progress_event),
        ];

        // Opening the audio device can block briefly (and fails in sandboxes), so keep
        // it off the main thread; search works regardless.
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async { Player::new(Arc::new(IosClientResolver)) })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(player) => this.player = Some(player),
                    Err(e) => this.audio_error = Some(e.to_string()),
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
            player: None,
            audio_error: None,
            playback: PlaybackState::default(),
            progress,
            scrub: None,
            autoplay: false,
            _poll: poll,
            _subscriptions: subscriptions,
        }
    }

    /// `MUSIC_GPUI_QUERY` pre-fills and runs a search; `MUSIC_GPUI_AUTOPLAY=1` then plays
    /// the first result. Lets RSS/CPU be measured without clicking around.
    fn apply_startup_env(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(query) = std::env::var("MUSIC_GPUI_QUERY") else {
            return;
        };
        self.autoplay = std::env::var_os("MUSIC_GPUI_AUTOPLAY").is_some();
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
                        if std::mem::take(&mut this.autoplay)
                            && let Some(first) = this.tracks.first().cloned()
                        {
                            this.play(first, cx);
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

    fn play(&mut self, track: Track, cx: &mut Context<Self>) {
        match &self.player {
            Some(player) => {
                player.play(track);
                self.refresh_soon(cx);
            }
            None => {
                if self.audio_error.is_none() {
                    self.audio_error = Some("audio engine is still starting".into());
                }
                cx.notify();
            }
        }
    }

    fn toggle_pause(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(player) = &self.player {
            player.toggle_pause();
            self.refresh_soon(cx);
        }
    }

    /// Re-reads the player state shortly after a command instead of waiting for the next tick.
    fn refresh_soon(&mut self, cx: &mut Context<Self>) {
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            executor.timer(Duration::from_millis(30)).await;
            this.update(cx, |this, cx| {
                if let Some(player) = &this.player {
                    let state = player.state();
                    if !same_snapshot(&this.playback, &state) {
                        this.playback = state;
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn poll_player(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(player) = &self.player else { return };
        let state = player.state();
        if same_snapshot(&self.playback, &state) {
            return;
        }
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
        self.playback = state;
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
                if let Some(player) = &self.player {
                    player.seek(Duration::from_secs_f32(value.start().max(0.)));
                }
                self.refresh_soon(cx);
            }
        }
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
        let track_to_play = track.clone();

        h_flex()
            .id(("row", ix))
            .w_full()
            .h(px(ROW_HEIGHT))
            .px_5()
            .gap_4()
            .cursor_pointer()
            .when(active, |this| this.bg(theme.list_active))
            .hover(move |style| style.bg(hover_bg))
            .on_click(cx.listener(move |this, _, _, cx| this.play(track_to_play.clone(), cx)))
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
        let theme = cx.theme();
        let state = &self.playback;

        let (title, artists): (SharedString, SharedString) = match &track {
            Some(t) => (t.title.clone().into(), t.artists.clone().into()),
            None => ("Not playing".into(), "Pick a song to start".into()),
        };

        let is_playing = state.status == PlaybackStatus::Playing;
        let can_toggle = self.player.is_some()
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
            // Left: artwork + title.
            .child(
                h_flex().w(px(300.)).flex_none().gap_3().child(art).child(
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
            // Right: status / errors.
            .child(
                div()
                    .w(px(260.))
                    .flex_none()
                    .text_xs()
                    .text_right()
                    .line_clamp(3)
                    .when_some(message, |this, (text, color)| {
                        this.text_color(color).child(text)
                    }),
            )
    }
}

impl Render for MusicApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.render_header(cx);
        let results = self.render_results(cx);
        let now_playing = self.render_now_playing(cx);
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(header)
            .child(results)
            .child(now_playing)
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
        .with_assets(gpui_kit_assets::Assets)
        .run(|cx: &mut App| {
            gpui_component::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);

            let bounds = Bounds::centered(None, size(px(1100.), px(720.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(720.), px(480.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Music".into()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let _window = cx
                .open_window(options, |window, cx| {
                    let view = cx.new(|cx| MusicApp::new(window, cx));
                    view.update(cx, |app, cx| app.apply_startup_env(window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
                .expect("failed to open window");
            #[cfg(feature = "screenshot")]
            screenshot::schedule(_window.into(), cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
}

/// Dev-only: `MUSIC_GPUI_SCREENSHOT=out.png` renders the window offscreen after
/// `MUSIC_GPUI_SCREENSHOT_DELAY` seconds (default 6), saves it, and quits.
#[cfg(feature = "screenshot")]
mod screenshot {
    use std::time::Duration;

    use gpui::{AnyWindowHandle, App, AppContext as _};

    pub fn schedule(window: AnyWindowHandle, cx: &mut App) {
        let Ok(path) = std::env::var("MUSIC_GPUI_SCREENSHOT") else {
            return;
        };
        let delay = std::env::var("MUSIC_GPUI_SCREENSHOT_DELAY")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(6.0);
        let executor = cx.background_executor().clone();
        cx.spawn(async move |cx| {
            executor.timer(Duration::from_secs_f64(delay)).await;
            let result = cx.update_window(window, |_, window, _| window.render_to_image());
            match result {
                Ok(Ok(image)) => match image.save(&path) {
                    Ok(()) => eprintln!("music-gpui: screenshot saved to {path}"),
                    Err(e) => eprintln!("music-gpui: saving screenshot failed: {e}"),
                },
                Ok(Err(e)) => eprintln!("music-gpui: render_to_image failed: {e}"),
                Err(e) => eprintln!("music-gpui: window gone: {e}"),
            }
            cx.update(|cx| cx.quit());
        })
        .detach();
    }
}
