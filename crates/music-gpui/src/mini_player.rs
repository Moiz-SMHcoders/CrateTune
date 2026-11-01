//! Compact always-on-top player window (⇧⌘M, View menu, bar button): artwork,
//! title / artist, previous / play / next, progress (click / drag to seek). It
//! reads everything from `MusicApp` (re-rendering when the app or the artwork
//! cache notifies, i.e. at the 250 ms poll rate) and sends commands back through
//! [`update_app`]. With "Same look in the mini player" it draws the bar's
//! progress style and play / pause background (`components/bar_fx.rs`), asking
//! for its own frames only while those animate. Its position is saved in the
//! settings file.
//!
//! Its title strip has "Up next" and the always-on-top pin, a row under the
//! transport has shuffle, repeat, like and the volume (Limusic's mini player,
//! GPL-3.0, `ui/src/lib/components/MiniPlayer.svelte`; no code copied). "Up
//! next" is the artwork card's and turntable's flip side (after Deviload's mini
//! player, MIT) and a list under the wider layouts, which grow for it. A size
//! change (layout, "Up next") reopens the window anchored to the screen corner
//! it is nearest to (Limusic's `src-tauri/src/mini.rs` `anchored`: per axis, a
//! window in the far half of its display keeps its far edge).
//!
//! macOS: a floating `NSPanel` (GPUI `WindowKind::Floating`) whose
//! `hidesOnDeactivate` is turned off, so it stays visible over other apps. Windows:
//! a borderless window made topmost (`win::set_topmost`) with its own close button
//! and drag strip. Unpinned it opens as an ordinary window (reopened at the same
//! spot).

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, FocusHandle, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Point, Render,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Subscription, TitlebarOptions,
    Window, WindowBounds, WindowKind, WindowOptions, div, point, px, size,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Root, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use music_core::{PlaybackStatus, Track, format_duration};

use crate::actions::with_main_window;
use crate::anim::{Frame, RenderRate, Tween};
use crate::app::{MusicApp, read_app, update_app};
use crate::components::artwork::{ArtworkCache, artwork};
use crate::components::bar_fx::{
    BackgroundArt, BackgroundFx, BackgroundGeometry, BackgroundLook, TrackFx,
};
use crate::components::progress::{ProgressTrack, TrackEvent, TrackLook, TrackParams};
use crate::icons::{AppIcon, Lucide};
use crate::queue::Repeat;
use crate::settings::WindowGeometry;
use crate::settings::bar_prefs::ProgressStyle;
use crate::settings::prefs::MiniPlayerSkin;

/// The row of shuffle, repeat, like and the volume under the transport.
const TOOLS_ROW: f32 = 30.;
/// "Up next" under the bar, compact and lyrics layouts (they grow for it).
const QUEUE_LIST: f32 = 236.;
/// One "Up next" row.
const QUEUE_ROW: f32 = 40.;

/// The window's content size for `skin` with or without "Up next".
pub fn window_size(skin: MiniPlayerSkin, queue: bool) -> (f32, f32) {
    let (w, h) = skin.size();
    let list = if queue && grows_for_queue(skin) {
        QUEUE_LIST
    } else {
        0.
    };
    (w, h + TOOLS_ROW + list)
}

/// Layouts that show "Up next" under themselves (the others on their flip side).
fn grows_for_queue(skin: MiniPlayerSkin) -> bool {
    matches!(
        skin,
        MiniPlayerSkin::Bar | MiniPlayerSkin::Compact | MiniPlayerSkin::Lyrics
    )
}

/// The top-left corner for a window that was at `old` and is now `new` big,
/// so that it keeps to the edges of `display` it is nearest to: per axis, a
/// window whose centre is in the far half keeps its far edge (Limusic's
/// `anchored`); shrinking and growing back returns it to the same spot.
pub fn anchored(
    old: Bounds<Pixels>,
    new: gpui::Size<Pixels>,
    display: Bounds<Pixels>,
) -> Point<Pixels> {
    let axis = |p: Pixels, old: Pixels, new: Pixels, start: Pixels, len: Pixels| {
        if p * 2. + old > start * 2. + len {
            p + old - new
        } else {
            p
        }
    };
    point(
        axis(
            old.origin.x,
            old.size.width,
            new.width,
            display.origin.x,
            display.size.width,
        ),
        axis(
            old.origin.y,
            old.size.height,
            new.height,
            display.origin.y,
            display.size.height,
        ),
    )
}

pub struct MiniPlayer {
    focus: FocusHandle,
    /// The layout this window was opened with (another one reopens it).
    skin: MiniPlayerSkin,
    /// "Up next" and the pin as the window was opened (a size or window kind
    /// change reopens it).
    queue: bool,
    on_top: bool,
    volume: TrackFx,
    /// The turntable layout's turning record.
    spin: crate::turntable::Spin,
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
        let np = self.settings.read(cx).settings().now_playing;
        let look = Opened {
            skin: np.mini_skin,
            queue: np.mini_queue,
            on_top: np.mini_on_top,
        };
        // Opening a window builds its view, which reads the app: do it after this update.
        cx.defer(move |cx| {
            let handle = open(geometry, look, cx);
            update_app(cx, |app, cx| {
                app.mini = handle;
                cx.notify();
            });
        });
    }
}

/// What a mini player window is opened with.
#[derive(Clone, Copy)]
struct Opened {
    skin: MiniPlayerSkin,
    queue: bool,
    on_top: bool,
}

fn open(geometry: Option<WindowGeometry>, look: Opened, cx: &mut App) -> Option<AnyWindowHandle> {
    let Opened {
        skin,
        queue,
        on_top,
    } = look;
    let (width, height) = window_size(skin, queue);
    let size = size(px(width), px(height));
    let bounds = geometry
        .map(|g| {
            // Where it was, at its new size: kept to the nearest screen corner.
            let old = Bounds::new(
                point(px(g.x), px(g.y)),
                gpui::size(px(g.width), px(g.height)),
            );
            let display = cx
                .displays()
                .iter()
                .map(|d| d.bounds())
                .find(|d| d.contains(&old.center()))
                .or_else(|| cx.primary_display().map(|d| d.bounds()));
            let origin = match display {
                Some(display) if g.width > 0. && g.height > 0. => anchored(old, size, display),
                _ => old.origin,
            };
            Bounds::new(origin, size)
        })
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
        // Unpinned: an ordinary window.
        kind: if on_top {
            WindowKind::Floating
        } else {
            WindowKind::Normal
        },
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
            if on_top && let Some(hwnd) = crate::services::hwnd(window) {
                crate::win::set_topmost(hwnd.get());
            }
            let view = cx.new(|cx| MiniPlayer::new(look, window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .map_err(|e| eprintln!("music-gpui: mini player: {e}"))
        .ok()?;
    Some(window.into())
}

impl MiniPlayer {
    fn new(look: Opened, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
            skin: look.skin,
            queue: look.queue,
            on_top: look.on_top,
            volume: TrackFx::default(),
            spin: Default::default(),
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
            let np = app.settings.read(cx).settings().now_playing;
            let skin = np.mini_skin;
            let extras = Extras {
                volume: app.volume,
                shuffle: app.queue.shuffle(),
                repeat: app.queue.repeat(),
                liked: app
                    .playback
                    .track
                    .as_ref()
                    .is_some_and(|t| crate::store::is_liked(&t.video_id)),
                queue: np.mini_queue,
                on_top: np.mini_on_top,
                upcoming: np
                    .mini_queue
                    .then(|| {
                        app.queue
                            .upcoming()
                            .iter()
                            .take(12)
                            .map(|i| (i.uid, i.track.clone(), app.added_by_autoplay(i.uid)))
                            .collect()
                    })
                    .unwrap_or_default(),
                more: app.queue.upcoming().len(),
                spin: np.turntable_spin,
            };
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
                extras,
            )
        });
        let Some((
            (track, status, position, duration, has_next, has_engine, backdrop),
            (bar, hires_art, scrub, waveform, skin, lyric),
            extras,
        )) = info
        else {
            return div().size_full().into_any_element();
        };
        let resized = extras.queue != self.queue && (grows_for_queue(skin) || skin != self.skin);
        if skin != self.skin || extras.on_top != self.on_top || resized {
            // Another layout (or "Up next" under it) needs another window size and
            // title bar spot, unpinned another kind of window: open the mini
            // player again where it is.
            self.skin = skin;
            self.queue = extras.queue;
            self.on_top = extras.on_top;
            cx.defer(|cx| {
                update_app(cx, |app, cx| {
                    app.toggle_mini_player(cx);
                    app.toggle_mini_player(cx);
                });
            });
        }
        self.queue = extras.queue;
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
            // Text on the darkened (light mode: lightened) artwork, like Now
            // Playing (theme palette).
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
        let flip = extras.queue && !grows_for_queue(skin);
        let tools = self.tools_row(
            &mut frame,
            &extras,
            track.as_ref(),
            (fg, muted, accent, track_bg),
            cx,
        );
        let below = (extras.queue && grows_for_queue(skin)).then(|| {
            div()
                .px_3()
                .pt_1()
                .child(queue_list(&extras, QUEUE_LIST - 8., muted, cx))
        });
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
                // "Up next" is the card's flip side.
                .map(|this| {
                    if flip {
                        this.child(queue_list(&extras, 224., muted, cx))
                    } else {
                        this.child(art(176., 12., cx))
                            .child(div().w_full().child(titles(true, None)))
                    }
                })
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
                // "Spin the record" (`turntable.rs`): only while the deck shows.
                let look = if flip {
                    crate::turntable::SpinLook::Off
                } else {
                    extras.spin
                };
                self.spin.step(
                    &mut frame,
                    playing
                        && look != crate::turntable::SpinLook::Off
                        && crate::effects::ambient_motion(),
                );
                let label_px =
                    (196. * crate::turntable::LABEL_SHARE * window.scale_factor()).round() as u32;
                let art = track
                    .as_ref()
                    .zip(hires_image.clone())
                    .map(|(t, art)| (t.video_id.as_str(), art));
                let spin = self.spin.deck_spin(look, art, label_px, cx);
                v_flex()
                    .px_4()
                    .gap_2()
                    .items_center()
                    .map(|this| {
                        if flip {
                            this.child(queue_list(&extras, 244., muted, cx))
                        } else {
                            this.child(crate::turntable::deck(
                                px(196.),
                                hires_image.clone(),
                                angle,
                                &crate::turntable::DeckLook::default(),
                                &spin,
                            ))
                            .child(div().w_full().child(titles(true, None)))
                        }
                    })
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
                        // Darkened for white text; lightened for light mode's dark text.
                        .child(div().absolute().inset_0().bg(if crate::theme::is_light() {
                            crate::theme::veil(0.62)
                        } else {
                            crate::theme::veil(0.35)
                        })),
                )
            })
            .children(layers)
            .child(title_strip(extras.queue, extras.on_top, cx))
            .child(body)
            .child(tools)
            .children(below)
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

/// What the title strip, the tools row and "Up next" show.
struct Extras {
    volume: f32,
    shuffle: bool,
    repeat: Repeat,
    liked: bool,
    queue: bool,
    on_top: bool,
    /// "Up next" (when shown): queue item, track, added by autoplay.
    upcoming: Vec<(u64, Track, bool)>,
    /// All upcoming items.
    more: usize,
    /// "Spin the record".
    spin: crate::turntable::SpinLook,
}

impl MiniPlayer {
    /// Shuffle, repeat, like, and the volume (mute button and slider).
    fn tools_row(
        &mut self,
        frame: &mut Frame,
        extras: &Extras,
        track: Option<&Track>,
        (fg, muted, accent, rail): (Hsla, Hsla, Hsla, Hsla),
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let style = crate::components::player_bar::bar_button_style(cx);
        let toggle = |id: &'static str, icon: Lucide, on: bool| {
            Button::new(id)
                .custom(style)
                .xsmall()
                .icon(if on {
                    Icon::new(icon).text_color(accent)
                } else {
                    Icon::new(icon)
                })
                .selected(on)
        };
        let liked = extras.liked;
        let volume = extras.volume;
        let vol_frame =
            self.volume
                .step(frame, ProgressStyle::Standard, false, false, || None, 0.7);
        let slider = ProgressTrack::new(
            "mini-volume",
            TrackParams {
                style: ProgressStyle::Standard,
                frac: volume,
                hover: None,
                reveal: vol_frame.reveal.max(0.35),
                amplitude: 0.,
                phase: 0.,
                spectrum: None,
                peak_caps: false,
                waveform: None,
                wave_fade: 0.,
                enabled: true,
                look: TrackLook {
                    played: accent,
                    rail,
                    thumb: fg,
                    ghost: accent.opacity(0.45),
                },
                height: px(14.),
                scale: 0.7,
            },
            self.volume.input.clone(),
        )
        .bounds_into(self.volume.bounds.clone())
        .on_event(std::rc::Rc::new(|event, _, cx| match event {
            TrackEvent::Press(f) | TrackEvent::Drag(f) | TrackEvent::Release(f) => {
                with_main_window(cx, |app, window, cx| {
                    app.set_volume((f * 100.).round() / 100., window, cx)
                });
            }
            TrackEvent::Hover(_) | TrackEvent::Scroll(_) => {}
        }));
        let like_track = track.cloned();
        h_flex()
            .h(px(TOOLS_ROW))
            .px_3()
            .gap_1()
            .items_center()
            .text_color(muted)
            .child(
                toggle("mini-shuffle", Lucide::Shuffle, extras.shuffle)
                    .tooltip(if extras.shuffle {
                        t!("player.tooltip.shuffle_on")
                    } else {
                        t!("player.tooltip.shuffle_off")
                    })
                    .on_click(|_, _, cx| {
                        update_app(cx, |app, cx| app.toggle_shuffle(cx));
                    }),
            )
            .child(
                toggle(
                    "mini-repeat",
                    if extras.repeat == Repeat::One {
                        Lucide::Repeat1
                    } else {
                        Lucide::Repeat
                    },
                    extras.repeat != Repeat::Off,
                )
                .tooltip(match extras.repeat {
                    Repeat::Off => t!("player.tooltip.repeat_off"),
                    Repeat::All => t!("player.tooltip.repeat_all"),
                    Repeat::One => t!("player.tooltip.repeat_one"),
                })
                .on_click(|_, _, cx| {
                    update_app(cx, |app, cx| app.cycle_repeat(cx));
                }),
            )
            .child(
                Button::new("mini-like")
                    .custom(style)
                    .xsmall()
                    .icon(if liked {
                        Icon::new(AppIcon::HeartFilled).text_color(accent)
                    } else {
                        Icon::new(Lucide::Heart)
                    })
                    .tooltip(if liked {
                        t!("common.remove_from_liked")
                    } else {
                        t!("common.like")
                    })
                    .disabled(like_track.is_none())
                    .on_click(move |_, _, cx| {
                        if let Some(track) = like_track.clone() {
                            with_main_window(cx, |app, window, cx| {
                                app.toggle_like(track, window, cx)
                            });
                        }
                    }),
            )
            .child(div().flex_1())
            .child(
                Button::new("mini-mute")
                    .custom(style)
                    .xsmall()
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
                    .on_click(|_, _, cx| {
                        with_main_window(cx, |app, window, cx| app.mute_toggle(window, cx));
                    }),
            )
            .child(div().w(px(88.)).h(px(14.)).flex_none().child(slider))
            .into_any_element()
    }
}

/// "Up next": the next songs (click one to play it), "∞" on those autoplay
/// added, `height` px tall.
fn queue_list(
    extras: &Extras,
    height: f32,
    muted: Hsla,
    cx: &mut Context<MiniPlayer>,
) -> gpui::AnyElement {
    let fits = ((height - 24.) / QUEUE_ROW).floor().max(1.) as usize;
    let shown = extras.upcoming.iter().take(fits);
    let hover = cx.theme().list_hover;
    let rows: Vec<gpui::AnyElement> = shown
        .enumerate()
        .map(|(i, (uid, track, autoplay))| {
            let uid = *uid;
            h_flex()
                .id(("mini-queue", i))
                .w_full()
                .h(px(QUEUE_ROW))
                .flex_none()
                .px_1()
                .gap_2()
                .rounded(px(6.))
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .on_click(move |_, _, cx| {
                    update_app(cx, |app, cx| app.queue_jump(uid, cx));
                })
                .child(artwork(track.artwork_url(120), px(30.), px(4.), cx))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .truncate()
                                .child(track.title.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .truncate()
                                .child(track.artists.clone()),
                        ),
                )
                .when(*autoplay, |this| {
                    this.child(crate::player_extras::autoplay_mark(muted))
                })
                .into_any_element()
        })
        .collect();
    let hidden = extras.more.saturating_sub(rows.len());
    v_flex()
        .w_full()
        .h(px(height))
        .flex_none()
        .overflow_hidden()
        .child(
            h_flex()
                .h(px(22.))
                .px_1()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(muted)
                .child(div().flex_1().child(t!("np.queue.title")))
                .when(hidden > 0, |this| this.child(tp!("np.queue.more", hidden))),
        )
        .when(rows.is_empty(), |this| {
            this.child(
                div()
                    .px_1()
                    .text_xs()
                    .text_color(muted)
                    .child(t!("np.queue.empty")),
            )
        })
        .children(rows)
        .into_any_element()
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

/// The top strip: "Up next", the pin and the expand button on the right. On
/// macOS the strip is the (transparent) title bar with the close button at its
/// left; elsewhere the window has no title bar, so the strip draws a close
/// button and is the drag area.
fn title_strip(queue: bool, on_top: bool, cx: &App) -> gpui::AnyElement {
    let style = crate::components::player_bar::bar_button_style(cx);
    let accent = crate::theme::AppColors::get(cx).accent;
    let lit = |icon: Lucide, on: bool| {
        if on {
            Icon::new(icon).text_color(accent)
        } else {
            Icon::new(icon)
        }
    };
    let buttons = h_flex()
        .gap_0p5()
        .child(
            Button::new("mini-queue-toggle")
                .custom(style)
                .xsmall()
                .icon(lit(Lucide::ListMusic, queue))
                .selected(queue)
                .tooltip(t!("np.up_next"))
                .on_click(move |_, _, cx| {
                    update_app(cx, |app, cx| {
                        app.settings
                            .update(cx, |p, cx| p.set_mini_queue(!queue, cx))
                    });
                }),
        )
        .child(
            Button::new("mini-pin")
                .custom(style)
                .xsmall()
                .icon(lit(
                    if on_top { Lucide::Pin } else { Lucide::PinOff },
                    on_top,
                ))
                .selected(on_top)
                .tooltip(if on_top {
                    t!("np_extras.mini.unpin")
                } else {
                    t!("np_extras.mini.pin")
                })
                .on_click(move |_, _, cx| {
                    update_app(cx, |app, cx| {
                        app.settings
                            .update(cx, |p, cx| p.set_mini_on_top(!on_top, cx))
                    });
                }),
        );
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
            .child(buttons)
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
        .child(buttons)
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
