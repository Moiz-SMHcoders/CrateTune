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
    AnimationExt as _, AnyElement, Bounds, Context, Div, FocusHandle, FontStyle, FontWeight, Hsla,
    Image, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ObjectFit,
    ParentElement as _, Pixels, RenderImage, ScrollEvent, ScrollHandle, SharedString,
    SpringAnimation, SpringConfig, StatefulInteractiveElement as _, Styled as _, StyledImage as _,
    Transformation, Window, canvas, div, hsla, img, linear_color_stop, linear_gradient, point,
    prelude::FluentBuilder as _, px, radians, relative, size, svg,
};
use gpui_component::{
    IconName, IconNamed as _, Selectable as _, Sizable as _,
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
use crate::karaoke::{FEATHER, Fill, KaraokeLine};
use crate::lyrics::{LyricsState, PreparedLyrics};
use crate::settings::prefs::{
    LyricsAnimation, LyricsDisplay, NowPlayingBackground, NowPlayingLayout, TextAlign,
};
use crate::theme::AppColors;
use crate::util::off_thread;

/// Hi-res artwork + blurred backdrop entries kept (most recent tracks).
const HIRES_KEEP: usize = 8;

const OPEN: Spec = Spec::ms(320, ease_out_cubic);
const CLOSE: Spec = Spec::ms(260, ease_in_out_cubic);
const BACKDROP_FADE: Spec = Spec::ms(500, ease_in_out_cubic);
const ICON_MORPH: Spec = Spec::ms(260, ease_out_cubic);
const LINE_ACTIVATE: Spec = Spec::ms(420, ease_in_out_cubic);
const LINE_DIM: Spec = Spec::ms(500, ease_out_cubic);
const SCROLL: Spec = Spec::ms(460, ease_in_out_cubic);
/// Extra delay per line below the focused one: the list "ripples" into place.
const SCROLL_STAGGER: Duration = Duration::from_millis(30);
/// Wheel scrolling pauses auto-follow for this long.
const MANUAL_SCROLL_HOLD: Duration = Duration::from_secs(3);
/// Where the focused lyric line's center sits, as a fraction of the pane height.
const FOCUS_ANCHOR: f32 = 0.36;

/// Lyrics column width cap (long lines wrap, like Apple Music's lyrics column).
const LYRICS_MAX_WIDTH: f32 = 600.;
/// ...in the lyrics-first layout.
const LYRICS_WIDE_MAX_WIDTH: f32 = 920.;
/// Alpha of the not-yet-sung part of the active line.
const ACTIVE_DIM: f32 = 0.32;
/// Romanization / translation lines, relative to the lyric size.
const SUB_LINE_SCALE: f32 = 0.5;
const BACKGROUND_VOCALS_SCALE: f32 = 0.62;

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
        /// Dominant vibrant colour (dynamic theme), if the art has one.
        accent: Option<Hsla>,
    },
    Failed,
}

struct LineAnim {
    /// 0 = inactive, 1 = the line being sung.
    active: Tween<f32>,
    /// Alpha of unsung text (distance fade).
    dim: Tween<f32>,
    /// Paint-time vertical offset for staggered scrolling.
    shift: Tween<Pixels>,
}

impl LineAnim {
    fn new() -> Self {
        Self {
            active: Tween::new(0.),
            dim: Tween::new(0.3),
            shift: Tween::new(px(0.)),
        }
    }
}

/// How an animation style drives the karaoke renderer.
#[derive(Clone, Copy)]
struct Look {
    /// Scale of lines that aren't being sung.
    inactive_scale: f32,
    /// Lift of sung words, as a fraction of the font size.
    lift: f32,
    /// Per-word fill for word-synced lyrics (else the line lights as a whole).
    per_word: bool,
    sweep: bool,
    feather: f32,
    /// Halo alpha behind sung glyphs.
    glow: Option<f32>,
}

impl Look {
    fn of(animation: LyricsAnimation) -> Self {
        let karaoke = Self {
            inactive_scale: 0.9,
            lift: 0.08,
            per_word: true,
            sweep: true,
            feather: FEATHER,
            glow: None,
        };
        match animation {
            LyricsAnimation::None => Self {
                inactive_scale: 1.0,
                lift: 0.0,
                per_word: false,
                ..karaoke
            },
            LyricsAnimation::Fade => Self {
                inactive_scale: 0.96,
                lift: 0.0,
                sweep: false,
                ..karaoke
            },
            LyricsAnimation::Glow => Self {
                inactive_scale: 0.92,
                lift: 0.04,
                glow: Some(0.16),
                ..karaoke
            },
            LyricsAnimation::Slide => Self {
                lift: 0.18,
                feather: 0.2,
                ..karaoke
            },
            LyricsAnimation::Karaoke => karaoke,
            LyricsAnimation::Apple => Self {
                inactive_scale: 0.86,
                lift: 0.12,
                feather: 1.0,
                glow: Some(0.08),
                ..karaoke
            },
        }
    }
}

pub struct NowPlaying {
    pub open: bool,
    pub reveal: Tween<f32>,
    pub focus: FocusHandle,
    backdrop_prev: Option<Arc<RenderImage>>,
    backdrop_cur: Option<Arc<RenderImage>>,
    backdrop_id: Option<String>,
    backdrop_fade: Tween<f32>,
    /// Gradient / solid / glow colours, cross-faded between tracks.
    colors: [Tween<Hsla>; 3],
    colors_id: Option<String>,
    /// Glow background phase; advances only while playing.
    glow_phase: f32,
    glows: Option<[Arc<RenderImage>; 3]>,
    glow_last: Option<Instant>,
    play_morph: Tween<f32>,
    scroll: ScrollHandle,
    plain_scroll: ScrollHandle,
    lines_for: Option<String>,
    line_anims: Vec<LineAnim>,
    focus_line: Option<usize>,
    applied_scroll: Option<Pixels>,
    pub manual_until: Option<Instant>,
    hovered_line: Option<usize>,
    progress_bounds: Rc<Cell<Bounds<Pixels>>>,
}

impl NowPlaying {
    pub fn new(focus: FocusHandle) -> Self {
        let grey = Tween::new(hsla(0., 0., 0.12, 1.));
        Self {
            open: false,
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
            scroll: ScrollHandle::new(),
            plain_scroll: ScrollHandle::new(),
            lines_for: None,
            line_anims: Vec::new(),
            focus_line: None,
            applied_scroll: None,
            manual_until: None,
            hovered_line: None,
            progress_bounds: Rc::default(),
        }
    }
}

fn white(alpha: f32) -> Hsla {
    hsla(0., 0., 1., alpha)
}

/// `color`'s hue at a fixed (dark) lightness, saturation capped for readability.
fn deep(color: Hsla, lightness: f32, alpha: f32) -> Hsla {
    hsla(color.h, (color.s * 0.9).min(0.75), lightness, alpha)
}

impl MusicApp {
    pub fn open_now_playing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(track) = self.playback.track.clone() else {
            return;
        };
        self.ensure_lyrics(&track, cx);
        self.ensure_hires(&track, cx);
        self.np.open = true;
        self.np.reveal.animate_to(1., OPEN, Instant::now());
        window.focus(&self.np.focus, cx);
        cx.notify();
    }

    pub fn close_now_playing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.np.open = false;
        self.np.reveal.animate_to(0., CLOSE, Instant::now());
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub fn seek_to(&mut self, position: Duration, cx: &mut Context<Self>) {
        let Some(engine) = &mut self.engine else {
            return;
        };
        engine.seek(position);
        self.clock.seek(position, Instant::now());
        self.np.manual_until = None;
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
                        let accent = crate::theme::accent_from_artwork(bytes);
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
                            accent,
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
        let viewport = window.viewport_size();
        let track = self.playback.track.clone();
        let position = self.clock.position(frame.now);
        if self.clock.is_running() {
            // Lyrics, progress and time labels move continuously while playing.
            frame.keep_alive();
        }
        let (config, display) = {
            let s = self.settings.read(cx).settings();
            (s.now_playing, s.lyrics.display())
        };

        let backdrop = self.render_backdrop(track.as_ref(), config.background, viewport, frame);
        let header = self.render_np_header(config.layout, config.background, cx);
        let body: AnyElement = match config.layout {
            NowPlayingLayout::Split => {
                let left = self.render_player_column(track.as_ref(), position, viewport, frame, cx);
                let lyrics = self.render_lyrics(
                    track.as_ref(),
                    position,
                    viewport.height,
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
                    viewport.height,
                    LYRICS_WIDE_MAX_WIDTH,
                    display,
                    frame,
                    window,
                    cx,
                );
                let strip = self.render_strip(track.as_ref(), position, frame, cx);
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
        };

        div()
            .id("now-playing")
            .key_context("NowPlaying")
            .track_focus(&self.np.focus)
            .occlude()
            .absolute()
            .left_0()
            .top(viewport.height * (1. - reveal))
            .w_full()
            .h(viewport.height)
            .overflow_hidden()
            .opacity((reveal * 1.8).min(1.))
            .text_color(gpui::white())
            .child(backdrop)
            .child(v_flex().absolute().inset_0().child(header).child(body))
            .into_any_element()
    }

    fn render_np_header(
        &mut self,
        layout: NowPlayingLayout,
        background: NowPlayingBackground,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let demo = self.engine.as_ref().is_some_and(|e| e.is_demo());
        let state = &self.playback;
        let buffering = state.buffering && state.status == PlaybackStatus::Playing;
        let speed = (state.speed - 1.).abs() > 0.01;
        let speed_label = format!("{:.2}×", state.speed);
        let sleep = self.sleep_timer_label();
        let custom = self.settings.read(cx).sleep_custom();
        let armed = sleep.is_some();
        h_flex()
            .h(px(56.))
            .flex_none()
            .px_5()
            .gap_2()
            .child(
                Button::new("np-close")
                    .ghost()
                    .rounded(px(999.))
                    .icon(IconName::ChevronDown)
                    .tooltip("Close (Esc)")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.close_now_playing(window, cx)),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(white(0.6))
                    .child("NOW PLAYING"),
            )
            .when(demo, |this| {
                this.child(Tag::secondary().small().rounded_full().child("Demo clock"))
            })
            .when(buffering, |this| {
                this.child(Tag::secondary().small().rounded_full().child("Buffering…"))
            })
            .when(speed, |this| {
                this.child(Tag::secondary().small().rounded_full().child(speed_label))
            })
            .when(self.playback.preview_only, |this| {
                this.child(Tag::warning().small().rounded_full().child("Preview"))
            })
            .when_some(
                crate::module_service::source_label(self.playback.source.as_ref())
                    .filter(|_| self.playback.track.is_some()),
                |this, label| this.child(Tag::secondary().small().rounded_full().child(label)),
            )
            .child(
                Button::new("np-sleep")
                    .ghost()
                    .small()
                    .rounded(px(999.))
                    .icon(Lucide::Moon)
                    .selected(armed)
                    .when_some(sleep, |b, label| b.label(label))
                    .tooltip("Sleep timer")
                    .dropdown_menu(move |menu, _, _| {
                        MusicApp::sleep_timer_menu(menu, armed, custom)
                    }),
            )
            .child(
                Button::new("np-layout")
                    .ghost()
                    .rounded(px(999.))
                    .icon(Lucide::LayoutTemplate)
                    .tooltip("Layout and background")
                    .dropdown_menu(move |mut menu, _, _| {
                        menu = menu.label("Layout");
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
                        menu = menu.separator().label("Background");
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
                Button::new("np-settings")
                    .ghost()
                    .rounded(px(999.))
                    .icon(IconName::Settings)
                    .tooltip("Settings")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_settings(None, window, cx)),
                    ),
            )
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
                if self.clock.is_running() {
                    if let Some(last) = self.np.glow_last {
                        self.np.glow_phase += (now - last).as_secs_f32().min(0.1);
                    }
                    self.np.glow_last = Some(now);
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
        let playing = self.playback.status == PlaybackStatus::Playing;
        // Prefer the 544 px art; fall back to the list thumbnail while it loads.
        let art: Option<Arc<Image>> = track.and_then(|t| match self.hires.get(&t.video_id) {
            Some(HiRes::Ready { art, .. }) => Some(art.clone()),
            _ => {
                let url = t.artwork_url(120)?;
                ArtworkCache::global(cx).update(cx, |cache, cx| cache.get(&url, cx))
            }
        });
        let radius = (side * 0.035).max(px(6.));
        div()
            .size(side)
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
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
                    })
                    // GPUI's built-in stateful spring (keyed by element id): slightly
                    // underdamped, so the art "pops" when playback resumes.
                    .with_spring(
                        "np-artwork-scale",
                        SpringAnimation::new(SpringConfig::new(260., 22., 1.)).to(if playing {
                            1.0f32
                        } else {
                            0.9
                        }),
                        move |this, scale| this.size(side * scale),
                    ),
            )
    }

    /// Progress bar (click to seek) and elapsed / remaining time.
    fn progress_view(&self, position: Duration, cx: &mut Context<Self>) -> Div {
        let total = self.playback.duration.unwrap_or_default();
        let frac = if total.is_zero() {
            0.
        } else {
            (position.as_secs_f32() / total.as_secs_f32()).clamp(0., 1.)
        };
        let progress_fill = AppColors::get(cx).np_progress;
        let bounds_cell = self.np.progress_bounds.clone();
        let bar = div()
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
                    .h(px(4.))
                    .group_hover("np-progress", |s| s.h(px(6.)))
                    .rounded_full()
                    .bg(white(0.22))
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
            );
        let times = h_flex()
            .w_full()
            .justify_between()
            .text_xs()
            .text_color(white(0.6))
            .child(format_duration(position.as_secs()))
            .child(if total.is_zero() {
                "--:--".to_owned()
            } else {
                format!(
                    "-{}",
                    format_duration(total.saturating_sub(position).as_secs())
                )
            });
        v_flex().w_full().gap_1().child(bar).child(times)
    }

    /// Previous / play-pause / next; `scale` 1 = the full-size buttons.
    fn transport_view(&mut self, scale: f32, frame: &mut Frame, cx: &mut Context<Self>) -> Div {
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

    fn up_next_view(&self) -> Option<Div> {
        self.queue.peek_next().map(|next| {
            h_flex()
                .w_full()
                .justify_center()
                .gap_1()
                .text_sm()
                .child(div().flex_none().text_color(white(0.45)).child("Up next"))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(white(0.7))
                        .child(format!("{} · {}", next.title, next.artists)),
                )
        })
    }

    fn titles(track: Option<&Track>) -> (SharedString, SharedString) {
        match track {
            Some(t) => (t.title.clone().into(), t.artists.clone().into()),
            None => ("Not playing".into(), "".into()),
        }
    }

    /// Split layout: artwork, title, progress, transport, up next.
    fn render_player_column(
        &mut self,
        track: Option<&Track>,
        position: Duration,
        viewport: gpui::Size<Pixels>,
        frame: &mut Frame,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        // Half the content row (minus padding/gap), and leave room for text below.
        let half_width = (viewport.width - px(144.)) * 0.5;
        let side = (viewport.height * 0.46)
            .min(half_width * 0.9)
            .clamp(px(120.), px(400.));
        let (title, artists) = Self::titles(track);
        v_flex()
            .w(side)
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
                    // Like (library + YouTube Music when signed in).
                    .children(track.map(|t| {
                        crate::components::like::like_button("np-like", t, Some(white(0.8)), cx)
                    })),
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
        let current = track.and_then(|t| match self.lyrics.states.get(&t.video_id) {
            Some(LyricsState::Ready(p)) => p.synced.as_ref().map(|l| {
                let i = l.lines.partition_point(|line| line.start <= position);
                let now = i.checked_sub(1).map(|i| l.lines[i].text.clone());
                let next = l.lines.get(i).map(|line| line.text.clone());
                (now, next)
            }),
            _ => None,
        });
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

    /// Lyrics-first layout: small artwork, title, progress and transport in one strip.
    fn render_strip(
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
        viewport_height: Pixels,
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
            return message("Nothing playing", None, false);
        };
        let prepared = match self.lyrics.states.get(&track.video_id) {
            None | Some(LyricsState::Loading) => {
                return message("Loading lyrics…", None, true);
            }
            Some(LyricsState::Missing { .. }) => {
                return message(
                    "No lyrics",
                    Some("None of the enabled lyrics providers has this track.".into()),
                    false,
                );
            }
            Some(LyricsState::Failed(e)) => {
                return message("Couldn't load lyrics", Some(e.clone()), false);
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
            (None, true) => Some(("Translating…".to_owned(), false)),
            (Some(Err(e)), true) => Some((format!("Translation failed: {e}"), true)),
            (Some(Ok(_)), true) => None,
        };
        let translated = match translation {
            Some(Ok(lines)) => Some(lines),
            _ => None,
        };
        let toolbar = self.lyrics_toolbar(&prepared, display, translation_status, cx);

        let body = if prepared.synced.is_some() {
            self.synced_lines(
                track,
                &prepared,
                translated.as_deref(),
                position,
                viewport_height,
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

    /// Provider · sync kind, translation status, translate and reload buttons.
    fn lyrics_toolbar(
        &self,
        prepared: &PreparedLyrics,
        display: LyricsDisplay,
        translation_status: Option<(String, bool)>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let kind = match prepared.kind {
            SyncKind::Word => "Word-synced",
            SyncKind::Line => "Line-synced",
            SyncKind::Plain => "Unsynced",
        };
        h_flex()
            .h(px(32.))
            .flex_none()
            .px_3()
            .gap_2()
            .text_xs()
            .text_color(white(0.5))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(format!("{} · {kind}", prepared.provider)),
            )
            .when_some(translation_status, |this, (text, failed)| {
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
                            .label("Retry")
                            .on_click(cx.listener(|this, _, _, cx| this.retry_translation(cx))),
                    )
                })
            })
            .child(
                Button::new("np-translate")
                    .ghost()
                    .xsmall()
                    .rounded(px(999.))
                    .icon(Lucide::Languages)
                    .selected(display.translate)
                    .tooltip(if display.translate {
                        "Translation: on"
                    } else {
                        "Translation: off"
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let on = !display.translate;
                        this.settings.update(cx, |p, cx| p.set_translate(on, cx));
                    })),
            )
            .child(
                Button::new("np-lyrics-reload")
                    .ghost()
                    .xsmall()
                    .rounded(px(999.))
                    .icon(Lucide::RotateCcw)
                    .tooltip("Search the providers again")
                    .on_click(cx.listener(|this, _, _, cx| this.reload_lyrics(cx))),
            )
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

    /// Synced lyrics: staggered auto-scroll, karaoke lines, click to seek.
    #[allow(clippy::too_many_arguments)]
    fn synced_lines(
        &mut self,
        track: &Track,
        prepared: &Arc<PreparedLyrics>,
        translated: Option<&Vec<Option<SharedString>>>,
        position: Duration,
        viewport_height: Pixels,
        display: LyricsDisplay,
        frame: &mut Frame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(lyrics) = prepared.synced.as_ref() else {
            return div().into_any_element();
        };
        let now = frame.now;
        let np = &mut self.np;
        if np.lines_for.as_deref() != Some(&track.video_id) {
            np.lines_for = Some(track.video_id.clone());
            np.line_anims = lyrics.lines.iter().map(|_| LineAnim::new()).collect();
            np.focus_line = None;
            np.applied_scroll = None;
            np.hovered_line = None;
        }

        let lines = &lyrics.lines;
        let active = lyrics.active_line(position);
        // The line the list centers on: the active one, else the last one started.
        let focus = active
            .or_else(|| {
                lines
                    .partition_point(|l| l.start <= position)
                    .checked_sub(1)
            })
            .unwrap_or(0);

        // ---- scrolling: keep the focused line ~1/3 from the top, staggered ----
        // Child 0 is a spacer, so line i is child i + 1. Bounds are last frame's layout.
        let manual = np.manual_until.is_some_and(|until| until > now);
        let viewport = np.scroll.bounds();
        let target = np.scroll.bounds_for_item(focus + 1).map(|b| {
            b.top() - viewport.top() + b.size.height * 0.5 - viewport.size.height * FOCUS_ANCHOR
        });
        if !manual && let Some(target) = target {
            let from = if np.manual_until.take().is_some() {
                Some(-np.scroll.offset().y) // returning from a manual scroll
            } else if np.focus_line != Some(focus) {
                np.applied_scroll
            } else {
                None
            };
            if let Some(from) = from {
                let delta = target - from;
                for (i, anim) in np.line_anims.iter_mut().enumerate() {
                    let rank = (i + 1).saturating_sub(focus).min(12) as u32;
                    let current = anim.shift.value(now);
                    anim.shift.animate_from(
                        current + delta,
                        px(0.),
                        SCROLL.delayed(SCROLL_STAGGER * rank),
                        now,
                    );
                }
            }
            np.scroll.set_offset(point(px(0.), -target));
            np.applied_scroll = Some(target);
            np.focus_line = Some(focus);
        } else if !manual {
            // No layout yet (first frame): render once more to get bounds.
            frame.keep_alive();
        }

        let look = Look::of(display.animation);
        let word_synced = prepared.kind == SyncKind::Word;
        let size = display.text_size;
        let mut font = window.text_style().font();
        font.weight = FontWeight::BOLD;
        let mut sub_font = font.clone();
        sub_font.weight = FontWeight::MEDIUM;
        let mut bg_font = font.clone();
        bg_font.weight = FontWeight::SEMIBOLD;
        bg_font.style = FontStyle::Italic;
        let lift = px(size * look.lift);
        let base_align = display.align;
        let sub_size = (size * SUB_LINE_SCALE).max(12.);
        let bg_size = size * BACKGROUND_VOCALS_SCALE;

        let mut children: Vec<AnyElement> = Vec::with_capacity(lines.len() + 2);
        children.push(div().h(viewport_height).into_any_element());
        for (i, line) in lines.iter().enumerate() {
            let is_active = active == Some(i);
            let anim = &mut np.line_anims[i];
            let distance = i.abs_diff(focus);
            let dim_target = if is_active {
                if look.per_word && word_synced {
                    ACTIVE_DIM
                } else {
                    0.9
                }
            } else if np.hovered_line == Some(i) {
                0.75
            } else {
                // Apple-Music-style depth: lines fade with distance from the focus.
                (0.44 * 0.78f32.powi(distance.saturating_sub(1) as i32)).max(0.1)
            };
            anim.active
                .animate_to(if is_active { 1. } else { 0. }, LINE_ACTIVATE, now);
            anim.dim.animate_to(dim_target, LINE_DIM, now);
            let act = frame.get(&anim.active);
            let dim = frame.get(&anim.dim);
            let shift = frame.get(&anim.shift);
            let past = i < focus || (i == focus && position >= line.end);

            let fill = if is_active && look.per_word && word_synced {
                Fill::Words(
                    (0..line.words.len())
                        .map(|w| line.word_progress(w, position))
                        .collect(),
                )
            } else if is_active || past {
                Fill::All
            } else {
                Fill::None
            };
            let info = &prepared.lines[i];
            // Duets: the other singer's lines go to the other side.
            let align = match (
                base_align,
                info.secondary_singer && display.show_background_vocals,
            ) {
                (TextAlign::Left, true) => TextAlign::Right,
                (TextAlign::Right, true) => TextAlign::Left,
                (a, _) => a,
            }
            .factor();
            let scale = look.inactive_scale + (1. - look.inactive_scale) * act;
            let lit = white(dim + (1. - dim) * act);
            let glow = look.glow.map(|a| white(a * act));
            let karaoke = |text: SharedString,
                           word_starts: Arc<[usize]>,
                           fill: Fill,
                           font: gpui::Font,
                           font_size: f32,
                           lit: Hsla,
                           dim: Hsla,
                           lift: Pixels,
                           glow: Option<Hsla>| KaraokeLine {
                text,
                word_starts,
                fill,
                font,
                font_size: px(font_size),
                line_height: px(font_size * display.line_spacing),
                scale,
                lit,
                dim,
                lift,
                shift,
                align,
                sweep: look.sweep,
                feather: look.feather,
                glow,
            };
            let mut row = v_flex()
                .id(("lyric", i))
                .w_full()
                .px_3()
                .py(px(size * 0.26))
                .cursor_pointer()
                .child(karaoke(
                    info.text.clone(),
                    info.word_starts.clone(),
                    fill,
                    font.clone(),
                    size,
                    lit,
                    white(dim),
                    lift * act,
                    glow,
                ));
            if display.show_background_vocals
                && let Some((bg, text, starts)) = &info.background
            {
                let bg_fill = if is_active && look.per_word && word_synced {
                    Fill::Words(
                        (0..bg.words.len())
                            .map(|w| bg.word_progress(w, position))
                            .collect(),
                    )
                } else if is_active || past {
                    Fill::All
                } else {
                    Fill::None
                };
                row = row.child(karaoke(
                    text.clone(),
                    starts.clone(),
                    bg_fill,
                    bg_font.clone(),
                    bg_size,
                    white((dim + (1. - dim) * act) * 0.8),
                    white(dim * 0.8),
                    px(0.),
                    None,
                ));
            }
            let sub_alpha = (dim + 0.12 + 0.5 * act).min(0.85);
            let roman = prepared
                .romanized
                .get(i)
                .cloned()
                .flatten()
                .and_then(|(script, r)| display.romanize.enabled(script).then_some(r));
            let translation = if display.translate {
                translated
                    .and_then(|t| t.get(i).cloned().flatten())
                    .or_else(|| info.provided_translation.clone())
            } else {
                None
            };
            for sub in [roman, translation].into_iter().flatten() {
                row = row.child(div().pt(px(2.)).child(karaoke(
                    sub,
                    Arc::from([0usize]),
                    Fill::All,
                    sub_font.clone(),
                    sub_size,
                    white(sub_alpha),
                    white(sub_alpha),
                    px(0.),
                    None,
                )));
            }
            let start = line.start;
            children.push(
                row.on_click(cx.listener(move |this, _, _, cx| this.seek_to(start, cx)))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if *hovered {
                            this.np.hovered_line = Some(i);
                        } else if this.np.hovered_line == Some(i) {
                            this.np.hovered_line = None;
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        children.push(div().h(viewport_height).into_any_element());

        div()
            .id("lyrics")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.np.scroll)
            .on_scroll(cx.listener(|this, _: &ScrollEvent, _, cx| {
                this.np.manual_until = Some(Instant::now() + MANUAL_SCROLL_HOLD);
                cx.notify();
            }))
            .children(children)
            .into_any_element()
    }
}
