//! Full-window "Now Playing" screen: blurred artwork backdrop, artwork that
//! breathes with play state, progress/transport, and a synced, karaoke-style
//! lyrics pane with Apple-Music-like staggered scrolling.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnimationExt as _, AnyElement, Bounds, Context, FocusHandle, FontWeight, Hsla, Image,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ObjectFit,
    ParentElement as _, Pixels, RenderImage, ScrollEvent, ScrollHandle, SharedString,
    SpringAnimation, SpringConfig, StatefulInteractiveElement as _, Styled as _, StyledImage as _,
    Transformation, Window, canvas, div, hsla, img, linear_color_stop, linear_gradient, point,
    prelude::FluentBuilder as _, px, radians, relative, size, svg,
};
use gpui_component::{
    IconName, IconNamed as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    spinner::Spinner,
    tag::Tag,
    v_flex,
};
use music_core::{Lyrics, PlaybackStatus, Track, format_duration};

use crate::anim::{Frame, Spec, Tween, ease_in_out_cubic, ease_out_cubic};
use crate::app::MusicApp;
use crate::components::artwork::ArtworkCache;
use crate::icons::AppIcon;
use crate::karaoke::{self, Fill, KaraokeLine};
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

const LYRIC_SIZE: f32 = 34.;
const LYRIC_LINE_HEIGHT: f32 = 42.;
/// Lyrics column width cap (long lines wrap, like Apple Music's lyrics column).
const LYRICS_MAX_WIDTH: f32 = 600.;
const INACTIVE_SCALE: f32 = 0.9;
/// Alpha of the not-yet-sung part of the active line.
const ACTIVE_DIM: f32 = 0.32;

pub enum LyricsState {
    Loading,
    Missing,
    Failed(String),
    Ready(Arc<PreparedLyrics>),
}

pub struct PreparedLyrics {
    lyrics: Lyrics,
    lines: Vec<(SharedString, Arc<[usize]>)>,
}

impl PreparedLyrics {
    fn new(lyrics: Lyrics) -> Self {
        let lines = lyrics
            .lines
            .iter()
            .map(|l| {
                (
                    l.text.clone().into(),
                    karaoke::word_starts(&l.text, &l.words),
                )
            })
            .collect();
        Self { lyrics, lines }
    }
}

/// Large artwork, its pre-blurred backdrop and accent colour, per video id.
pub enum HiRes {
    Loading,
    Ready {
        art: Arc<Image>,
        backdrop: Arc<RenderImage>,
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

pub struct NowPlaying {
    pub open: bool,
    pub reveal: Tween<f32>,
    pub focus: FocusHandle,
    backdrop_prev: Option<Arc<RenderImage>>,
    backdrop_cur: Option<Arc<RenderImage>>,
    backdrop_id: Option<String>,
    backdrop_fade: Tween<f32>,
    play_morph: Tween<f32>,
    scroll: ScrollHandle,
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
        Self {
            open: false,
            reveal: Tween::new(0.),
            focus,
            backdrop_prev: None,
            backdrop_cur: None,
            backdrop_id: None,
            backdrop_fade: Tween::new(1.),
            play_morph: Tween::new(0.),
            scroll: ScrollHandle::new(),
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

    /// Fetches lyrics for `track` once (LRCLIB, blocking → background executor).
    pub fn ensure_lyrics(&mut self, track: &Track, cx: &mut Context<Self>) {
        // Failed fetches are retried the next time the track is played / opened.
        if self
            .lyrics
            .get(&track.video_id)
            .is_some_and(|l| !matches!(l, LyricsState::Failed(_)))
        {
            return;
        }
        self.lyrics
            .insert(track.video_id.clone(), LyricsState::Loading);
        let track = track.clone();
        cx.spawn(async move |this, cx| {
            let id = track.video_id.clone();
            let result = off_thread(move || music_core::fetch_lyrics(&track)).await;
            this.update(cx, |this, cx| {
                let state = match result {
                    Ok(Some(lyrics)) => LyricsState::Ready(Arc::new(PreparedLyrics::new(lyrics))),
                    Ok(None) => LyricsState::Missing,
                    Err(e) => LyricsState::Failed(e.to_string()),
                };
                this.lyrics.insert(id, state);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Fetches the 544 px artwork (shared cache, fetch threads) and blurs it +
    /// extracts its accent on a worker thread, once per track; keeps the last
    /// [`HIRES_KEEP`] tracks.
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
                        backdrop.map(|b| (b, accent))
                    })
                    .await;
                    match result {
                        Ok((backdrop, accent)) => HiRes::Ready {
                            art,
                            backdrop,
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

        let backdrop = self.render_backdrop(track.as_ref(), frame);
        let left = self.render_player_column(track.as_ref(), position, viewport, frame, cx);
        let lyrics =
            self.render_lyrics(track.as_ref(), position, viewport.height, frame, window, cx);

        let demo = self.engine.as_ref().is_some_and(|e| e.is_demo());
        let header = h_flex()
            .h(px(56.))
            .flex_none()
            .px_5()
            .gap_3()
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
            .when(self.playback.preview_only, |this| {
                this.child(Tag::warning().small().rounded_full().child("Preview"))
            })
            .child(
                Button::new("np-settings")
                    .ghost()
                    .rounded(px(999.))
                    .icon(IconName::Settings)
                    .tooltip("Settings")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_settings(None, window, cx)),
                    ),
            );

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
            .child(
                v_flex().absolute().inset_0().child(header).child(
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
                        ),
                ),
            )
            .into_any_element()
    }

    fn render_backdrop(
        &mut self,
        track: Option<&Track>,
        frame: &mut Frame,
    ) -> impl IntoElement + use<> {
        // Swap in the new track's blurred art once it's ready, cross-fading from the old one.
        if let Some(track) = track
            && self.np.backdrop_id.as_deref() != Some(&track.video_id)
            && let Some(HiRes::Ready { backdrop, .. }) = self.hires.get(&track.video_id)
        {
            self.np.backdrop_prev = self.np.backdrop_cur.replace(backdrop.clone());
            self.np.backdrop_id = Some(track.video_id.clone());
            self.np
                .backdrop_fade
                .animate_from(0., 1., BACKDROP_FADE, frame.now);
        }
        let fade = frame.get(&self.np.backdrop_fade);
        if fade >= 1. {
            self.np.backdrop_prev = None;
        }
        let layer = |image: Arc<RenderImage>| {
            div()
                .absolute()
                .inset_0()
                .child(img(image).size_full().object_fit(ObjectFit::Cover))
        };
        div()
            .absolute()
            .inset_0()
            .bg(gpui::rgb(0x141418))
            .when_some(self.np.backdrop_prev.clone(), |this, image| {
                this.child(layer(image))
            })
            .when_some(self.np.backdrop_cur.clone(), |this, image| {
                this.child(layer(image).opacity(fade))
            })
            // Dark gradient so white text stays readable on bright artwork.
            .child(div().absolute().inset_0().bg(linear_gradient(
                180.,
                linear_color_stop(hsla(0., 0., 0., 0.2), 0.),
                linear_color_stop(hsla(0., 0., 0., 0.55), 1.),
            )))
    }

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
        let status = self.playback.status.clone();
        let playing = status == PlaybackStatus::Playing;

        // Prefer the 544 px art; fall back to the list thumbnail while it loads.
        let art: Option<Arc<Image>> = track.and_then(|t| match self.hires.get(&t.video_id) {
            Some(HiRes::Ready { art, .. }) => Some(art.clone()),
            _ => {
                let url = t.artwork_url(120)?;
                ArtworkCache::global(cx).update(cx, |cache, cx| cache.get(&url, cx))
            }
        });
        let progress_fill = AppColors::get(cx).np_progress;
        let artwork = div()
            .size(side)
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .rounded(px(14.))
                    .shadow_2xl()
                    .bg(white(0.08))
                    .when_some(art, |this, art| {
                        this.child(
                            img(art)
                                .size_full()
                                .rounded(px(14.))
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
            );

        let (title, artists): (SharedString, SharedString) = match track {
            Some(t) => (t.title.clone().into(), t.artists.clone().into()),
            None => ("Not playing".into(), "".into()),
        };
        let total = self.playback.duration.unwrap_or_default();
        let frac = if total.is_zero() {
            0.
        } else {
            (position.as_secs_f32() / total.as_secs_f32()).clamp(0., 1.)
        };

        let bounds_cell = self.np.progress_bounds.clone();
        let progress = div()
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

        // Play/pause: the two icons rotate + scale + fade through each other.
        self.np
            .play_morph
            .animate_to(if playing { 1. } else { 0. }, ICON_MORPH, frame.now);
        let morph = frame.get(&self.np.play_morph);
        let icon = |name: IconName, shown: f32, turn: f32| {
            let scale = 0.35 + 0.65 * shown;
            svg()
                .absolute()
                .size(px(30.))
                .path(name.path())
                .text_color(hsla(0., 0., 0.08, shown))
                .with_transformation(
                    Transformation::scale(size(scale, scale)).with_rotation(radians(turn)),
                )
        };
        let can_toggle = self.engine.is_some()
            && matches!(
                status,
                PlaybackStatus::Playing | PlaybackStatus::Paused | PlaybackStatus::Ended
            );
        let play_button = div()
            .id("np-play")
            .size(px(68.))
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
            .when(status == PlaybackStatus::Loading, |this| {
                this.child(Spinner::new().large().color(hsla(0., 0., 0.1, 1.)))
            })
            .when(status != PlaybackStatus::Loading, |this| {
                this.child(icon(IconName::Play, 1. - morph, -morph * 1.6))
                    .child(icon(IconName::Pause, morph, (1. - morph) * 1.6))
            });

        // Previous / next: white glyphs on a translucent circle.
        let skip = |id: &'static str, icon: AppIcon, enabled: bool| {
            div()
                .id(id)
                .size(px(48.))
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
                        .size(px(26.))
                        .path(icon.path())
                        .text_color(white(if enabled { 0.92 } else { 0.35 })),
                )
        };
        let has_track = track.is_some();
        let transport = h_flex()
            .w_full()
            .justify_center()
            .gap(px(28.))
            .child(
                skip("np-prev", AppIcon::Previous, has_track).when(has_track, |this| {
                    this.on_click(cx.listener(|this, _, _, cx| this.previous_track(cx)))
                }),
            )
            .child(play_button)
            .child(
                skip("np-next", AppIcon::Next, self.queue.has_next())
                    .when(self.queue.has_next(), |this| {
                        this.on_click(cx.listener(|this, _, _, cx| this.next_track(cx)))
                    }),
            );
        let up_next = self.queue.peek_next().map(|next| {
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
        });

        v_flex()
            .w(side)
            .flex_none()
            .justify_center()
            .gap_4()
            .child(artwork)
            .child(
                v_flex()
                    .w_full()
                    .gap_0p5()
                    .pt_2()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::BOLD)
                            .line_clamp(2)
                            .child(title),
                    )
                    .child(div().text_color(white(0.65)).truncate().child(artists)),
            )
            .child(v_flex().w_full().gap_1().child(progress).child(times))
            .child(transport)
            .children(up_next)
    }

    fn render_lyrics(
        &mut self,
        track: Option<&Track>,
        position: Duration,
        viewport_height: Pixels,
        frame: &mut Frame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pane = || {
            div()
                .flex_1()
                .min_w_0()
                .max_w(px(LYRICS_MAX_WIDTH))
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
                            this.child(div().text_sm().text_color(white(0.5)).child(detail))
                        }),
                )
                .into_any_element()
        };
        let Some(track) = track else {
            return message("Nothing playing", None, false);
        };
        let prepared = match self.lyrics.get(&track.video_id) {
            None | Some(LyricsState::Loading) => {
                return message("Loading lyrics…", None, true);
            }
            Some(LyricsState::Missing) => {
                return message(
                    "No synced lyrics",
                    Some("LRCLIB has no time-synced lyrics for this track.".into()),
                    false,
                );
            }
            Some(LyricsState::Failed(e)) => {
                return message("Couldn't load lyrics", Some(e.clone()), false);
            }
            Some(LyricsState::Ready(prepared)) => prepared.clone(),
        };

        let now = frame.now;
        let np = &mut self.np;
        if np.lines_for.as_deref() != Some(&track.video_id) {
            np.lines_for = Some(track.video_id.clone());
            np.line_anims = prepared.lines.iter().map(|_| LineAnim::new()).collect();
            np.focus_line = None;
            np.applied_scroll = None;
            np.hovered_line = None;
        }

        let lines = &prepared.lyrics.lines;
        let active = prepared.lyrics.active_line(position);
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

        let mut font = window.text_style().font();
        font.weight = FontWeight::BOLD;
        let lift = px(LYRIC_SIZE * 0.08);

        let mut children: Vec<AnyElement> = Vec::with_capacity(lines.len() + 2);
        children.push(div().h(viewport_height).into_any_element());
        for (i, line) in lines.iter().enumerate() {
            let is_active = active == Some(i);
            let anim = &mut np.line_anims[i];
            let distance = i.abs_diff(focus);
            let dim_target = if is_active {
                ACTIVE_DIM
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

            let fill = if is_active {
                Fill::Words(
                    (0..line.words.len())
                        .map(|w| line.word_progress(w, position))
                        .collect(),
                )
            } else if i < focus || (i == focus && position >= line.end) {
                Fill::All
            } else {
                Fill::None
            };
            let (text, word_starts) = prepared.lines[i].clone();
            let start = line.start;
            children.push(
                div()
                    .id(("lyric", i))
                    .w_full()
                    .px_3()
                    .py(px(9.))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.seek_to(start, cx)))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if *hovered {
                            this.np.hovered_line = Some(i);
                        } else if this.np.hovered_line == Some(i) {
                            this.np.hovered_line = None;
                        }
                        cx.notify();
                    }))
                    .child(KaraokeLine {
                        text,
                        word_starts,
                        fill,
                        font: font.clone(),
                        font_size: px(LYRIC_SIZE),
                        line_height: px(LYRIC_LINE_HEIGHT),
                        scale: INACTIVE_SCALE + (1. - INACTIVE_SCALE) * act,
                        lit: white(dim + (1. - dim) * act),
                        dim: white(dim),
                        lift: lift * act,
                        shift,
                    })
                    .into_any_element(),
            );
        }
        children.push(div().h(viewport_height).into_any_element());

        pane()
            .child(
                div()
                    .id("lyrics")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.np.scroll)
                    .on_scroll(cx.listener(|this, _: &ScrollEvent, _, cx| {
                        this.np.manual_until = Some(Instant::now() + MANUAL_SCROLL_HOLD);
                        cx.notify();
                    }))
                    .children(children),
            )
            .into_any_element()
    }
}
