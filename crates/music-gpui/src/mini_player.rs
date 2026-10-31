//! Compact always-on-top player window (⇧⌘M, View menu, bar button): artwork,
//! title / artist, previous / play / next, progress (click / drag to seek). It
//! reads everything from `MusicApp` (re-rendering when the app or the artwork
//! cache notifies, i.e. at the 250 ms poll rate) and sends commands back through
//! [`update_app`]. With "Same look in the mini player" it draws the bar's
//! progress style and play / pause background (`components/bar_fx.rs`), asking
//! for its own frames only while those animate. Its position is saved in the
//! settings file.
//!
//! macOS: a floating `NSPanel` (GPUI `WindowKind::Floating`) whose
//! `hidesOnDeactivate` is turned off, so it stays visible over other apps. Windows:
//! a borderless window made topmost (`win::set_topmost`) with its own close button
//! and drag strip.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, FocusHandle, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _,
    StyledImage as _, Subscription, TitlebarOptions, Window, WindowBounds, WindowKind,
    WindowOptions, div, point, px, size,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconName, Root, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use music_core::{PlaybackStatus, format_duration};

use crate::actions::with_main_window;
use crate::anim::{Frame, RenderRate, Tween};
use crate::app::{MusicApp, read_app, update_app};
use crate::components::artwork::{ArtworkCache, artwork};
use crate::components::bar_fx::{
    BackgroundArt, BackgroundFx, BackgroundGeometry, BackgroundLook, TrackFx,
};
use crate::components::progress::{ProgressTrack, TrackEvent, TrackLook, TrackParams};
use crate::icons::AppIcon;
use crate::settings::WindowGeometry;
use crate::settings::bar_prefs::ProgressStyle;
use crate::settings::prefs::MiniPlayerSkin;

pub struct MiniPlayer {
    focus: FocusHandle,
    /// The layout this window was opened with (another one reopens it).
    skin: MiniPlayerSkin,
    /// Turntable: 0 = needle on the record, 1 = arm lifted beside it.
    arm: Tween<f32>,
    progress: TrackFx,
    background: BackgroundFx,
    wake: Option<(std::time::Instant, gpui::Task<()>)>,
    /// `MUSIC_TIMINGS`: renders per second of this window ("mini").
    rate: Option<RenderRate>,
    _subscriptions: Vec<Subscription>,
}

impl MusicApp {
    pub fn toggle_mini_player(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.mini.take() {
            // Closing runs the window-closed observers, which update the app:
            // do it after this update.
            cx.defer(move |cx| {
                handle
                    .update(cx, |_, window, _| window.remove_window())
                    .ok();
            });
            cx.notify();
            return;
        }
        let geometry = self.settings.read(cx).settings().mini_player;
        let skin = self.settings.read(cx).settings().now_playing.mini_skin;
        // Opening a window builds its view, which reads the app: do it after this update.
        cx.defer(move |cx| {
            let handle = open(geometry, skin, cx);
            update_app(cx, |app, cx| {
                app.mini = handle;
                cx.notify();
            });
        });
    }
}

fn open(
    geometry: Option<WindowGeometry>,
    skin: MiniPlayerSkin,
    cx: &mut App,
) -> Option<AnyWindowHandle> {
    let (width, height) = skin.size();
    let size = size(px(width), px(height));
    let bounds = geometry
        .map(|g| Bounds::new(point(px(g.x), px(g.y)), size))
        .filter(|b| crate::on_screen(b, cx))
        .unwrap_or_else(|| {
            // Default: bottom-right of the primary display.
            let display = cx.primary_display().map(|d| d.bounds());
            match display {
                Some(d) => Bounds::new(
                    point(
                        d.origin.x + d.size.width - size.width - px(24.),
                        d.origin.y + d.size.height - size.height - px(96.),
                    ),
                    size,
                ),
                None => Bounds::centered(None, size, cx),
            }
        });
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some(t!("mini.window_title").into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(10.), px(9.))),
        }),
        kind: WindowKind::Floating,
        is_resizable: false,
        is_minimizable: false,
        focus: true,
        show: true,
        ..Default::default()
    };
    let window = cx
        .open_window(options, |window, cx| {
            keep_visible_when_inactive(window);
            #[cfg(target_os = "windows")]
            if let Some(hwnd) = crate::services::hwnd(window) {
                crate::win::set_topmost(hwnd.get());
            }
            let view = cx.new(|cx| MiniPlayer::new(skin, window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .map_err(|e| eprintln!("music-gpui: mini player: {e}"))
        .ok()?;
    Some(window.into())
}

impl MiniPlayer {
    fn new(skin: MiniPlayerSkin, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = vec![
            cx.observe(&ArtworkCache::global(cx), |_, _, cx| cx.notify()),
            cx.observe_window_bounds(window, |_, window, cx| {
                let b = window.bounds();
                let geometry = WindowGeometry {
                    x: b.origin.x.into(),
                    y: b.origin.y.into(),
                    width: b.size.width.into(),
                    height: b.size.height.into(),
                    maximized: false,
                };
                update_app(cx, |app, cx| {
                    app.settings
                        .update(cx, |p, cx| p.set_mini_player_geometry(geometry, cx))
                });
            }),
        ];
        if let Some(app) = crate::app::app_entity(cx) {
            subscriptions.push(cx.observe(&app, |_, _, cx| cx.notify()));
        }
        let playing =
            read_app(cx, |app, _| app.playback.status == PlaybackStatus::Playing).unwrap_or(false);
        Self {
            focus: cx.focus_handle(),
            skin,
            arm: Tween::new(if playing { 0. } else { 1. }),
            progress: TrackFx::default(),
            background: BackgroundFx::default(),
            wake: None,
            rate: RenderRate::from_env("mini"),
            _subscriptions: subscriptions,
        }
    }
}

impl Render for MiniPlayer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut frame = Frame::new();
        let now = frame.now;
        let info = read_app(cx, |app, cx| {
            let bar = app.settings.read(cx).settings().player_bar;
            let skin = app.settings.read(cx).settings().now_playing.mini_skin;
            let hires_art = app
                .current_video_id()
                .and_then(|id| match app.hires.get(id) {
                    Some(crate::now_playing::HiRes::Ready {
                        backdrop,
                        art_colors,
                        art,
                        ..
                    }) => Some((backdrop.clone(), *art_colors, art.clone())),
                    _ => None,
                });
            // The line being sung (wakes this window when the next one starts).
            let lyric = (skin == MiniPlayerSkin::Lyrics)
                .then(|| {
                    let track = app.playback.track.as_ref();
                    app.lyric_snippet(track, app.clock.position(now), &mut frame)
                })
                .flatten();
            (
                (
                    app.playback.track.clone(),
                    app.playback.status.clone(),
                    app.clock.position(now),
                    app.playback.duration,
                    app.queue.has_next(),
                    app.engine.is_some(),
                    app.mini_backdrop(cx),
                ),
                (
                    bar,
                    hires_art,
                    app.scrub,
                    app.playback
                        .track
                        .as_ref()
                        .and_then(|t| app.waveform.ready_for(&t.video_id).cloned()),
                    skin,
                    lyric,
                ),
            )
        });
        let Some((
            (track, status, position, duration, has_next, has_engine, backdrop),
            (bar, hires_art, scrub, waveform, skin, lyric),
        )) = info
        else {
            return div().size_full().into_any_element();
        };
        if skin != self.skin {
            // Another layout needs another window size (and title bar spot):
            // open the mini player again where it is.
            self.skin = skin;
            cx.defer(|cx| {
                update_app(cx, |app, cx| {
                    app.toggle_mini_player(cx);
                    app.toggle_mini_player(cx);
                });
            });
        }
        let art_size = if skin == MiniPlayerSkin::Card {
            544
        } else {
            226
        };
        let art_url = track.as_ref().and_then(|t| t.artwork_url(art_size));
        let art = |side: f32, radius: f32, cx: &mut Context<Self>| {
            artwork(art_url.clone(), px(side), px(radius), cx)
        };
        let theme = cx.theme();
        let (mut fg, mut muted, bg, mut track_bg) = (
            theme.foreground,
            theme.muted_foreground,
            theme.background,
            theme.muted,
        );
        if backdrop.is_some() {
            // Text on the darkened artwork, like Now Playing (theme palette).
            let palette = crate::theme::AppPalette::get(cx);
            fg = palette.on_artwork;
            muted = palette.on_artwork_muted;
            track_bg = palette.on_artwork.opacity(0.22);
        }
        let accent = crate::theme::AppColors::get(cx).accent;
        let playing = status == PlaybackStatus::Playing;
        let can_toggle = has_engine
            && matches!(
                status,
                PlaybackStatus::Playing | PlaybackStatus::Paused | PlaybackStatus::Ended
            );
        let total = duration.unwrap_or_default();
        let shown = scrub.unwrap_or(position.as_secs_f32()).max(0.);
        let frac = if total.is_zero() {
            0.
        } else {
            (shown / total.as_secs_f32()).clamp(0., 1.)
        };
        // The bar's look, or the plain line when the mini player doesn't follow it.
        let (style, effect) = if bar.mini_follows_bar {
            (
                bar.progress_style,
                bar.background_effect.then_some(bar.background),
            )
        } else {
            (ProgressStyle::Standard, None)
        };
        let wants_spectrum = (style == ProgressStyle::Equalizer
            || effect == Some(crate::settings::bar_prefs::BarBackground::Pulse))
            && crate::effects::spectrum_allowed(style == ProgressStyle::Equalizer)
            && (playing || self.progress.level() > 0.);
        if wants_spectrum {
            read_app(cx, |app, _| app.want_spectrum());
        }
        let analyzer = || {
            read_app(cx, |app, _| {
                app.audio_player().map(|p| p.spectrum_analyzer())
            })
            .flatten()
        };
        let fx = self
            .progress
            .step(&mut frame, style, playing, wants_spectrum, analyzer, 0.8);
        let waveform = waveform.filter(|_| style == ProgressStyle::Waveform);
        let wave_fade = if style == ProgressStyle::Waveform {
            self.progress.wave_step(
                &mut frame,
                track.as_ref().map(|t| t.video_id.as_str()),
                waveform.is_some(),
            )
        } else {
            0.
        };
        let surface = bg;
        let (backdrop_img, art_colors, hires_image) = match hires_art {
            Some((b, c, a)) => (Some(b), c, Some(a)),
            None => (None, None, None),
        };
        let tones = crate::components::bar_fx::ArtTones::new(
            art_colors.as_ref(),
            &crate::theme::AppPalette::get(cx),
        );
        let level = self.progress.level();
        let mut look = BackgroundLook::from_config(&bar, playing, level);
        look.style = effect;
        let layers = self.background.layers(
            &mut frame,
            look,
            &BackgroundArt {
                // The artwork style already shows it; the glow then only tints.
                backdrop: backdrop_img.filter(|_| backdrop.is_none()),
                tones,
                accent,
                surface,
            },
            BackgroundGeometry {
                width: skin.size().0,
                info_end: 96.,
                center: (96., skin.size().0 - 110.),
            },
        );
        let total_secs = total.as_secs_f32();
        let enabled = track.is_some() && !total.is_zero();
        let track_el = ProgressTrack::new(
            "mini-progress",
            TrackParams {
                style,
                frac,
                hover: self.progress.hover.filter(|_| !self.progress.dragging()),
                reveal: fx.reveal,
                amplitude: fx.amplitude,
                phase: fx.phase,
                spectrum: fx.spectrum,
                peak_caps: bar.peak_caps,
                waveform,
                wave_fade,
                enabled,
                look: TrackLook {
                    played: accent,
                    rail: track_bg,
                    thumb: fg,
                    ghost: accent.opacity(0.45),
                },
                height: px(14.),
                scale: 0.8,
            },
            self.progress.input.clone(),
        )
        .bounds_into(self.progress.bounds.clone())
        .on_event(std::rc::Rc::new({
            let this = cx.entity().downgrade();
            move |event, _, cx| {
                let seconds = |f: f32| (f * total_secs).clamp(0., total_secs);
                match event {
                    TrackEvent::Hover(at) => {
                        this.update(cx, |this, cx| {
                            this.progress.hover = at;
                            cx.notify();
                        })
                        .ok();
                    }
                    TrackEvent::Press(f) | TrackEvent::Drag(f) => {
                        update_app(cx, |app, cx| {
                            app.scrub = Some(seconds(f));
                            cx.notify();
                        });
                    }
                    TrackEvent::Release(f) => {
                        update_app(cx, |app, cx| {
                            app.scrub = None;
                            app.seek_to(std::time::Duration::from_secs_f32(seconds(f)), cx);
                        });
                    }
                    TrackEvent::Scroll(_) => {}
                }
            }
        }));
        let (title, artists) = match &track {
            Some(t) => (t.title.clone(), t.artists.clone()),
            None => (
                t!("common.not_playing").into(),
                crate::identity::APP_NAME.into(),
            ),
        };
        let parts = Parts {
            playing,
            loading: status == PlaybackStatus::Loading,
            can_toggle,
            has_track: track.is_some(),
            has_next,
        };
        let shown_label = format_duration(shown as u64);
        let total_label = if total.is_zero() {
            "--:--".to_owned()
        } else {
            format_duration(total.as_secs())
        };
        let titles = |align_center: bool, size_title: Option<gpui::Pixels>| {
            v_flex()
                .min_w_0()
                .gap_0p5()
                .when(align_center, |this| this.items_center())
                .child(
                    div()
                        .max_w_full()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .when_some(size_title, |this, size| this.text_size(size))
                        .child(title.clone()),
                )
                .child(
                    div()
                        .max_w_full()
                        .truncate()
                        .text_sm()
                        .text_color(muted)
                        .child(artists.clone()),
                )
        };
        let times_with = |track_el: gpui::AnyElement| {
            h_flex()
                .w_full()
                .gap_2()
                .text_xs()
                .text_color(muted)
                .items_center()
                .child(shown_label.clone())
                .child(div().flex_1().min_w_0().child(track_el))
                .child(total_label.clone())
        };
        let track_el = track_el.into_any_element();
        let body = match skin {
            MiniPlayerSkin::Bar => h_flex()
                .px_3()
                .gap_3()
                .child(art(72., 8., cx))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(titles(false, None))
                        .child(div().pt_1().child(times_with(track_el))),
                )
                .child(parts.transport(None, cx))
                .into_any_element(),
            MiniPlayerSkin::Compact => h_flex()
                .px_3()
                .gap_3()
                .child(art(40., 6., cx))
                .child(div().flex_1().min_w_0().child(titles(false, None)))
                .child(parts.transport(Some(false), cx))
                .into_any_element(),
            MiniPlayerSkin::Card => v_flex()
                .px_4()
                .pt_1()
                .gap_2()
                .items_center()
                .child(art(176., 12., cx))
                .child(div().w_full().child(titles(true, None)))
                .child(times_with(track_el))
                .child(parts.transport(None, cx))
                .into_any_element(),
            MiniPlayerSkin::Lyrics => {
                let (now_line, next_line) = lyric.unwrap_or((None, None));
                v_flex()
                    .px_3()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_3()
                            .child(art(44., 6., cx))
                            .child(div().flex_1().min_w_0().child(titles(false, None)))
                            .child(parts.transport(None, cx)),
                    )
                    .child(
                        v_flex()
                            .h(px(58.))
                            .justify_center()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_size(px(17.))
                                    .line_height(px(22.))
                                    .font_weight(FontWeight::BOLD)
                                    .line_clamp(2)
                                    .child(
                                        now_line.filter(|l| !l.trim().is_empty()).unwrap_or_else(
                                            || {
                                                if track.is_some() {
                                                    "♪".to_owned()
                                                } else {
                                                    String::new()
                                                }
                                            },
                                        ),
                                    ),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(muted)
                                    .truncate()
                                    .children(next_line),
                            ),
                    )
                    .child(times_with(track_el))
                    .into_any_element()
            }
            MiniPlayerSkin::Turntable => {
                let rest = if playing { 0. } else { 1. };
                self.arm.animate_to(rest, crate::turntable::LIFT, now);
                let lift = frame.get(&self.arm);
                let angle = crate::turntable::arm_angle(frac, lift);
                v_flex()
                    .px_4()
                    .gap_2()
                    .items_center()
                    .child(crate::turntable::deck(
                        px(196.),
                        hires_image.clone(),
                        angle,
                        &crate::turntable::DeckLook::default(),
                    ))
                    .child(div().w_full().child(titles(true, None)))
                    .child(times_with(track_el))
                    .child(parts.transport(None, cx))
                    .into_any_element()
            }
        };
        let compact_line = (skin == MiniPlayerSkin::Compact).then(|| {
            div()
                .absolute()
                .left_0()
                .bottom_0()
                .h(px(2.))
                .w(gpui::relative(frac))
                .bg(accent)
        });
        let animating = frame.is_animating();
        if let Some(at) = frame.finish(window) {
            self.schedule(at, cx);
        } else if !animating {
            self.wake = None;
        }
        if let Some(rate) = &mut self.rate {
            rate.record(now, animating);
        }

        div()
            .id("mini-player")
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .bg(bg)
            .text_color(fg)
            .when_some(backdrop, |this, image| {
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .child(
                            gpui::img(image)
                                .size_full()
                                .object_fit(gpui::ObjectFit::Cover),
                        )
                        .child(div().absolute().inset_0().bg(gpui::hsla(0., 0., 0., 0.35))),
                )
            })
            .children(layers)
            .child(title_strip(cx))
            .child(body)
            .children(compact_line)
            // The wheel changes the volume, as over the bar's volume control.
            .on_scroll(|event, _, cx| {
                let lines = crate::components::player_bar::scroll_lines(event);
                if lines == 0. {
                    return;
                }
                with_main_window(cx, |app, window, cx| {
                    let step = crate::components::player_bar::SCROLL_VOLUME_STEP;
                    let target = (app.volume + lines * step).clamp(0., 1.);
                    app.set_volume((target * 100.).round() / 100., window, cx);
                });
            })
            .into_any_element()
    }
}

/// What the transport buttons need.
struct Parts {
    playing: bool,
    loading: bool,
    can_toggle: bool,
    has_track: bool,
    has_next: bool,
}

impl Parts {
    /// Previous / play / next; `Some(false)` leaves out "previous" (compact).
    fn transport(&self, previous: Option<bool>, cx: &App) -> gpui::Div {
        h_flex()
            .flex_none()
            .gap_1()
            .when(previous != Some(false), |this| {
                this.child(
                    Button::new("mini-prev")
                        .custom(crate::components::player_bar::bar_button_style(cx))
                        .small()
                        .icon(AppIcon::Previous)
                        .disabled(!self.has_track)
                        .on_click(|_, _, cx| {
                            update_app(cx, |app, cx| app.previous_track(cx));
                        }),
                )
            })
            .child(
                Button::new("mini-play")
                    .primary()
                    .rounded(px(999.))
                    .icon(if self.playing {
                        IconName::Pause
                    } else {
                        IconName::Play
                    })
                    .loading(self.loading)
                    .disabled(!self.can_toggle)
                    .on_click(|_, _, cx| {
                        update_app(cx, |app, cx| app.toggle_playback(cx));
                    }),
            )
            .child(
                Button::new("mini-next")
                    .custom(crate::components::player_bar::bar_button_style(cx))
                    .small()
                    .icon(AppIcon::Next)
                    .disabled(!self.has_next)
                    .on_click(|_, _, cx| {
                        update_app(cx, |app, cx| app.next_track(cx));
                    }),
            )
    }
}

/// The top strip: the expand button on the right. On macOS the strip is the
/// (transparent) title bar with the close button at its left; elsewhere the window
/// has no title bar, so the strip draws a close button and is the drag area.
fn title_strip(cx: &App) -> gpui::AnyElement {
    let expand = Button::new("mini-expand")
        .custom(crate::components::player_bar::bar_button_style(cx))
        .xsmall()
        .icon(IconName::Maximize)
        .tooltip(t!("mini.tooltip.show_main"))
        .on_click(|_, _, cx| {
            cx.activate(true);
            with_main_window(cx, |_, window, _| window.activate_window());
        });
    if cfg!(target_os = "macos") {
        return h_flex()
            .h(px(28.))
            .pl(px(72.))
            .pr_1()
            .justify_end()
            .child(expand)
            .into_any_element();
    }
    h_flex()
        .h(px(28.))
        .px_1()
        .items_center()
        .child(
            Button::new("mini-close")
                .custom(crate::components::player_bar::bar_button_style(cx))
                .xsmall()
                .icon(IconName::Close)
                .tooltip(t!("common.close"))
                .on_click(|_, window, _| window.remove_window()),
        )
        .child(
            div()
                .id("mini-drag")
                .flex_1()
                .h_full()
                // Windows: the hit test answers "caption", so Windows moves it.
                .window_control_area(gpui::WindowControlArea::Drag)
                // Linux (client-side decorations): ask the compositor to move it. Not
                // on Windows, where a handled press would stop the caption drag.
                .when(cfg!(target_os = "linux"), |this| {
                    this.on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
                        window.start_window_move()
                    })
                }),
        )
        .child(expand)
        .into_any_element()
}

/// NSPanels hide when the app deactivates by default; a mini player shouldn't.
#[cfg(target_os = "macos")]
fn keep_visible_when_inactive(window: &Window) {
    use objc2::msg_send;
    use objc2::runtime::{AnyObject, Bool};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    if let RawWindowHandle::AppKit(appkit) = handle.as_raw() {
        let view = appkit.ns_view.as_ptr().cast::<AnyObject>();
        // SAFETY: `ns_view` is the live content view of the window being created
        // (main thread); `window` and `setHidesOnDeactivate:` are standard NSView /
        // NSWindow messages.
        unsafe {
            let ns_window: *mut AnyObject = msg_send![view, window];
            if !ns_window.is_null() {
                let _: () = msg_send![ns_window, setHidesOnDeactivate: Bool::NO];
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn keep_visible_when_inactive(_: &Window) {}

/// `MUSIC_ON_TOP=1` (measurements): keeps the main window above other apps' windows
/// and on every Space (also over a full-screen app), so macOS keeps delivering
/// display-link frames while the machine is in use (an occluded or off-Space window
/// gets none, which makes CPU samples meaningless).
#[cfg(target_os = "macos")]
pub fn float_above_others(window: &Window) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    if let RawWindowHandle::AppKit(appkit) = handle.as_raw() {
        let view = appkit.ns_view.as_ptr().cast::<AnyObject>();
        // SAFETY: as in `keep_visible_when_inactive`; `setLevel:` takes an NSInteger
        // (3 = NSFloatingWindowLevel), `setCollectionBehavior:` an NSUInteger
        // (canJoinAllSpaces | fullScreenAuxiliary).
        unsafe {
            let ns_window: *mut AnyObject = msg_send![view, window];
            if !ns_window.is_null() {
                let _: () = msg_send![ns_window, setLevel: 3isize];
                let _: () =
                    msg_send![ns_window, setCollectionBehavior: (1usize << 0) | (1usize << 8)];
                let _: () = msg_send![ns_window, orderFrontRegardless];
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn float_above_others(_: &Window) {}

impl MiniPlayer {
    fn schedule(&mut self, at: std::time::Instant, cx: &mut Context<Self>) {
        let now = std::time::Instant::now();
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
                .timer(delay.max(std::time::Duration::from_millis(1)))
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

impl MusicApp {
    /// The blurred artwork behind the mini player ("Blurred artwork" style), once ready.
    fn mini_backdrop(&self, cx: &App) -> Option<std::sync::Arc<gpui::RenderImage>> {
        let style = self.settings.read(cx).settings().now_playing.mini_player;
        if style != crate::settings::prefs::MiniPlayerStyle::Artwork {
            return None;
        }
        match self.hires.get(self.current_video_id()?) {
            Some(crate::now_playing::HiRes::Ready { backdrop, .. }) => Some(backdrop.clone()),
            _ => None,
        }
    }
}
