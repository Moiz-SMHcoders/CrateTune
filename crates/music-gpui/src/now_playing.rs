//! Full-window "Now Playing" screen in three layouts (split, big artwork,
//! lyrics first) over one of four backgrounds (blurred artwork, artwork
//! gradient, solid colour, drifting glow), with transport, sleep timer and a
//! lyrics pane: synced lyrics with Apple-Music-like staggered scrolling in six
//! animation styles (variants of the `KaraokeLine` renderer), background vocals,
//! duet sides, romanization and translation lines, or plain text.
//!
//! Frames are only requested while something moves (`anim::Frame`): the
//! playback clock, a tween, or the glow background while playing.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnimationExt as _, AnyElement, App, Bounds, Context, Div, Entity, FocusHandle, FontWeight,
    Hsla, Image, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ObjectFit,
    ParentElement as _, Pixels, RenderImage, ScrollHandle, SharedString, SpringAnimation,
    SpringConfig, StatefulInteractiveElement as _, StyleRefinement, Styled as _, StyledImage as _,
    Task, Transformation, Window, canvas, div, hsla, img, linear_color_stop, linear_gradient,
    prelude::FluentBuilder as _, px, radians, relative, size, svg,
};
use gpui_component::{
    Disableable as _, IconName, IconNamed as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    spinner::Spinner,
    tag::Tag,
    v_flex,
};
use music_core::lyrics::SyncKind;
use music_core::{PlaybackStatus, Track, format_duration};

use crate::anim::{Frame, Spec, Tween, ease_in_out_cubic, ease_out_cubic};
use crate::app::{MusicApp, update_app};
use crate::backdrop::Palette;
use crate::components::artwork::ArtworkCache;
use crate::icons::{AppIcon, Lucide};
use crate::lyrics::{LyricsState, PreparedLyrics};
use crate::np_lyrics::{LyricsList, MAX_SELECTED};
use crate::np_region::{self, RegionKey};
use crate::settings::prefs::{LyricsDisplay, NowPlayingBackground, NowPlayingLayout, TextAlign};
use crate::snapshot::{self, Snapshot};
use crate::theme::AppColors;
use crate::util::off_thread;

/// Hi-res artwork + blurred backdrop entries kept (most recent tracks).
const HIRES_KEEP: usize = 8;

const OPEN: Spec = Spec::ms(320, ease_out_cubic);
const CLOSE: Spec = Spec::ms(260, ease_in_out_cubic);
const BACKDROP_FADE: Spec = Spec::ms(500, ease_in_out_cubic);
const ICON_MORPH: Spec = Spec::ms(260, ease_out_cubic);
/// Redraw interval of the glow background when nothing else animates.
const GLOW_FRAME: Duration = Duration::from_millis(33);

/// Lyrics column width cap (long lines wrap, like Apple Music's lyrics column).
const LYRICS_MAX_WIDTH: f32 = 600.;
/// ...in the lyrics-first layout.
const LYRICS_WIDE_MAX_WIDTH: f32 = 920.;

/// Large artwork, its pre-blurred backdrop, palette and accent colour, per video id.
pub enum HiRes {
    Loading,
    Ready {
        art: Arc<Image>,
        backdrop: Arc<RenderImage>,
        /// Gradient / solid / glow background colours.
        palette: Palette,
        /// Soft lights in the palette's colours (animated glow background).
        glows: [Arc<RenderImage>; 3],
        /// Scored seed colours (dynamic theme, `palette.rs`), if the art has any.
        art_colors: Option<crate::theme::ArtColors>,
    },
    Failed,
}

#[derive(Clone, PartialEq)]
struct HeaderSnap {
    layout: NowPlayingLayout,
    background: NowPlayingBackground,
    demo: bool,
    buffering: bool,
    speed: Option<f32>,
    preview: bool,
    source: Option<SharedString>,
    sleep: Option<String>,
    sleep_custom: Duration,
    /// The window is (or is going) full screen; the button's tooltip.
    fullscreen: bool,
    fullscreen_tip: SharedString,
    /// The sound menu (lit while an effect is on).
    sound: crate::components::sound_menu::SoundSnap,
}

#[derive(Clone, PartialEq)]
struct ToolbarSnap {
    label: SharedString,
    /// The "sources tried" menu behind the label.
    sources: Arc<crate::np_lyrics::SourcesSnap>,
    /// Synced lyrics: the timing nudge controls show.
    synced: bool,
    offset_ms: i32,
    translate: bool,
    /// "Translating…" / the failure, and whether it failed (Retry).
    status: Option<(String, bool)>,
    /// Synced lyrics can be shared as a card.
    can_share: bool,
    selecting: bool,
    selected: usize,
}

pub struct NowPlaying {
    pub open: bool,
    /// Opened from the bar's lyrics button: a design without a lyrics pane shows
    /// the lyrics-first layout until Now Playing closes.
    pub lyrics_view: bool,
    pub reveal: Tween<f32>,
    pub focus: FocusHandle,
    backdrop_prev: Option<Arc<RenderImage>>,
    backdrop_cur: Option<Arc<RenderImage>>,
    backdrop_id: Option<String>,
    backdrop_fade: Tween<f32>,
    /// Gradient / solid / glow colours, cross-faded between tracks.
    pub(crate) colors: [Tween<Hsla>; 3],
    colors_id: Option<String>,
    /// Glow background phase; advances only while playing.
    glow_phase: f32,
    glows: Option<[Arc<RenderImage>; 3]>,
    glow_last: Option<Instant>,
    play_morph: Tween<f32>,
    plain_scroll: ScrollHandle,
    /// The synced lyrics list (`np_lyrics.rs`).
    pub list: LyricsList,
    header: Option<Entity<Snapshot<HeaderSnap>>>,
    /// Cached regions: the split layout's player column, the lyrics-first strip.
    column: Option<Entity<np_region::Region>>,
    strip: Option<Entity<np_region::Region>>,
    /// The open lyrics card dialog (`lyrics_card.rs`).
    pub card: Option<Entity<crate::lyrics_card::LyricsCard>>,
    toolbar: Option<Entity<Snapshot<ToolbarSnap>>>,
    progress_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// A pending timed re-render (`Frame::wake_at`) while nothing animates.
    wake: Option<(Instant, Task<()>)>,
    /// Turntable design: 0 = needle on the record, 1 = arm lifted.
    pub(crate) arm: Tween<f32>,
}

impl NowPlaying {
    pub fn new(focus: FocusHandle) -> Self {
        let grey = Tween::new(hsla(0., 0., 0.12, 1.));
        Self {
            open: false,
            lyrics_view: false,
            reveal: Tween::new(0.),
            focus,
            backdrop_prev: None,
            backdrop_cur: None,
            backdrop_id: None,
            backdrop_fade: Tween::new(1.),
            colors: [grey, grey, grey],
            colors_id: None,
            glow_phase: 0.,
            glows: None,
            glow_last: None,
            play_morph: Tween::new(0.),
            plain_scroll: ScrollHandle::new(),
            list: LyricsList::new(),
            header: None,
            column: None,
            strip: None,
            card: None,
            toolbar: None,
            progress_bounds: Rc::default(),
            wake: None,
            arm: Tween::new(1.),
        }
    }
}

pub(crate) fn white(alpha: f32) -> Hsla {
    hsla(0., 0., 1., alpha)
}

/// `color`'s hue at a fixed (dark) lightness, saturation capped for readability.
pub(crate) fn deep(color: Hsla, lightness: f32, alpha: f32) -> Hsla {
    hsla(color.h, (color.s * 0.9).min(0.75), lightness, alpha)
}

impl MusicApp {
    pub fn open_now_playing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A restored queue at a fresh start: load its track paused, then open.
        let Some(track) = self
            .playback
            .track
            .clone()
            .or_else(|| self.cue_restored(cx))
        else {
            return;
        };
        self.ensure_lyrics(&track, cx);
        self.ensure_hires(&track, cx);
        self.np.open = true;
        self.np.reveal.animate_to(1., OPEN, Instant::now());
        window.focus(&self.np.focus, cx);
        self.fullscreen_on_np_open(window, cx);
        cx.notify();
    }

    pub fn close_now_playing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.np.open = false;
        self.np.lyrics_view = false;
        self.np.reveal.animate_to(0., CLOSE, Instant::now());
        window.focus(&self.focus, cx);
        self.fullscreen_on_np_close(window, cx);
        cx.notify();
    }

    /// The moment playback reaches `target` (from `position` now, at the current speed).
    pub(crate) fn when_position(
        &self,
        target: Duration,
        position: Duration,
        now: Instant,
    ) -> Instant {
        let speed = self.playback.speed.max(0.1);
        now + target.saturating_sub(position).div_f32(speed)
    }

    /// Renders again at `at` (see `Frame::wake_at`): while nothing animates, the
    /// time label, the next lyric line or a low-rate background only need a frame
    /// when they change. A later or equal pending wake-up is replaced.
    pub fn schedule_np_wake(&mut self, at: Instant, cx: &mut Context<Self>) {
        if self
            .np
            .wake
            .as_ref()
            .is_some_and(|(pending, _)| *pending <= at)
        {
            return;
        }
        let delay = at.saturating_duration_since(Instant::now());
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update(cx, |this, cx| {
                this.np.wake = None;
                cx.notify();
            })
            .ok();
        });
        self.np.wake = Some((at, task));
    }

    pub fn seek_to(&mut self, position: Duration, cx: &mut Context<Self>) {
        let Some(engine) = &mut self.engine else {
            return;
        };
        engine.seek(position);
        self.clock.seek(position, Instant::now());
        self.np.list.manual_until = None;
        self.refresh_soon(cx);
        cx.notify();
    }

    /// Fetches the 544 px artwork (shared cache, fetch threads) and blurs it +
    /// extracts its palette and accent on a worker thread, once per track; keeps
    /// the last [`HIRES_KEEP`] tracks.
    pub fn ensure_hires(&mut self, track: &Track, cx: &mut Context<Self>) {
        if self.hires.contains_key(&track.video_id) {
            return;
        }
        let id = track.video_id.clone();
        self.hires_order.push_back(id.clone());
        while self.hires_order.len() > HIRES_KEEP {
            if let Some(old) = self.hires_order.pop_front() {
                self.hires.remove(&old);
            }
        }
        let Some(url) = track.artwork_url(544) else {
            self.hires.insert(id, HiRes::Failed);
            return;
        };
        self.hires.insert(id.clone(), HiRes::Loading);
        let art_key = url.clone();
        // Artwork colours only feed the dynamic theme (`theme_state.rs` extracts them
        // later if the theme is switched to dynamic), the player bar's background
        // effect (glow / dynamic gradient / pulse colours) and the app-wide look.
        let want_colors = crate::look_state::wants_art_colors(self.settings.read(cx).settings());
        let art = ArtworkCache::global(cx).update(cx, |cache, cx| cache.load(url, cx));
        cx.spawn(async move |this, cx| {
            let entry = match art.await {
                None => HiRes::Failed,
                Some(art) => {
                    let image = art.clone();
                    let result = off_thread(move || {
                        let bytes = image.bytes();
                        let started = Instant::now();
                        let backdrop = crate::backdrop::blurred_backdrop(bytes);
                        let accent = want_colors
                            .then(|| crate::palette::cached_art_colors(&art_key, bytes))
                            .flatten();
                        if std::env::var_os("MUSIC_TIMINGS").is_some() {
                            eprintln!(
                                "music-gpui: backdrop blur + accent took {:?}",
                                started.elapsed()
                            );
                        }
                        backdrop.map(|(backdrop, palette)| {
                            let [a, b, c] = palette;
                            let glows = [
                                crate::backdrop::glow_blob(deep(c, 0.5, 0.8)),
                                crate::backdrop::glow_blob(deep(a, 0.42, 0.7)),
                                crate::backdrop::glow_blob(deep(b, 0.38, 0.65)),
                            ];
                            ((backdrop, palette, glows), accent)
                        })
                    })
                    .await;
                    match result {
                        Ok(((backdrop, palette, glows), accent)) => HiRes::Ready {
                            art,
                            backdrop,
                            palette,
                            glows,
                            art_colors: accent,
                        },
                        Err(e) => {
                            eprintln!("music-gpui: hi-res artwork {id}: {e}");
                            HiRes::Failed
                        }
                    }
                }
            };
            this.update(cx, |this, cx| {
                // Dropped from the keep-list meanwhile: don't resurrect it.
                if let Some(slot) = this.hires.get_mut(&id) {
                    *slot = entry;
                    this.update_accent(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn render_now_playing_screen(
        &mut self,
        reveal: f32,
        frame: &mut Frame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Below the custom top bar (`titlebar.rs`), except in full screen.
        let top = self.np_top_inset(window);
        let viewport = self.np_viewport(window);
        let track = self.playback.track.clone();
        let position = self.clock.position(frame.now);
        if self.clock.is_running() {
            // The time labels change once a second (the progress bar moves less than
            // a pixel between the 250 ms polls, which re-render anyway); lyrics ask
            // for frames or wake-ups themselves.
            let next_second = Duration::from_secs(position.as_secs() + 1);
            frame.wake_at(self.when_position(next_second, position, frame.now));
        }
        let (mut config, display) = {
            let s = self.settings.read(cx).settings();
            (s.now_playing, s.lyrics.display())
        };
        if self.np.lyrics_view
            && !matches!(
                config.layout,
                NowPlayingLayout::Split | NowPlayingLayout::Lyrics
            )
        {
            config.layout = NowPlayingLayout::Lyrics;
        }

        // Designs with colours of their own cover the background: keep it cheap
        // (it still cross-fades the palette they use).
        let background = if config.layout.own_background() {
            NowPlayingBackground::Solid
        } else {
            config.background
        };
        let backdrop = self.render_backdrop(track.as_ref(), background, viewport, frame);
        // Designs with a backdrop of their own draw it over the regular one.
        let design_backdrop =
            self.design_backdrop(config.layout, track.as_ref(), position, viewport, frame, cx);
        let fullscreen = self.fullscreen_target(window);
        let header_state = self.header_snap(config.layout, config.background, fullscreen, cx);
        let header = snapshot::sync(
            &mut self.np.header,
            header_state,
            Self::header_view,
            frame,
            cx,
        )
        .cached(StyleRefinement::default().w_full().h(px(56.)).flex_none());
        let body: AnyElement = match config.layout {
            NowPlayingLayout::Split => {
                // A cached region: rebuilt when what it shows changes, not with
                // every lyrics frame.
                let side = Self::player_column_side(viewport);
                let key = self.region_key(track.as_ref(), position, viewport, side);
                let left = np_region::sync(
                    &mut self.np.column,
                    key,
                    |app, frame, window, cx| {
                        let track = app.playback.track.clone();
                        let position = app.clock.position(frame.now);
                        let viewport = app.np_viewport(window);
                        app.render_player_column(track.as_ref(), position, viewport, frame, cx)
                            .into_any_element()
                    },
                    frame,
                    cx,
                )
                .cached(StyleRefinement::default().w(side).h_full().flex_none());
                let lyrics = self.render_lyrics(
                    track.as_ref(),
                    position,
                    LYRICS_MAX_WIDTH,
                    display,
                    frame,
                    window,
                    cx,
                );
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .px(px(48.))
                    .gap(px(48.))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .justify_center()
                            .child(left),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .justify_center()
                            .child(lyrics),
                    )
                    .into_any_element()
            }
            NowPlayingLayout::Centered => self
                .render_centered(track.as_ref(), position, viewport, frame, cx)
                .into_any_element(),
            NowPlayingLayout::Lyrics => {
                let lyrics = self.render_lyrics(
                    track.as_ref(),
                    position,
                    LYRICS_WIDE_MAX_WIDTH,
                    display,
                    frame,
                    window,
                    cx,
                );
                let key = self.region_key(track.as_ref(), position, viewport, px(0.));
                let strip = np_region::sync(
                    &mut self.np.strip,
                    key,
                    |app, frame, _, cx| {
                        let track = app.playback.track.clone();
                        let position = app.clock.position(frame.now);
                        app.render_strip(track.as_ref(), position, frame, cx)
                            .into_any_element()
                    },
                    frame,
                    cx,
                )
                .cached(StyleRefinement::default().w_full().h(px(104.)).flex_none());
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        h_flex()
                            .flex_1()
                            .min_h_0()
                            .px(px(48.))
                            .justify_center()
                            .child(lyrics),
                    )
                    .child(strip)
                    .into_any_element()
            }
            NowPlayingLayout::Immersive => {
                self.render_immersive(track.as_ref(), position, viewport, frame, cx)
            }
            NowPlayingLayout::Card => {
                self.render_card(track.as_ref(), position, viewport, frame, cx)
            }
            NowPlayingLayout::Minimal => {
                self.render_minimal(track.as_ref(), position, viewport, frame, cx)
            }
            NowPlayingLayout::Editorial => {
                self.render_editorial(track.as_ref(), position, viewport, frame, cx)
            }
            NowPlayingLayout::Queue => {
                self.render_with_queue(track.as_ref(), position, viewport, frame, cx)
            }
            NowPlayingLayout::Turntable => {
                self.render_turntable(track.as_ref(), position, viewport, frame, cx)
            }
        };

        let (viz_behind, viz_over) = match self.np_visualizer_view(cx) {
            Some((view, true)) => (Some(view), None),
            Some((view, false)) => (None, Some(view)),
            None => (None, None),
        };
        div()
            .id("now-playing")
            .key_context("NowPlaying")
            .track_focus(&self.np.focus)
            .occlude()
            .absolute()
            .left_0()
            // Reduced motion: a fade instead of the slide up.
            .top(if crate::effects::ambient_motion() {
                top + viewport.height * (1. - reveal)
            } else {
                top
            })
            .w_full()
            .h(viewport.height)
            .overflow_hidden()
            .opacity(if crate::effects::ambient_motion() {
                (reveal * 1.8).min(1.)
            } else {
                reveal
            })
            .text_color(gpui::white())
            .child(backdrop)
            .children(design_backdrop)
            .children(viz_behind)
            .child(v_flex().absolute().inset_0().child(header).child(body))
            .children(viz_over)
            .into_any_element()
    }

    /// What the header shows (it is a cached [`Snapshot`] view: the menus and
    /// buttons are only rebuilt when this changes).
    fn header_snap(
        &self,
        layout: NowPlayingLayout,
        background: NowPlayingBackground,
        fullscreen: bool,
        cx: &App,
    ) -> HeaderSnap {
        let state = &self.playback;
        let keys = crate::shortcuts::display("toggle_fullscreen", cx);
        let fullscreen_tip = match (fullscreen, keys) {
            (false, Some(keys)) => t!("np.fullscreen_enter_keys", keys = keys),
            (true, Some(keys)) => t!("np.fullscreen_exit_keys", keys = keys),
            (false, None) => t!("np.fullscreen_enter").to_owned(),
            (true, None) => t!("np.fullscreen_exit").to_owned(),
        };
        HeaderSnap {
            layout,
            background,
            demo: self.engine.as_ref().is_some_and(|e| e.is_demo()),
            buffering: state.buffering && state.status == PlaybackStatus::Playing,
            // Formatted in `header_view` (only when the snapshot changes).
            speed: ((state.speed - 1.).abs() > 0.01).then_some(state.speed),
            preview: state.preview_only,
            source: crate::module_service::source_label(state.source.as_ref())
                .filter(|_| state.track.is_some())
                .map(SharedString::from),
            sleep: self.sleep_timer_label(),
            sleep_custom: self.settings.read(cx).sleep_custom(),
            fullscreen,
            fullscreen_tip: fullscreen_tip.into(),
            sound: crate::components::sound_menu::SoundSnap::new(self.settings.read(cx).settings()),
        }
    }

    fn header_view(s: &HeaderSnap, _: &mut Window, _: &mut App) -> AnyElement {
        let (layout, background) = (s.layout, s.background);
        let custom = s.sleep_custom;
        let armed = s.sleep.is_some();
        let sound = s.sound;
        h_flex()
            .size_full()
            .px_5()
            .gap_2()
            .child(
                Button::new("np-close")
                    .ghost()
                    .rounded(px(999.))
                    .icon(IconName::ChevronDown)
                    .tooltip(t!("np.close_tooltip"))
                    .on_click(|_, window, cx| {
                        update_app(cx, |app, cx| app.close_now_playing(window, cx));
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(white(0.6))
                    .child(t!("np.header")),
            )
            .when(s.demo, |this| {
                this.child(
                    Tag::secondary()
                        .small()
                        .rounded_full()
                        .child(t!("np.demo_clock")),
                )
            })
            .when(s.buffering, |this| {
                this.child(
                    Tag::secondary()
                        .small()
                        .rounded_full()
                        .child(t!("np.buffering")),
                )
            })
            .when_some(s.speed, |this, speed| {
                let speed = t!(
                    "np.speed",
                    speed = crate::i18n::format::decimal(speed.into(), 2)
                );
                this.child(Tag::secondary().small().rounded_full().child(speed))
            })
            .when(s.preview, |this| {
                this.child(
                    Tag::warning()
                        .small()
                        .rounded_full()
                        .child(t!("np.preview")),
                )
            })
            .when_some(s.source.clone(), |this, label| {
                this.child(Tag::secondary().small().rounded_full().child(label))
            })
            .child(
                Button::new("np-sleep")
                    .ghost()
                    .small()
                    .rounded(px(999.))
                    .icon(Lucide::Moon)
                    .selected(armed)
                    .when_some(s.sleep.clone(), |b, label| b.label(label))
                    .tooltip(t!("np.sleep_tooltip"))
                    .dropdown_menu(move |menu, _, _| {
                        MusicApp::sleep_timer_menu(menu, armed, custom)
                    }),
            )
            .child(
                Button::new("np-sound")
                    .ghost()
                    .rounded(px(999.))
                    .icon(Lucide::SlidersHorizontal)
                    .selected(sound.active())
                    .tooltip(t!("sound_menu.tooltip"))
                    .dropdown_menu(move |menu, window, cx| {
                        crate::components::sound_menu::sound_menu(menu, sound, window, cx)
                    }),
            )
            .child(
                Button::new("np-layout")
                    .ghost()
                    .rounded(px(999.))
                    .icon(Lucide::LayoutTemplate)
                    .tooltip(t!("np.layout_tooltip"))
                    .dropdown_menu(move |mut menu, _, _| {
                        menu = menu.label(t!("np.menu.design"));
                        for l in NowPlayingLayout::ALL {
                            menu = menu.item(
                                PopupMenuItem::new(l.title()).checked(l == layout).on_click(
                                    move |_, _, cx| {
                                        update_app(cx, |app, cx| {
                                            app.settings.update(cx, |p, cx| p.set_np_layout(l, cx))
                                        });
                                    },
                                ),
                            );
                        }
                        menu = menu.separator().label(t!("np.menu.background"));
                        for b in NowPlayingBackground::ALL {
                            menu = menu.item(
                                PopupMenuItem::new(b.title())
                                    .checked(b == background)
                                    .on_click(move |_, _, cx| {
                                        update_app(cx, |app, cx| {
                                            app.settings
                                                .update(cx, |p, cx| p.set_np_background(b, cx))
                                        });
                                    }),
                            );
                        }
                        menu
                    }),
            )
            .child(
                Button::new("np-fullscreen")
                    .ghost()
                    .rounded(px(999.))
                    .icon(if s.fullscreen {
                        Lucide::Minimize
                    } else {
                        Lucide::Maximize
                    })
                    .tooltip(s.fullscreen_tip.clone())
                    .on_click(|_, window, cx| {
                        update_app(cx, |app, cx| app.toggle_fullscreen(window, cx));
                    }),
            )
            .child(
                Button::new("np-settings")
                    .ghost()
                    .rounded(px(999.))
                    .icon(IconName::Settings)
                    .tooltip(t!("common.settings"))
                    .on_click(|_, window, cx| {
                        update_app(cx, |app, cx| app.open_settings(None, window, cx));
                    }),
            )
            .into_any_element()
    }

    fn render_backdrop(
        &mut self,
        track: Option<&Track>,
        style: NowPlayingBackground,
        viewport: gpui::Size<Pixels>,
        frame: &mut Frame,
    ) -> impl IntoElement + use<> {
        let ready = track.and_then(|t| match self.hires.get(&t.video_id) {
            Some(HiRes::Ready {
                backdrop,
                palette,
                glows,
                ..
            }) => Some((
                t.video_id.clone(),
                backdrop.clone(),
                *palette,
                glows.clone(),
            )),
            _ => None,
        });
        if let Some((id, backdrop, palette, glows)) = ready {
            // Swap in the new track's blurred art, cross-fading from the old one.
            if self.np.backdrop_id.as_deref() != Some(id.as_str()) {
                self.np.backdrop_prev = self.np.backdrop_cur.replace(backdrop);
                self.np.backdrop_id = Some(id.clone());
                self.np
                    .backdrop_fade
                    .animate_from(0., 1., BACKDROP_FADE, frame.now);
            }
            if self.np.colors_id.as_deref() != Some(id.as_str()) {
                self.np.colors_id = Some(id);
                self.np.glows = Some(glows);
                for (tween, color) in self.np.colors.iter_mut().zip(palette) {
                    tween.animate_to(color, BACKDROP_FADE, frame.now);
                }
            }
        }
        let base = div().absolute().inset_0().bg(gpui::rgb(0x141418));
        let layer = match style {
            NowPlayingBackground::Blur => {
                let fade = frame.get(&self.np.backdrop_fade);
                if fade >= 1. {
                    self.np.backdrop_prev = None;
                }
                let image = |image: Arc<RenderImage>| {
                    div()
                        .absolute()
                        .inset_0()
                        .child(img(image).size_full().object_fit(ObjectFit::Cover))
                };
                base.when_some(self.np.backdrop_prev.clone(), |this, i| {
                    this.child(image(i))
                })
                .when_some(self.np.backdrop_cur.clone(), |this, i| {
                    this.child(image(i).opacity(fade))
                })
            }
            NowPlayingBackground::Gradient => {
                let [a, b, _] = self.np.colors.each_ref().map(|t| frame.get(t));
                base.bg(linear_gradient(
                    160.,
                    linear_color_stop(deep(a, 0.30, 1.), 0.),
                    linear_color_stop(deep(b, 0.12, 1.), 1.),
                ))
            }
            NowPlayingBackground::Solid => {
                let a = frame.get(&self.np.colors[0]);
                base.bg(deep(a, 0.17, 1.))
            }
            NowPlayingBackground::Glow => {
                let a = frame.get(&self.np.colors[0]);
                // The phase only advances while playing, so a paused screen is still.
                let now = frame.now;
                if self.clock.is_running() && crate::effects::ambient_motion() {
                    if let Some(last) = self.np.glow_last {
                        self.np.glow_phase += (now - last).as_secs_f32().min(0.1);
                    }
                    self.np.glow_last = Some(now);
                    // The lights drift a fraction of a pixel per frame: 30 fps is
                    // plenty when nothing else animates.
                    frame.wake_in(GLOW_FRAME);
                } else {
                    self.np.glow_last = None;
                }
                let t = self.np.glow_phase;
                let (w, h) = (viewport.width, viewport.height);
                let diameter = w.max(h) * 0.9;
                let blob = |fx: f32, fy: f32, image: Arc<RenderImage>| {
                    img(image)
                        .absolute()
                        .left(w * fx - diameter * 0.5)
                        .top(h * fy - diameter * 0.5)
                        .size(diameter)
                };
                let positions = [
                    (
                        0.28 + 0.16 * (t * 0.21).sin(),
                        0.30 + 0.14 * (t * 0.17 + 1.).cos(),
                    ),
                    (
                        0.72 + 0.14 * (t * 0.19 + 2.).cos(),
                        0.62 + 0.16 * (t * 0.23).sin(),
                    ),
                    (
                        0.50 + 0.22 * (t * 0.13 + 4.).sin(),
                        0.85 + 0.10 * (t * 0.29 + 3.).cos(),
                    ),
                ];
                base.bg(deep(a, 0.06, 1.))
                    .when_some(self.np.glows.clone(), |this, glows| {
                        this.children(
                            glows
                                .into_iter()
                                .zip(positions)
                                .map(|(image, (x, y))| blob(x, y, image)),
                        )
                    })
            }
        };
        div()
            .absolute()
            .inset_0()
            .child(layer)
            // Dark gradient so white text stays readable on bright colours.
            .child(div().absolute().inset_0().bg(linear_gradient(
                180.,
                linear_color_stop(hsla(0., 0., 0., 0.2), 0.),
                linear_color_stop(hsla(0., 0., 0., 0.55), 1.),
            )))
    }

    fn artwork_view(&self, track: Option<&Track>, side: Pixels, cx: &mut Context<Self>) -> Div {
        self.artwork_shaped(track, side, Some((side * 0.035).max(px(6.))), cx)
    }

    /// The 544 px art of `track` (the list thumbnail while it loads).
    pub(crate) fn art_image(
        &self,
        track: Option<&Track>,
        cx: &mut Context<Self>,
    ) -> Option<Arc<Image>> {
        track.and_then(|t| match self.hires.get(&t.video_id) {
            Some(HiRes::Ready { art, .. }) => Some(art.clone()),
            _ => {
                let url = t.artwork_url(120)?;
                ArtworkCache::global(cx).update(cx, |cache, cx| cache.get(&url, cx))
            }
        })
    }

    /// Artwork `side` px square with corner `radius` (`None`: a circle), that
    /// shrinks a little while paused.
    pub(crate) fn artwork_shaped(
        &self,
        track: Option<&Track>,
        side: Pixels,
        radius: Option<Pixels>,
        cx: &mut Context<Self>,
    ) -> Div {
        let playing = self.playback.status == PlaybackStatus::Playing;
        let art = self.art_image(track, cx);
        let radius = radius.unwrap_or(side * 0.5);
        let inner = div()
            .rounded(radius)
            .shadow_2xl()
            .bg(white(0.08))
            .when_some(art, |this, art| {
                this.child(
                    img(art)
                        .size_full()
                        .rounded(radius)
                        .object_fit(ObjectFit::Cover),
                )
            });
        let scale = if playing { 1.0f32 } else { 0.9 };
        let shaped = if crate::effects::ambient_motion() {
            // GPUI's built-in stateful spring (keyed by element id): slightly
            // underdamped, so the art "pops" when playback resumes.
            inner
                .with_spring(
                    "np-artwork-scale",
                    SpringAnimation::new(SpringConfig::new(260., 22., 1.)).to(scale),
                    move |this, scale| this.size(side * scale),
                )
                .into_any_element()
        } else {
            // Reduced / no motion: the size just changes.
            inner.size(side * scale).into_any_element()
        };
        div()
            .size(side)
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .child(shaped)
    }

    /// Progress bar (click to seek) and elapsed / remaining time.
    fn progress_view(&self, position: Duration, cx: &mut Context<Self>) -> Div {
        let fill = AppColors::get(cx).np_progress;
        let bar = self.progress_bar(position, px(4.), white(0.22), fill, cx);
        v_flex()
            .w_full()
            .gap_1()
            .child(bar)
            .child(self.times_row(position, white(0.6)))
    }

    /// Played fraction of the current track at `position`.
    pub(crate) fn played(&self, position: Duration) -> f32 {
        let total = self.playback.duration.unwrap_or_default();
        if total.is_zero() {
            0.
        } else {
            (position.as_secs_f32() / total.as_secs_f32()).clamp(0., 1.)
        }
    }

    /// Elapsed and remaining time, at the two ends of a row.
    pub(crate) fn times_row(&self, position: Duration, color: Hsla) -> Div {
        let total = self.playback.duration.unwrap_or_default();
        h_flex()
            .w_full()
            .justify_between()
            .text_xs()
            .text_color(color)
            .child(format_duration(position.as_secs()))
            .child(if total.is_zero() {
                "--:--".to_owned()
            } else {
                format!(
                    "-{}",
                    format_duration(total.saturating_sub(position).as_secs())
                )
            })
    }

    /// A `thickness` px progress line (thicker on hover), click to seek.
    pub(crate) fn progress_bar(
        &self,
        position: Duration,
        thickness: Pixels,
        rail: Hsla,
        progress_fill: Hsla,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let total = self.playback.duration.unwrap_or_default();
        let frac = self.played(position);
        let bounds_cell = self.np.progress_bounds.clone();
        div()
            .id("np-progress")
            .group("np-progress")
            .w_full()
            .h(px(18.))
            .flex()
            .items_center()
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    let b = this.np.progress_bounds.get();
                    if b.size.width > px(0.) && !total.is_zero() {
                        let f = ((ev.position.x - b.left()) / b.size.width).clamp(0., 1.);
                        this.seek_to(total.mul_f32(f), cx);
                    }
                }),
            )
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(thickness)
                    .group_hover("np-progress", move |s| s.h(thickness + px(2.)))
                    .rounded_full()
                    .bg(rail)
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .h_full()
                            .w(relative(frac))
                            .rounded_full()
                            .bg(progress_fill),
                    )
                    .child(
                        canvas(move |bounds, _, _| bounds_cell.set(bounds), |_, _, _, _| {})
                            .absolute()
                            .size_full(),
                    ),
            )
    }

    /// Previous / play-pause / next; `scale` 1 = the full-size buttons.
    pub(crate) fn transport_view(
        &mut self,
        scale: f32,
        frame: &mut Frame,
        cx: &mut Context<Self>,
    ) -> Div {
        let status = self.playback.status.clone();
        let playing = status == PlaybackStatus::Playing;
        let busy = status == PlaybackStatus::Loading || (playing && self.playback.buffering);
        // Play/pause: the two icons rotate + scale + fade through each other.
        self.np
            .play_morph
            .animate_to(if playing { 1. } else { 0. }, ICON_MORPH, frame.now);
        let morph = frame.get(&self.np.play_morph);
        let icon = |name: IconName, shown: f32, turn: f32| {
            let s = 0.35 + 0.65 * shown;
            svg()
                .absolute()
                .size(px(30. * scale))
                .path(name.path())
                .text_color(hsla(0., 0., 0.08, shown))
                .with_transformation(Transformation::scale(size(s, s)).with_rotation(radians(turn)))
        };
        let can_toggle = self.engine.is_some()
            && matches!(
                status,
                PlaybackStatus::Playing | PlaybackStatus::Paused | PlaybackStatus::Ended
            );
        let play_button = div()
            .id("np-play")
            .size(px(68. * scale))
            .flex_none()
            .rounded_full()
            .bg(white(if can_toggle { 0.95 } else { 0.4 }))
            .shadow_lg()
            .flex()
            .items_center()
            .justify_center()
            .when(can_toggle, |this| {
                this.cursor_pointer()
                    .hover(|s| s.bg(white(0.82)))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_playback(cx)))
            })
            .when(busy, |this| {
                this.child(Spinner::new().large().color(hsla(0., 0., 0.1, 1.)))
            })
            .when(!busy, |this| {
                this.child(icon(IconName::Play, 1. - morph, -morph * 1.6))
                    .child(icon(IconName::Pause, morph, (1. - morph) * 1.6))
            });

        // Previous / next: white glyphs on a translucent circle.
        let skip = |id: &'static str, icon: AppIcon, enabled: bool| {
            div()
                .id(id)
                .size(px(48. * scale))
                .flex_none()
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .when(enabled, |this| {
                    this.cursor_pointer().hover(|s| s.bg(white(0.14)))
                })
                .child(
                    svg()
                        .size(px(26. * scale))
                        .path(icon.path())
                        .text_color(white(if enabled { 0.92 } else { 0.35 })),
                )
        };
        let has_track = self.playback.track.is_some();
        let has_next = self.queue.has_next();
        h_flex()
            .justify_center()
            .gap(px(28. * scale))
            .child(
                skip("np-prev", AppIcon::Previous, has_track).when(has_track, |this| {
                    this.on_click(cx.listener(|this, _, _, cx| this.previous_track(cx)))
                }),
            )
            .child(play_button)
            .child(
                skip("np-next", AppIcon::Next, has_next).when(has_next, |this| {
                    this.on_click(cx.listener(|this, _, _, cx| this.next_track(cx)))
                }),
            )
    }

    pub(crate) fn up_next_view(&self) -> Option<Div> {
        self.queue.peek_next().map(|next| {
            h_flex()
                .w_full()
                .justify_center()
                .gap_1()
                .text_sm()
                .child(
                    div()
                        .flex_none()
                        .text_color(white(0.45))
                        .child(t!("np.up_next")),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(white(0.7))
                        .child(format!("{} · {}", next.title, next.artists)),
                )
        })
    }

    pub(crate) fn titles(track: Option<&Track>) -> (SharedString, SharedString) {
        match track {
            Some(t) => (t.title.clone().into(), t.artists.clone().into()),
            None => (t!("common.not_playing").into(), "".into()),
        }
    }

    /// Width of the split layout's player column.
    fn player_column_side(viewport: gpui::Size<Pixels>) -> Pixels {
        // Half the content row (minus padding/gap), and leave room for text below.
        let half_width = (viewport.width - px(144.)) * 0.5;
        (viewport.height * 0.46)
            .min(half_width * 0.9)
            .clamp(px(120.), px(400.))
    }

    /// What the player column / strip region shows (see `np_region.rs`).
    fn region_key(
        &self,
        track: Option<&Track>,
        position: Duration,
        viewport: gpui::Size<Pixels>,
        side: Pixels,
    ) -> RegionKey {
        let state = &self.playback;
        RegionKey {
            track: track.map(|t| t.video_id.clone()),
            status: match state.status {
                PlaybackStatus::Idle => 0,
                PlaybackStatus::Loading => 1,
                PlaybackStatus::Playing => 2,
                PlaybackStatus::Paused => 3,
                PlaybackStatus::Ended => 4,
                PlaybackStatus::Error(_) => 5,
            },
            buffering: state.buffering,
            second: position.as_secs(),
            progress: (self.played(position) * 1000.) as u32,
            liked: track.is_some_and(|t| crate::store::is_liked(&t.video_id)),
            next: self.queue.peek_next().map(|t| t.video_id.clone()),
            has_art: track
                .is_some_and(|t| matches!(self.hires.get(&t.video_id), Some(HiRes::Ready { .. }))),
            width: viewport.width.as_f32() as i32,
            height: viewport.height.as_f32() as i32,
            extra: side.as_f32() as u64
                | u64::from(self.engine.is_some()) << 32
                | u64::from(self.queue.has_next()) << 33
                | u64::from(track.is_some_and(|t| crate::store::is_disliked(&t.video_id))) << 34,
        }
    }

    /// Split layout: artwork, title, progress, transport, up next.
    pub(crate) fn render_player_column(
        &mut self,
        track: Option<&Track>,
        position: Duration,
        viewport: gpui::Size<Pixels>,
        frame: &mut Frame,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let side = Self::player_column_side(viewport);
        let (title, artists) = Self::titles(track);
        v_flex()
            .w(side)
            .h_full()
            .flex_none()
            .justify_center()
            .gap_4()
            .child(self.artwork_view(track, side, cx))
            .child(
                h_flex()
                    .w_full()
                    .pt_2()
                    .gap_2()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::BOLD)
                                    .line_clamp(2)
                                    .child(title),
                            )
                            .child(div().text_color(white(0.65)).truncate().child(artists)),
                    )
                    // Dislike, then like (library + YouTube Music when signed in).
                    .children(track.filter(|t| !t.is_local()).map(|t| {
                        crate::components::dislike::dislike_icon_button(
                            "np-dislike",
                            t,
                            px(36.),
                            white(0.8),
                            white(0.12),
                            cx,
                        )
                    }))
                    .children(track.map(|t| like_icon_button("np-like", t, px(36.), cx))),
            )
            .child(self.progress_view(position, cx))
            .child(self.transport_view(1., frame, cx))
            .children(self.up_next_view())
    }

    /// Big-artwork layout: everything centered, the current lyric line below.
    fn render_centered(
        &mut self,
        track: Option<&Track>,
        position: Duration,
        viewport: gpui::Size<Pixels>,
        frame: &mut Frame,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let side = (viewport.height * 0.5)
            .min(viewport.width * 0.6)
            .clamp(px(140.), px(520.));
        let (title, artists) = Self::titles(track);
        // The line being sung (or the last one) and the next, as plain text.
        let current = self.lyric_snippet(track, position, frame);
        let width = (side * 1.15).min(viewport.width - px(64.));
        v_flex()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap_4()
            .pb_6()
            .child(self.artwork_view(track, side, cx))
            .child(
                v_flex()
                    .w(width)
                    .items_center()
                    .gap_0p5()
                    .child(
                        div()
                            .w_full()
                            .text_2xl()
                            .font_weight(FontWeight::BOLD)
                            .text_center()
                            .truncate()
                            .child(title),
                    )
                    .child(div().text_color(white(0.65)).truncate().child(artists)),
            )
            .child(div().w(width).child(self.progress_view(position, cx)))
            .child(self.transport_view(1., frame, cx))
            .when_some(current, |this, (now, next)| {
                this.child(
                    v_flex()
                        .w(width)
                        .items_center()
                        .gap_1()
                        .pt_2()
                        .child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_center()
                                .text_color(white(0.92))
                                .line_clamp(2)
                                .child(now.unwrap_or_else(|| "♪".into())),
                        )
                        .when_some(next, |this, next| {
                            this.child(
                                div()
                                    .text_sm()
                                    .text_center()
                                    .text_color(white(0.5))
                                    .truncate()
                                    .child(next),
                            )
                        }),
                )
            })
    }

    /// The synced line being sung (or the last one) and the next, as plain text;
    /// wakes the screen when the next one starts.
    pub(crate) fn lyric_snippet(
        &self,
        track: Option<&Track>,
        position: Duration,
        frame: &mut Frame,
    ) -> Option<(Option<String>, Option<String>)> {
        let mut next_start = None;
        let position = track.map_or(position, |t| {
            self.lyrics.lyrics_position(&t.video_id, position)
        });
        let current = track.and_then(|t| match self.lyrics.states.get(&t.video_id) {
            Some(LyricsState::Ready(p)) => p.synced.as_ref().map(|l| {
                let i = l.lines.partition_point(|line| line.start <= position);
                let now = i.checked_sub(1).map(|i| l.lines[i].text.clone());
                let next = l.lines.get(i).map(|line| line.text.clone());
                next_start = l.lines.get(i).map(|line| line.start);
                (now, next)
            }),
            _ => None,
        });
        if let Some(start) = next_start.filter(|_| self.clock.is_running()) {
            frame.wake_at(self.when_position(start, position, frame.now));
        }
        current
    }

    /// Lyrics-first layout: small artwork, title, progress and transport in one strip.
    pub(crate) fn render_strip(
        &mut self,
        track: Option<&Track>,
        position: Duration,
        frame: &mut Frame,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let (title, artists) = Self::titles(track);
        h_flex()
            .h(px(104.))
            .flex_none()
            .px(px(48.))
            .gap_5()
            .child(self.artwork_view(track, px(72.), cx))
            .child(
                v_flex()
                    .w(px(220.))
                    .flex_none()
                    .gap_0p5()
                    .child(div().font_weight(FontWeight::BOLD).truncate().child(title))
                    .child(
                        div()
                            .text_sm()
                            .text_color(white(0.65))
                            .truncate()
                            .child(artists),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(self.progress_view(position, cx)),
            )
            .child(self.transport_view(0.75, frame, cx))
    }

    #[allow(clippy::too_many_arguments)]
    fn render_lyrics(
        &mut self,
        track: Option<&Track>,
        position: Duration,
        max_width: f32,
        display: LyricsDisplay,
        frame: &mut Frame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pane = || {
            v_flex()
                .flex_1()
                .min_w_0()
                .max_w(px(max_width))
                .h_full()
                .relative()
        };
        let message = |title: &str, detail: Option<String>, spinner: bool| {
            pane()
                .child(
                    v_flex()
                        .size_full()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .when(spinner, |this| this.child(Spinner::new().color(white(0.7))))
                        .child(
                            div()
                                .text_2xl()
                                .font_weight(FontWeight::BOLD)
                                .text_color(white(0.75))
                                .child(title.to_owned()),
                        )
                        .when_some(detail, |this, detail| {
                            this.child(
                                div()
                                    .text_sm()
                                    .text_center()
                                    .text_color(white(0.5))
                                    .child(detail),
                            )
                        }),
                )
                .into_any_element()
        };
        let Some(track) = track else {
            return message(t!("lyrics.nothing_playing"), None, false);
        };
        let prepared = match self.lyrics.states.get(&track.video_id) {
            None | Some(LyricsState::Loading) => {
                return message(t!("lyrics.loading"), None, true);
            }
            Some(LyricsState::Missing { .. }) => {
                return message(
                    t!("lyrics.missing_title"),
                    Some(t!("lyrics.missing_detail").into()),
                    false,
                );
            }
            Some(LyricsState::Failed(e)) => {
                return message(t!("lyrics.failed"), Some(e.clone()), false);
            }
            Some(LyricsState::Ready(prepared)) => prepared.clone(),
        };

        // Translation (requested once per track and translator settings).
        let translation = if display.translate {
            self.translation_for(&track.video_id, &prepared, cx)
        } else {
            None
        };
        let translation_status = match (&translation, display.translate) {
            (_, false) => None,
            (None, true) => Some((t!("lyrics.translating").to_owned(), false)),
            (Some(Err(e)), true) => Some((t!("lyrics.translation_failed", error = e), true)),
            (Some(Ok(_)), true) => None,
        };
        let translated = match translation {
            Some(Ok(lines)) => Some(lines),
            _ => None,
        };
        let kind = match prepared.kind {
            SyncKind::Word => t!("lyrics.kind.word"),
            SyncKind::Line => t!("lyrics.kind.line"),
            SyncKind::Plain => t!("lyrics.kind.plain"),
        };
        let toolbar_state = ToolbarSnap {
            label: t!("lyrics.from", provider = prepared.provider, kind = kind).into(),
            sources: self.sources_snap(&track.video_id, &prepared.provider),
            synced: prepared.synced.is_some(),
            offset_ms: self.lyrics.tracks.offset_ms(&track.video_id),
            translate: display.translate,
            status: translation_status,
            can_share: prepared.synced.is_some() || !prepared.plain.is_empty(),
            selecting: self.np.list.selecting,
            selected: self.np.list.selected.len(),
        };
        let toolbar = snapshot::sync(
            &mut self.np.toolbar,
            toolbar_state,
            Self::toolbar_view,
            frame,
            cx,
        )
        .cached(StyleRefinement::default().w_full().h(px(32.)).flex_none());

        // The track's timing nudge: lyrics time runs that far behind (or ahead).
        let position = self.lyrics.lyrics_position(&track.video_id, position);
        let body = if prepared.synced.is_some() {
            self.synced_lines(
                track,
                &prepared,
                translated.as_deref(),
                position,
                display,
                frame,
                window,
                cx,
            )
        } else {
            self.plain_lines(&prepared, translated.as_deref(), display)
        };
        pane()
            .child(toolbar)
            .child(div().flex_1().min_h_0().relative().child(body))
            .into_any_element()
    }

    /// Provider · sync kind, translation status, share / translate / reload
    /// buttons (a cached [`Snapshot`] view), or the card selection bar.
    fn toolbar_view(s: &ToolbarSnap, _: &mut Window, _: &mut App) -> AnyElement {
        if s.selecting {
            return h_flex()
                .size_full()
                .px_3()
                .gap_2()
                .text_xs()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(white(0.75))
                        .child(match s.selected {
                            0 => tp!("lyrics.select_hint", MAX_SELECTED),
                            n => tp!("lyrics.selected", n),
                        }),
                )
                .child(
                    Button::new("np-share-cancel")
                        .ghost()
                        .xsmall()
                        .label(t!("common.cancel"))
                        .on_click(|_, _, cx| {
                            update_app(cx, |app, cx| app.set_lyrics_selecting(false, cx));
                        }),
                )
                .child(
                    Button::new("np-share-create")
                        .primary()
                        .xsmall()
                        .label(t!("lyrics.create_card"))
                        .disabled(s.selected == 0)
                        .on_click(|_, window, cx| {
                            update_app(cx, |app, cx| app.open_lyrics_card(window, cx));
                        }),
                )
                .into_any_element();
        }
        let translate = s.translate;
        h_flex()
            .size_full()
            .px_3()
            .gap_2()
            .text_xs()
            .text_color(white(0.5))
            .child(
                // A row, so the button keeps its own width (left-aligned, not stretched).
                h_flex().flex_1().min_w_0().overflow_hidden().child(
                    crate::np_lyrics::source_button(s.label.clone(), s.sources.clone()),
                ),
            )
            .when(s.synced, |this| {
                this.child(crate::np_lyrics::nudge_controls(s.offset_ms))
            })
            .when_some(s.status.clone(), |this, (text, failed)| {
                this.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(if failed { white(0.7) } else { white(0.5) })
                        .child(text),
                )
                .when(failed, |this| {
                    this.child(
                        Button::new("np-translate-retry")
                            .ghost()
                            .xsmall()
                            .label(t!("common.retry"))
                            .on_click(|_, _, cx| {
                                update_app(cx, |app, cx| app.retry_translation(cx));
                            }),
                    )
                })
            })
            .when(s.can_share, |this| {
                this.child(
                    Button::new("np-share")
                        .ghost()
                        .xsmall()
                        .rounded(px(999.))
                        .icon(Lucide::Share2)
                        .tooltip(t!("lyrics.share_tooltip"))
                        .on_click(|_, _, cx| {
                            update_app(cx, |app, cx| app.set_lyrics_selecting(true, cx));
                        }),
                )
            })
            .child(
                Button::new("np-translate")
                    .ghost()
                    .xsmall()
                    .rounded(px(999.))
                    .icon(Lucide::Languages)
                    .selected(translate)
                    .tooltip(if translate {
                        t!("lyrics.translation_on")
                    } else {
                        t!("lyrics.translation_off")
                    })
                    .on_click(move |_, _, cx| {
                        update_app(cx, |app, cx| {
                            app.settings
                                .update(cx, |p, cx| p.set_translate(!translate, cx));
                        });
                    }),
            )
            .child(
                Button::new("np-lyrics-reload")
                    .ghost()
                    .xsmall()
                    .rounded(px(999.))
                    .icon(Lucide::RotateCcw)
                    .tooltip(t!("lyrics.reload_tooltip"))
                    .on_click(|_, _, cx| {
                        update_app(cx, |app, cx| app.reload_lyrics(cx));
                    }),
            )
            .into_any_element()
    }

    /// Unsynced lyrics: a plain scrollable list (with romanization / translation).
    fn plain_lines(
        &self,
        prepared: &PreparedLyrics,
        translated: Option<&Vec<Option<SharedString>>>,
        display: LyricsDisplay,
    ) -> AnyElement {
        let size = display.text_size * 0.72;
        let align = |d: Div| match display.align {
            TextAlign::Left => d,
            TextAlign::Center => d.text_center(),
            TextAlign::Right => d.text_right(),
        };
        let rows = prepared.plain.iter().enumerate().map(|(i, text)| {
            let roman = prepared
                .romanized
                .get(i)
                .cloned()
                .flatten()
                .and_then(|(script, r)| display.romanize.enabled(script).then_some(r));
            let translation = translated.and_then(|t| t.get(i).cloned().flatten());
            align(v_flex().w_full().px_3().py(px(size * 0.22)))
                .child(align(
                    div()
                        .text_size(px(size))
                        .line_height(px(size * display.line_spacing))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(white(0.85))
                        .child(if text.trim().is_empty() {
                            SharedString::from(" ")
                        } else {
                            text.clone()
                        }),
                ))
                .children([roman, translation].into_iter().flatten().map(|sub| {
                    align(
                        div()
                            .text_size(px(size * 0.62))
                            .text_color(white(0.55))
                            .child(sub),
                    )
                }))
        });
        div()
            .id("lyrics-plain")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.np.plain_scroll)
            .child(v_flex().w_full().py_6().children(rows))
            .into_any_element()
    }
}

/// The heart on Now Playing: a plain icon button (the shared `like_button` is a
/// gpui-component `Button`, several times the per-frame layout cost).
pub(crate) fn like_icon_button(
    id: &'static str,
    track: &Track,
    side: Pixels,
    cx: &App,
) -> gpui::Stateful<Div> {
    let liked = crate::store::is_liked(&track.video_id);
    let (path, color) = if liked {
        (AppIcon::HeartFilled.path(), AppColors::get(cx).accent)
    } else {
        (Lucide::Heart.path(), white(0.8))
    };
    let tip = if liked {
        t!("common.remove_from_liked")
    } else {
        t!("common.like")
    };
    let track = track.clone();
    div()
        .id(id)
        .size(side)
        .flex_none()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|s| s.bg(white(0.12)))
        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tip).build(window, cx))
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            let track = track.clone();
            update_app(cx, |app, cx| app.toggle_like(track, window, cx));
        })
        .child(svg().size(side * 0.5).path(path).text_color(color))
}
