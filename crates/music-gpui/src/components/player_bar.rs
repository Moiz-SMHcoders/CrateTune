//! The player bar at the bottom, in three zones: track info (artwork, title,
//! artists, status; click opens Now Playing; like), transport + progress
//! (shuffle / previous / play / next / repeat, tags and integration chips, the
//! progress track with elapsed / remaining time and a time tooltip), and
//! secondary controls (lyrics, queue, sleep timer, Cast, mini player, Now
//! Playing, volume with mute, scroll and a percentage tooltip). Narrow windows
//! drop secondary controls by priority ([`bar_layout`]) instead of overlapping.
//! Every optional item can be hidden (`settings/bar_items.rs`); the quality menu,
//! "Add to playlist" and the right-click menu are in `bar_menu.rs`.
//!
//! Rendering cost: the bar is its own view ([`PlayerBar`]). Its static parts —
//! the three zones' buttons — are [`AppRegion`]s, rendered from the app and
//! re-rendered whenever the app notifies. The progress / volume tracks and the
//! background effect are drawn inline by the bar view, which is the only thing
//! that asks for frames while a style animates (Wavy, Equalizer, Pulse,
//! Gradient) or the pointer is on a track. While the bar is "live" like that the
//! app embeds its regions (and the rest of the window, see `app.rs`) as cached
//! views, so a bar frame redraws the bar, not the page above it.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ObjectFit, ParentElement as _, Render, ScrollEvent, SharedString,
    StatefulInteractiveElement as _, StyleRefinement, Styled as _, StyledImage as _, Subscription,
    Task, WeakEntity, Window, div, img, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _, Size,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{ContextMenuExt as _, DropdownMenu as _},
    tag::Tag,
    v_flex,
};
use music_core::{PlaybackStatus, format_duration};

use super::bar_fx::{BackgroundArt, BackgroundFx, BackgroundGeometry, BackgroundLook, TrackFx};
use super::progress::{ProgressTrack, TrackEvent, TrackLook, TrackParams};
use crate::anim::{Frame, Spec, Tween, ease_out_cubic};
use crate::app::{MusicApp, app_entity, read_app, update_app};
use crate::components::artwork::ArtworkCache;
use crate::icons::{AppIcon, Lucide};
use crate::now_playing::HiRes;
use crate::queue::Repeat;
use crate::settings::bar_items::BarItems;
use crate::settings::bar_prefs::{PlayerBarConfig, ProgressStyle};
use crate::theme::AppColors;

pub const BAR_HEIGHT: f32 = 88.;
/// Horizontal padding of the bar and the gap between its zones.
const PAD: f32 = 16.;
/// Secondary icon buttons (≥ 28 pt hit targets).
const ICON_BUTTON: f32 = 32.;
/// Transport row: previous / next and play.
const SKIP_BUTTON: f32 = 36.;
const PLAY_BUTTON: f32 = 40.;
const TRANSPORT_WIDTH: f32 = 2. * ICON_BUTTON + 2. * SKIP_BUTTON + PLAY_BUTTON + 4. * 8.;
/// Width of the time labels beside the progress track.
const TIME_WIDTH: f32 = 46.;
/// Volume change per scroll line.
const SCROLL_VOLUME_STEP: f32 = 0.04;

// ---- layout -------------------------------------------------------------------

/// What fits at a window width; the zones never overlap, controls are dropped
/// in priority order instead.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BarLayout {
    /// Width of each side zone (equal, so the transport is centred in the window).
    pub side: f32,
    pub center: f32,
    /// Volume slider width; `None`: icon only (scroll / click still work).
    pub slider: Option<f32>,
    pub lyrics: bool,
    pub cast: bool,
    pub sleep: bool,
    pub sleep_label: bool,
    pub mini: bool,
    /// The "YouTube · AAC 128 kbps" source tag beside the transport.
    pub source_chip: bool,
    pub shuffle: bool,
    pub repeat: bool,
    pub queue: bool,
    pub now_playing: bool,
    /// Beside the like button in the track info.
    pub add_to_playlist: bool,
}

/// Items the user hid ([`BarItems`]) are never shown and take no room. Of the
/// rest, priority, highest first: Now Playing, queue, mute (always); then an
/// active Cast session, an armed sleep timer (both even when hidden: they are
/// how you stop them), the volume slider, lyrics, Cast, sleep timer, mini
/// player, the sleep timer's label. Shuffle / repeat need room beside the
/// transport, the source tag room in its flank (more without shuffle / repeat),
/// "Add to playlist" a track info zone of at least 240 pt.
pub fn bar_layout(
    width: f32,
    casting: bool,
    sleep_armed: bool,
    cast_available: bool,
    items: BarItems,
) -> BarLayout {
    let side = (width * 0.27).clamp(200., 380.);
    let center = (width - 4. * PAD - 2. * side).max(0.);
    let step = ICON_BUTTON + 4.;
    let fixed = 1 + usize::from(items.queue) + usize::from(items.now_playing);
    let mut free = side - fixed as f32 * step;
    // An active Cast session and an armed sleep timer always show (their labels clip).
    if casting {
        free -= step + 96.;
    }
    if sleep_armed {
        free -= step;
    }
    let mut take = |w: f32| {
        if free >= w {
            free -= w;
            true
        } else {
            false
        }
    };
    let slider_w = if side >= 370. {
        92.
    } else if side >= 300. {
        80.
    } else {
        64.
    };
    let slider = if items.volume_slider {
        take(slider_w + 8.)
            .then_some(slider_w)
            .or_else(|| take(64. + 8.).then_some(64.))
    } else {
        None
    };
    let lyrics = items.lyrics && take(step);
    let cast = casting || (cast_available && items.cast && take(step));
    let sleep = sleep_armed || (items.sleep_timer && take(step));
    let mini = items.mini_player && take(step);
    let sleep_label = sleep_armed && sleep && take(64.);
    // Shuffle and repeat: both or neither fit (they flank the transport).
    let toggle_w = ICON_BUTTON + 8.;
    let toggles = usize::from(items.shuffle) + usize::from(items.repeat);
    let transport = TRANSPORT_WIDTH - (2 - toggles) as f32 * toggle_w;
    let toggles_fit = center >= transport + 40.;
    let transport = if toggles_fit {
        transport
    } else {
        TRANSPORT_WIDTH - 2. * toggle_w
    };
    BarLayout {
        side,
        center,
        slider,
        lyrics,
        cast,
        sleep,
        sleep_label,
        mini,
        source_chip: items.quality_tag && (center - transport) / 2. >= 150.,
        shuffle: items.shuffle && toggles_fit,
        repeat: items.repeat && toggles_fit,
        queue: items.queue,
        now_playing: items.now_playing,
        add_to_playlist: items.add_to_playlist && side >= 240.,
    }
}

// ---- regions ------------------------------------------------------------------

pub type RegionFn = fn(&mut MusicApp, &mut Window, &mut Context<MusicApp>) -> AnyElement;

/// A part of the window rendered from the app (its state and listeners) that
/// can be embedded as a cached view: re-rendered whenever the app notifies (or
/// something inside it does), reused otherwise.
pub struct AppRegion {
    render: RegionFn,
    _observe: Option<Subscription>,
}

impl AppRegion {
    pub fn new(render: RegionFn, cx: &mut Context<Self>) -> Self {
        let observe = app_entity(cx).map(|app| cx.observe(&app, |_, _, cx| cx.notify()));
        Self {
            render,
            _observe: observe,
        }
    }
}

impl Render for AppRegion {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let render = self.render;
        match app_entity(cx) {
            Some(app) => app.update(cx, |app, cx| render(app, window, cx)),
            None => div().into_any_element(),
        }
    }
}

/// `slot`'s region (created on first use) as an element filling its parent:
/// a cached view while `cached`, else drawn with every frame like inline code.
pub fn region(
    slot: &mut Option<Entity<AppRegion>>,
    render: RegionFn,
    cached: bool,
    cx: &mut App,
) -> AnyElement {
    let region = slot
        .get_or_insert_with(|| cx.new(|cx| AppRegion::new(render, cx)))
        .clone();
    if cached {
        region
            .cached(StyleRefinement::default().size_full())
            .into_any_element()
    } else {
        div().size_full().child(region).into_any_element()
    }
}

// ---- the bar view --------------------------------------------------------------

pub struct PlayerBar {
    progress: TrackFx,
    volume: TrackFx,
    background: BackgroundFx,
    /// Volume bubble shown briefly after scrolling.
    volume_bubble_until: Option<Instant>,
    /// Time tooltip fade.
    tip: Tween<f32>,
    left: Option<Entity<AppRegion>>,
    transport: Option<Entity<AppRegion>>,
    right: Option<Entity<AppRegion>>,
    /// The last render asked for more frames soon (an animation or a fade runs).
    animating: Cell<bool>,
    wake: Option<(Instant, Task<()>)>,
    _observe: Option<Subscription>,
}

impl PlayerBar {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let observe = app_entity(cx).map(|app| cx.observe(&app, |_, _, cx| cx.notify()));
        Self {
            progress: TrackFx::default(),
            volume: TrackFx::default(),
            background: BackgroundFx::default(),
            volume_bubble_until: None,
            tip: Tween::new(0.),
            left: None,
            transport: None,
            right: None,
            animating: Cell::new(false),
            wake: None,
            _observe: observe,
        }
    }

    /// The bar redraws often right now (animation, fade, pointer on a track):
    /// the app then embeds the rest of the window as cached views.
    pub fn is_live(&self) -> bool {
        self.animating.get()
            || self.progress.engaged()
            || self.volume.engaged()
            || self.volume_bubble_until.is_some_and(|t| t > Instant::now())
    }

    /// Where the progress and volume tracks were last drawn (window coordinates).
    pub fn track_bounds(&self) -> (gpui::Bounds<gpui::Pixels>, gpui::Bounds<gpui::Pixels>) {
        (self.progress.bounds.get(), self.volume.bounds.get())
    }

    fn schedule(&mut self, at: Instant, cx: &mut Context<Self>) {
        let now = Instant::now();
        if self
            .wake
            .as_ref()
            .is_some_and(|(t, _)| *t <= at && *t > now)
        {
            return;
        }
        let delay = at.saturating_duration_since(now);
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(delay.max(Duration::from_millis(1)))
                .await;
            this.update(cx, |this, cx| {
                this.wake = None;
                cx.notify();
            })
            .ok();
        });
        self.wake = Some((at, task));
    }
}

/// What the bar view needs from the app for one frame.
struct BarState {
    has_track: bool,
    total: f32,
    position: f32,
    playing: bool,
    scrub: Option<f32>,
    volume: f32,
    config: PlayerBarConfig,
    art: BackgroundArt,
    casting: bool,
    sleep_armed: bool,
}

impl Render for PlayerBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut frame = Frame::new();
        let now = frame.now;
        let theme = cx.theme().clone();
        let surface = theme.secondary;
        let accent = AppColors::get(cx).accent;
        let Some(state) = read_app(cx, |app, cx| {
            let config = app.settings.read(cx).settings().player_bar;
            let restored = app.playback.track.is_none() && app.store.resume_at.is_some();
            let hires = app
                .current_video_id()
                .and_then(|id| match app.hires.get(id) {
                    Some(HiRes::Ready {
                        backdrop,
                        art_colors,
                        ..
                    }) => Some((backdrop.clone(), *art_colors)),
                    _ => None,
                });
            let (backdrop, art_colors) = match hires {
                Some((b, c)) => (Some(b), c),
                None => (None, None),
            };
            let tones = super::bar_fx::ArtTones::new(
                art_colors.as_ref(),
                &crate::theme::AppPalette::get(cx),
            );
            BarState {
                has_track: app.playback.track.is_some() || restored,
                total: app.playback.duration.map_or(0., |d| d.as_secs_f32()),
                position: app.clock.position(now).as_secs_f32(),
                playing: app.playback.status == PlaybackStatus::Playing,
                scrub: app.scrub,
                volume: app.volume,
                config,
                art: BackgroundArt {
                    backdrop,
                    tones,
                    accent,
                    surface,
                },
                casting: app.cast_device().is_some(),
                sleep_armed: app.playback.sleep_timer.is_some(),
            }
        }) else {
            return div().into_any_element();
        };
        let width = f32::from(window.viewport_size().width);
        let layout = bar_layout(
            width,
            state.casting,
            state.sleep_armed,
            true,
            state.config.items,
        );
        let live = self.is_live();
        let config = state.config;

        // ---- progress
        // The spectrum while playing, and after a pause until the bars have fallen.
        let spectrum_wanted =
            config.wants_spectrum() && (state.playing || self.progress.level() > 0.);
        if spectrum_wanted {
            read_app(cx, |app, _| app.want_spectrum());
        }
        let analyzer = || {
            read_app(cx, |app, _| {
                app.audio_player().map(|p| p.spectrum_analyzer())
            })
            .flatten()
        };
        let track_frame = self.progress.step(
            &mut frame,
            config.progress_style,
            state.playing,
            spectrum_wanted,
            analyzer,
            1.,
        );
        let enabled = state.has_track && state.total > 0.;
        let shown = state.scrub.unwrap_or(state.position).clamp(0., state.total);
        let frac = if state.total > 0. {
            shown / state.total
        } else {
            0.
        };
        let look = TrackLook {
            played: theme.slider_bar,
            rail: theme.slider_bar.opacity(0.18),
            thumb: theme.foreground,
            ghost: theme.slider_bar.opacity(0.34),
        };
        let hover = self.progress.hover.filter(|_| !self.progress.dragging());
        let weak = cx.entity().downgrade();
        let total = state.total;
        let progress_track = ProgressTrack::new(
            "bar-progress",
            TrackParams {
                style: config.progress_style,
                frac,
                hover,
                reveal: track_frame.reveal,
                amplitude: track_frame.amplitude,
                phase: track_frame.phase,
                spectrum: track_frame.spectrum,
                peak_caps: config.peak_caps,
                enabled,
                look,
                height: px(20.),
                scale: 1.,
            },
            self.progress.input.clone(),
        )
        .bounds_into(self.progress.bounds.clone())
        .on_event(Rc::new(move |event, _, cx| {
            on_progress(&weak, event, total, cx)
        }));

        // Time tooltip over the track (hover or scrub).
        let tip_at = if self.progress.dragging() {
            Some(frac)
        } else {
            hover
        };
        self.tip.animate_to(
            if tip_at.is_some() && enabled { 1. } else { 0. },
            Spec::ms(120, ease_out_cubic),
            now,
        );
        let tip_alpha = frame.get(&self.tip);
        let track_w = f32::from(self.progress.bounds.get().size.width);
        let tooltip = tip_at.filter(|_| tip_alpha > 0.01 && enabled).map(|f| {
            bubble(
                format_duration((f * state.total) as u64),
                (f * track_w).clamp(0., track_w),
                tip_alpha,
                &theme,
            )
        });

        let elapsed = format_duration(shown as u64);
        let right_time = if !enabled {
            "--:--".to_owned()
        } else if config.show_remaining {
            format!(
                "-{}",
                format_duration((state.total - shown).max(0.).ceil() as u64)
            )
        } else {
            format_duration(state.total as u64)
        };
        // Static styles never ask for frames of their own: the time labels and the
        // fill follow the app's once-a-second update (it notifies this view).
        let show_remaining = config.show_remaining;
        let fg = theme.foreground;
        let progress_row = h_flex()
            .w_full()
            .max_w(px(680.))
            .gap_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(
                div()
                    .w(px(TIME_WIDTH))
                    .flex_none()
                    .text_right()
                    .child(elapsed),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h(px(20.))
                    .child(progress_track)
                    .children(tooltip),
            )
            .child(
                div()
                    .id("bar-time-right")
                    .w(px(TIME_WIDTH))
                    .flex_none()
                    .cursor_pointer()
                    .hover(move |s| s.text_color(fg))
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(if show_remaining {
                            t!("bar.tooltip.show_length")
                        } else {
                            t!("bar.tooltip.show_remaining")
                        })
                        .build(window, cx)
                    })
                    .on_click(move |_, _, cx| {
                        update_app(cx, |app, cx| {
                            app.settings
                                .update(cx, |p, cx| p.set_show_remaining(!show_remaining, cx))
                        });
                    })
                    .child(right_time),
            );

        // ---- volume
        let vol_frame = self.volume.step(
            &mut frame,
            ProgressStyle::Standard,
            false,
            false,
            || None,
            0.85,
        );
        let bubble_scroll = self.volume_bubble_until.is_some_and(|t| t > now);
        if let Some(t) = self.volume_bubble_until.filter(|&t| t > now) {
            frame.wake_at(t);
        }
        let volume = state.volume;
        let vol_tip = (self.volume.engaged() || bubble_scroll).then(|| {
            let w = f32::from(self.volume.bounds.get().size.width);
            let f = if self.volume.dragging() || bubble_scroll {
                volume
            } else {
                self.volume.hover.unwrap_or(volume)
            };
            bubble(
                t!(
                    "bar.volume_percent",
                    percent = crate::i18n::format::number((f * 100.).round() as u64)
                ),
                f * w,
                1.,
                &theme,
            )
        });
        let weak = cx.entity().downgrade();
        let scroll_volume = config.scroll_volume;
        let volume_group = h_flex()
            .id("bar-volume")
            .flex_none()
            .h_full()
            .items_center()
            .gap_1()
            .when(scroll_volume, |this| {
                let weak = weak.clone();
                this.on_scroll(move |e: &ScrollEvent, window, cx| {
                    scroll_volume_by(&weak, scroll_lines(e), window, cx);
                })
            })
            .child(
                Button::new("mute")
                    .ghost()
                    .with_size(Size::Medium)
                    .icon(if volume <= 0. {
                        AppIcon::Muted
                    } else if volume < 0.5 {
                        AppIcon::VolumeLow
                    } else {
                        AppIcon::Volume
                    })
                    .tooltip(if volume <= 0. {
                        t!("bar.tooltip.unmute")
                    } else {
                        t!("bar.tooltip.mute")
                    })
                    .on_click(|_, window, cx| {
                        update_app(cx, |app, cx| app.mute_toggle(window, cx));
                    }),
            )
            .when_some(layout.slider, |this, w| {
                let weak = weak.clone();
                this.child(
                    div()
                        .relative()
                        .w(px(w))
                        .h(px(20.))
                        .flex_none()
                        .child(
                            ProgressTrack::new(
                                "bar-volume-track",
                                TrackParams {
                                    style: ProgressStyle::Standard,
                                    frac: volume,
                                    hover: None,
                                    reveal: vol_frame.reveal.max(0.35),
                                    amplitude: 0.,
                                    phase: 0.,
                                    spectrum: None,
                                    peak_caps: false,
                                    enabled: true,
                                    look,
                                    height: px(20.),
                                    scale: 0.85,
                                },
                                self.volume.input.clone(),
                            )
                            .bounds_into(self.volume.bounds.clone())
                            .on_event(Rc::new(
                                move |event, window, cx| {
                                    on_volume(&weak, event, scroll_volume, window, cx)
                                },
                            )),
                        )
                        .children(vol_tip),
                )
            });

        // ---- background
        let side = layout.side;
        let level = self.progress.level();
        let layers = self.background.layers(
            &mut frame,
            BackgroundLook::from_config(&config, state.playing, level),
            &state.art,
            BackgroundGeometry {
                width,
                info_end: PAD + side,
                center: (2. * PAD + side, width - 2. * PAD - side),
            },
        );

        // ---- zones
        let left = region(&mut self.left, MusicApp::bar_info_zone, live, cx);
        let transport = region(&mut self.transport, MusicApp::bar_transport_zone, live, cx);
        let right = region(&mut self.right, MusicApp::bar_secondary_zone, live, cx);

        let element = div()
            .id("player-bar")
            .relative()
            .h(px(BAR_HEIGHT))
            .w_full()
            .flex_none()
            .overflow_hidden()
            .border_t_1()
            .border_color(theme.border)
            .bg(surface)
            .children(layers)
            .child(
                h_flex()
                    .absolute()
                    .inset_0()
                    .px(px(PAD))
                    .gap(px(PAD))
                    .child(div().w(px(side)).h_full().flex_none().child(left))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .justify_center()
                            .items_center()
                            .gap(px(2.))
                            .child(div().w_full().h(px(PLAY_BUTTON + 2.)).child(transport))
                            .child(progress_row),
                    )
                    .child(
                        h_flex()
                            .w(px(side))
                            .h_full()
                            .flex_none()
                            .gap_1()
                            .child(div().flex_1().min_w_0().h_full().child(right))
                            .child(volume_group),
                    ),
            )
            .context_menu(super::bar_menu::customize_menu)
            .into_any_element();

        let animating = frame.is_animating();
        let wake = frame.finish(window);
        // Continuous styles wake on a timer (~30 fps) instead of every display frame.
        self.animating.set(
            animating
                || wake.is_some_and(|at| {
                    at.saturating_duration_since(now) < Duration::from_millis(100)
                }),
        );
        if let Some(at) = wake {
            self.schedule(at, cx);
        }
        element
    }
}

fn scroll_lines(e: &ScrollEvent) -> f32 {
    match e.delta {
        gpui::ScrollDelta::Lines(p) => p.y,
        gpui::ScrollDelta::Pixels(p) => f32::from(p.y) / 20.,
    }
}

/// A small tooltip bubble centred above `x` (px from the track's left).
fn bubble(
    text: impl Into<SharedString>,
    x: f32,
    alpha: f32,
    theme: &gpui_component::Theme,
) -> AnyElement {
    const W: f32 = 64.;
    div()
        .absolute()
        .bottom(px(20.))
        .left(px(x - W / 2.))
        .w(px(W))
        .flex()
        .justify_center()
        .opacity(alpha)
        .child(
            div()
                .px_1p5()
                .py_0p5()
                .rounded(px(6.))
                .bg(theme.popover)
                .border_1()
                .border_color(theme.border)
                .text_xs()
                .text_color(theme.popover_foreground)
                .shadow_sm()
                .whitespace_nowrap()
                .child(text.into()),
        )
        .into_any_element()
}

fn on_progress(bar: &WeakEntity<PlayerBar>, event: TrackEvent, total: f32, cx: &mut App) {
    let seconds = |f: f32| (f * total).clamp(0., total);
    match event {
        TrackEvent::Hover(at) => {
            bar.update(cx, |bar, cx| {
                bar.progress.hover = at;
                cx.notify();
            })
            .ok();
        }
        TrackEvent::Press(f) | TrackEvent::Drag(f) => {
            update_app(cx, |app, _| app.scrub = Some(seconds(f)));
            bar.update(cx, |_, cx| cx.notify()).ok();
        }
        TrackEvent::Release(f) => {
            update_app(cx, |app, cx| {
                app.scrub = None;
                app.seek_to(Duration::from_secs_f32(seconds(f)), cx);
            });
            bar.update(cx, |_, cx| cx.notify()).ok();
        }
        TrackEvent::Scroll(_) => {}
    }
}

fn on_volume(
    bar: &WeakEntity<PlayerBar>,
    event: TrackEvent,
    scroll: bool,
    window: &mut Window,
    cx: &mut App,
) {
    match event {
        TrackEvent::Hover(at) => {
            bar.update(cx, |bar, cx| {
                bar.volume.hover = at;
                cx.notify();
            })
            .ok();
        }
        TrackEvent::Press(f) | TrackEvent::Drag(f) | TrackEvent::Release(f) => {
            update_app(cx, |app, cx| {
                app.set_volume((f * 100.).round() / 100., window, cx)
            });
            bar.update(cx, |_, cx| cx.notify()).ok();
        }
        TrackEvent::Scroll(lines) if scroll => scroll_volume_by(bar, lines, window, cx),
        TrackEvent::Scroll(_) => {}
    }
}

fn scroll_volume_by(bar: &WeakEntity<PlayerBar>, lines: f32, window: &mut Window, cx: &mut App) {
    if lines == 0. {
        return;
    }
    update_app(cx, |app, cx| {
        let target = (app.volume + lines * SCROLL_VOLUME_STEP).clamp(0., 1.);
        app.set_volume((target * 100.).round() / 100., window, cx);
    });
    bar.update(cx, |bar, cx| {
        bar.volume_bubble_until = Some(Instant::now() + Duration::from_millis(900));
        cx.notify();
    })
    .ok();
}

// ---- zones (rendered from the app) ----------------------------------------------

impl MusicApp {
    /// The bar view (created on first use).
    pub(crate) fn player_bar_view(&mut self, cx: &mut Context<Self>) -> Entity<PlayerBar> {
        self.bar
            .get_or_insert_with(|| cx.new(PlayerBar::new))
            .clone()
    }

    /// The bar redraws often right now (see [`PlayerBar::is_live`]).
    pub(crate) fn bar_is_live(&self, cx: &App) -> bool {
        self.bar.as_ref().is_some_and(|b| b.read(cx).is_live())
    }

    fn bar_layout_now(&self, window: &Window, cx: &App) -> BarLayout {
        bar_layout(
            f32::from(window.viewport_size().width),
            self.cast_device().is_some(),
            self.playback.sleep_timer.is_some(),
            true,
            self.settings.read(cx).bar_items(),
        )
    }

    /// The track the bar shows: the playing one, or last session's restored one.
    pub(crate) fn bar_track(&self) -> (Option<music_core::Track>, bool) {
        let restored = self.playback.track.is_none() && self.store.resume_at.is_some();
        let track = self
            .playback
            .track
            .clone()
            .or_else(|| restored.then(|| self.queue.current().cloned()).flatten());
        (track, restored)
    }

    /// Left zone: artwork, title, artists, a status line; like.
    pub(crate) fn bar_info_zone(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let layout = self.bar_layout_now(window, cx);
        let like = self.settings.read(cx).bar_items().like;
        let (track, _) = self.bar_track();
        let image = track
            .as_ref()
            .and_then(|t| t.artwork_url(120))
            .and_then(|url| ArtworkCache::global(cx).update(cx, |cache, cx| cache.get(&url, cx)));
        let message = self.bar_message(cx);
        let theme = cx.theme();
        let (title, artists): (SharedString, SharedString) = match &track {
            Some(t) => (t.title.clone().into(), t.artists.clone().into()),
            None => (
                t!("common.not_playing").into(),
                t!("player.pick_a_song").into(),
            ),
        };
        let art = div()
            .size(px(56.))
            .flex_none()
            .rounded(px(6.))
            .overflow_hidden()
            .bg(theme.muted)
            .flex()
            .items_center()
            .justify_center()
            .map(|this| match image {
                Some(image) => this.child(img(image).size_full().object_fit(ObjectFit::Cover)),
                None => this.child(
                    Icon::new(Lucide::Music)
                        .size(px(22.))
                        .text_color(theme.muted_foreground),
                ),
            });
        let hover = theme.list_hover;
        let has_track = track.is_some();
        h_flex()
            .size_full()
            .gap_1()
            .child(
                h_flex()
                    .id("np-bar-open")
                    .flex_1()
                    .min_w_0()
                    .gap_3()
                    .p_1()
                    .rounded(px(8.))
                    .when(has_track, |this| {
                        this.cursor_pointer().hover(move |s| s.bg(hover)).on_click(
                            cx.listener(|this, _, window, cx| this.open_now_playing(window, cx)),
                        )
                    })
                    .child(art)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
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
                            )
                            .when_some(message, |this, (text, color)| {
                                this.child(div().truncate().text_xs().text_color(color).child(text))
                            }),
                    ),
            )
            .children(track.as_ref().filter(|_| like).map(|t| {
                crate::components::like::like_button("bar-like", t, None, cx)
                    .with_size(Size::Medium)
            }))
            .when(layout.add_to_playlist, |this| {
                this.child(self.bar_add_to_playlist_button(track.as_ref()))
            })
            .into_any_element()
    }

    /// Errors, loading, buffering, a skipped song.
    fn bar_message(&self, cx: &App) -> Option<(SharedString, Hsla)> {
        let theme = cx.theme();
        let state = &self.playback;
        let recent_failure = self
            .last_failure
            .as_ref()
            .filter(|(_, at)| at.elapsed() < Duration::from_secs(8))
            .map(|(title, _)| title.clone());
        if let Some(err) = &self.audio_error {
            Some((
                t!("player.audio_unavailable", error = err).into(),
                theme.danger,
            ))
        } else if let PlaybackStatus::Error(err) = &state.status {
            Some((err.clone().into(), theme.danger))
        } else if state.status == PlaybackStatus::Loading {
            Some((t!("common.loading").into(), theme.muted_foreground))
        } else if state.buffering && state.status == PlaybackStatus::Playing {
            Some((t!("player.buffering").into(), theme.muted_foreground))
        } else {
            recent_failure.map(|title| {
                (
                    t!("player.play_failed_skipped", title = title).into(),
                    theme.warning,
                )
            })
        }
    }

    /// Centre: tags · shuffle / previous / play / next / repeat · chips.
    pub(crate) fn bar_transport_zone(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let layout = self.bar_layout_now(window, cx);
        let items = self.settings.read(cx).bar_items();
        let chips = self.integration_chips(items, cx);
        let (track, restored) = self.bar_track();
        let state = &self.playback;
        let has_next = self.queue.has_next();
        let (shuffle, repeat) = (self.queue.shuffle(), self.queue.repeat());
        let is_playing = state.status == PlaybackStatus::Playing;
        let can_toggle = self.engine.is_some()
            && (restored
                || matches!(
                    state.status,
                    PlaybackStatus::Playing | PlaybackStatus::Paused | PlaybackStatus::Ended
                ));
        let loading = state.status == PlaybackStatus::Loading;
        let preview = state.preview_only && track.is_some();
        let source = crate::module_service::source_label(state.source.as_ref())
            .filter(|_| {
                track.is_some() && state.status != PlaybackStatus::Idle && layout.source_chip
            })
            .map(|label| {
                let module = state
                    .source
                    .as_ref()
                    .is_some_and(|s| s.kind == music_core::SourceKind::Module);
                (label, module)
            });
        let accent = AppColors::get(cx).accent;
        let toggle_icon = |icon: Lucide, on: bool| {
            let icon = Icon::new(icon);
            if on { icon.text_color(accent) } else { icon }
        };
        h_flex()
            .size_full()
            .gap_2()
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .justify_end()
                    .gap_1()
                    .overflow_hidden()
                    .when(preview, |this| {
                        this.child(chip_slot(
                            Tag::warning()
                                .small()
                                .rounded_full()
                                .child(t!("player.preview_badge"))
                                .into_any_element(),
                        ))
                    })
                    .when_some(source, |this, (label, module)| {
                        this.child(chip_slot(self.bar_quality_tag(label, module, cx)))
                    }),
            )
            .when(layout.shuffle, |this| {
                this.child(
                    Button::new("shuffle")
                        .ghost()
                        .with_size(Size::Medium)
                        .rounded(px(999.))
                        .icon(toggle_icon(Lucide::Shuffle, shuffle))
                        .selected(shuffle)
                        .tooltip(if shuffle {
                            t!("player.tooltip.shuffle_on")
                        } else {
                            t!("player.tooltip.shuffle_off")
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_shuffle(cx))),
                )
            })
            .child(
                Button::new("previous")
                    .ghost()
                    .with_size(px(26.))
                    .w(px(SKIP_BUTTON))
                    .h(px(SKIP_BUTTON))
                    .rounded(px(999.))
                    .icon(AppIcon::Previous)
                    .tooltip(t!("player.tooltip.previous"))
                    .disabled(track.is_none())
                    .on_click(cx.listener(|this, _, _, cx| this.previous_track(cx))),
            )
            .child(
                Button::new("play-pause")
                    .primary()
                    .with_size(px(28.))
                    .w(px(PLAY_BUTTON))
                    .h(px(PLAY_BUTTON))
                    .rounded(px(999.))
                    .icon(if is_playing {
                        IconName::Pause
                    } else {
                        IconName::Play
                    })
                    .tooltip(if is_playing {
                        t!("bar.tooltip.pause")
                    } else {
                        t!("bar.tooltip.play")
                    })
                    .loading(loading)
                    .disabled(!can_toggle)
                    .on_click(cx.listener(Self::toggle_pause)),
            )
            .child(
                Button::new("next")
                    .ghost()
                    .with_size(px(26.))
                    .w(px(SKIP_BUTTON))
                    .h(px(SKIP_BUTTON))
                    .rounded(px(999.))
                    .icon(AppIcon::Next)
                    .tooltip(t!("player.tooltip.next"))
                    .disabled(!has_next)
                    .on_click(cx.listener(|this, _, _, cx| this.next_track(cx))),
            )
            .when(layout.repeat, |this| {
                this.child(
                    Button::new("repeat")
                        .ghost()
                        .with_size(Size::Medium)
                        .rounded(px(999.))
                        .icon(toggle_icon(
                            if repeat == Repeat::One {
                                Lucide::Repeat1
                            } else {
                                Lucide::Repeat
                            },
                            repeat != Repeat::Off,
                        ))
                        .selected(repeat != Repeat::Off)
                        .tooltip(match repeat {
                            Repeat::Off => t!("player.tooltip.repeat_off"),
                            Repeat::All => t!("player.tooltip.repeat_all"),
                            Repeat::One => t!("player.tooltip.repeat_one"),
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.cycle_repeat(cx))),
                )
            })
            // SponsorBlock "Skip", Music Together: shrink and clip, never overlap.
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .overflow_hidden()
                    .children(chips.into_iter().map(chip_slot)),
            )
            .into_any_element()
    }

    /// Right zone (before the volume): lyrics, queue, sleep timer, Cast, mini
    /// player, Now Playing — in priority order when the window is narrow.
    pub(crate) fn bar_secondary_zone(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let layout = self.bar_layout_now(window, cx);
        let cast = layout
            .cast
            .then(|| self.integration_cast_button(cx))
            .flatten();
        let (track, _) = self.bar_track();
        let sleep = self.sleep_timer_label();
        let sleep_armed = sleep.is_some();
        let sleep_custom = self.settings.read(cx).sleep_custom();
        let np_open = self.np.open;
        let lyrics_open = np_open && self.np.lyrics_view;
        let accent = AppColors::get(cx).accent;
        let toggle = |id: &'static str, icon: Lucide, on: bool| {
            Button::new(id)
                .ghost()
                .with_size(Size::Medium)
                .icon(if on {
                    Icon::new(icon).text_color(accent)
                } else {
                    Icon::new(icon)
                })
                .selected(on)
        };
        h_flex()
            .size_full()
            .justify_end()
            .gap_1()
            .when(layout.lyrics, |this| {
                this.child(
                    toggle("lyrics-toggle", Lucide::MicVocal, lyrics_open)
                        .tooltip(t!("bar.tooltip.lyrics"))
                        .disabled(track.is_none())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.toggle_lyrics_view(window, cx)),
                        ),
                )
            })
            .when(layout.queue, |this| {
                this.child(
                    toggle("queue-toggle", Lucide::ListMusic, self.queue_open)
                        .tooltip(t!("player.tooltip.queue"))
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_queue_panel(cx))),
                )
            })
            .when(layout.sleep, |this| {
                this.child(
                    div()
                        .flex_shrink(1.)
                        .min_w(px(ICON_BUTTON))
                        .overflow_hidden()
                        .child(
                            toggle("sleep-timer", Lucide::Moon, sleep_armed)
                                .when_some(sleep.filter(|_| layout.sleep_label), |b, label| {
                                    b.label(label)
                                })
                                .tooltip(t!("player.sleep.title"))
                                .dropdown_menu(move |menu, _, _| {
                                    MusicApp::sleep_timer_menu(menu, sleep_armed, sleep_custom)
                                }),
                        ),
                )
            })
            // "Casting to …" clips at its end; the icon stays.
            .children(cast.map(|cast| {
                div()
                    .flex_shrink(1.)
                    .min_w(px(ICON_BUTTON))
                    .max_w(px(160.))
                    .h(px(ICON_BUTTON))
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .child(cast)
            }))
            .when(layout.mini, |this| {
                this.child(
                    toggle(
                        "mini-toggle",
                        Lucide::PictureInPicture2,
                        self.mini.is_some(),
                    )
                    .tooltip(t!("player.tooltip.mini_player"))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_mini_player(cx))),
                )
            })
            .when(layout.now_playing, |this| {
                this.child(
                    Button::new("np-expand")
                        .ghost()
                        .with_size(Size::Medium)
                        .icon(IconName::ChevronUp)
                        .selected(np_open && !lyrics_open)
                        .tooltip(t!("player.tooltip.now_playing"))
                        .disabled(track.is_none())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_now_playing(window, cx)),
                        ),
                )
            })
            .into_any_element()
    }

    /// Mute button: silence, or back to the volume before muting.
    pub(crate) fn mute_toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = if self.volume > 0. {
            0.
        } else {
            self.unmuted_volume
        };
        self.set_volume(target, window, cx);
    }

    /// Lyrics button: Now Playing with the lyrics in view (a design without a
    /// lyrics pane shows the lyrics-first layout until it closes); again closes it.
    pub(crate) fn toggle_lyrics_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.np.open && self.np.lyrics_view {
            self.close_now_playing(window, cx);
            return;
        }
        self.np.lyrics_view = true;
        if self.np.open {
            cx.notify();
        } else {
            self.open_now_playing(window, cx);
        }
    }
}

/// A tag / chip beside the transport: shrinks (clipping its end), down to nothing.
fn chip_slot(chip: AnyElement) -> gpui::Div {
    div()
        .flex_shrink(1.)
        .min_w_0()
        .overflow_hidden()
        .child(chip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::bar_items::BarItem;

    const WIDTHS: [f32; 11] = [
        700., 760., 880., 1000., 1180., 1280., 1440., 1600., 1920., 2200., 2400.,
    ];

    fn layout(width: f32, items: BarItems) -> BarLayout {
        bar_layout(width, false, false, true, items)
    }

    fn all_items() -> BarItems {
        let mut all = BarItems::none();
        for item in BarItem::ALL {
            all.set(item, true);
        }
        all
    }

    /// Width the right zone's buttons take (Cast's label excluded: it clips).
    fn right_width(l: &BarLayout) -> f32 {
        let step = ICON_BUTTON + 4.;
        let buttons = [l.queue, l.now_playing, l.lyrics, l.cast, l.sleep, l.mini];
        let mut w = step * (1 + buttons.iter().filter(|b| **b).count()) as f32;
        if l.sleep_label {
            w += 64.;
        }
        if let Some(slider) = l.slider {
            w += slider + 8.;
        }
        w
    }

    /// Whether the layout shows `item` (`None`: not a layout decision).
    fn shows(l: &BarLayout, item: BarItem) -> Option<bool> {
        Some(match item {
            BarItem::QualityTag => l.source_chip,
            BarItem::Shuffle => l.shuffle,
            BarItem::Repeat => l.repeat,
            BarItem::Lyrics => l.lyrics,
            BarItem::AddToPlaylist => l.add_to_playlist,
            BarItem::Queue => l.queue,
            BarItem::SleepTimer => l.sleep,
            BarItem::Cast => l.cast,
            BarItem::MiniPlayer => l.mini,
            BarItem::NowPlaying => l.now_playing,
            BarItem::Volume => l.slider.is_some(),
            BarItem::Like | BarItem::SponsorBlock | BarItem::Together => return None,
        })
    }

    /// The default items, everything, nothing, and each item alone hidden / shown.
    fn item_sets() -> Vec<BarItems> {
        let mut sets = vec![BarItems::default(), all_items(), BarItems::none()];
        for item in BarItem::ALL {
            let mut hidden = all_items();
            hidden.set(item, false);
            let mut alone = BarItems::none();
            alone.set(item, true);
            sets.extend([hidden, alone]);
        }
        sets
    }

    #[test]
    fn narrow_bars_drop_controls_by_priority_without_overlap() {
        for width in WIDTHS {
            for casting in [false, true] {
                for sleep in [false, true] {
                    for items in item_sets() {
                        let l = bar_layout(width, casting, sleep, true, items);
                        // The zones and their padding add up to the window width.
                        assert!(2. * l.side + l.center + 4. * PAD <= width + 0.5, "{width}");
                        if casting {
                            assert!(l.cast, "an active Cast session stays visible at {width}");
                        } else {
                            // The right zone's buttons fit beside the volume.
                            assert!(right_width(&l) <= l.side + 0.5, "{width} {items:?}: {l:?}");
                        }
                        if sleep {
                            assert!(l.sleep, "an armed sleep timer stays visible at {width}");
                        }
                        // Hidden items never show (active Cast / timer aside).
                        for item in BarItem::ALL {
                            let forced = (item == BarItem::Cast && casting)
                                || (item == BarItem::SleepTimer && sleep);
                            if !items.shows(item) && !forced {
                                assert!(!shows(&l, item).unwrap_or(false), "{item:?}");
                            }
                        }
                    }
                }
            }
        }
        let narrow = layout(880., BarItems::default());
        assert!(narrow.slider.is_some(), "volume slider at 880 pt");
        assert!(narrow.shuffle && narrow.repeat);
        assert!(!narrow.mini && !narrow.source_chip && !narrow.add_to_playlist);
        let common = layout(1280., BarItems::default());
        assert!(common.mini && common.lyrics && common.slider == Some(80.));
        let wide = layout(1600., BarItems::default());
        assert!(wide.lyrics && wide.cast && wide.sleep && wide.mini && wide.source_chip);
        assert_eq!(wide.slider, Some(92.));
    }

    #[test]
    fn hidden_items_free_room_for_the_others() {
        for width in WIDTHS {
            let base = layout(width, all_items());
            for hidden in BarItem::ALL {
                let mut items = all_items();
                items.set(hidden, false);
                let l = layout(width, items);
                // Nothing else disappears, and the slider never gets shorter.
                for item in BarItem::ALL.into_iter().filter(|i| *i != hidden) {
                    if shows(&base, item) == Some(true) {
                        assert_eq!(
                            shows(&l, item),
                            Some(true),
                            "{width}: hiding {hidden:?} hid {item:?}"
                        );
                    }
                }
                if hidden != BarItem::Volume {
                    assert!(l.slider >= base.slider, "{width}: {hidden:?}");
                }
            }
        }
        // 880 pt: without lyrics and Cast the sleep timer fits; with only the
        // essentials and the mini player, the mini player does.
        let items = BarItems {
            lyrics: false,
            cast: false,
            ..BarItems::default()
        };
        assert!(layout(880., items).sleep && !layout(880., BarItems::default()).sleep);
        let mut items = BarItems::none();
        items.mini_player = true;
        items.queue = true;
        items.now_playing = true;
        assert!(layout(880., items).mini);
        // 1180 pt: the source tag fits once shuffle / repeat are hidden.
        let mut items = BarItems::default();
        assert!(!layout(1180., items).source_chip);
        items.shuffle = false;
        items.repeat = false;
        assert!(layout(1180., items).source_chip);
        // Only play / pause, previous / next and mute are left with nothing.
        let none = layout(1280., BarItems::none());
        assert_eq!(right_width(&none), ICON_BUTTON + 4.);
        assert!(none.slider.is_none() && !none.source_chip && !none.shuffle);
        // "Add to playlist" (off by default) needs a track info zone of 240 pt.
        let items = BarItems {
            add_to_playlist: true,
            ..BarItems::default()
        };
        assert!(!layout(880., items).add_to_playlist || layout(880., items).side >= 240.);
        assert!(!layout(700., items).add_to_playlist && layout(1280., items).add_to_playlist);
    }
}
