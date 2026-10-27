//! Compact always-on-top player window (⇧⌘M, View menu, bar button): artwork,
//! title / artist, previous / play / next, progress. It reads everything from
//! `MusicApp` (re-rendering when the app or the artwork cache notifies, i.e. at
//! the 250 ms poll rate — no animation frames) and sends commands back through
//! [`update_app`]. Its position is saved in the settings file.
//!
//! macOS: a floating `NSPanel` (GPUI `WindowKind::Floating`) whose
//! `hidesOnDeactivate` is turned off, so it stays visible over other apps.

use gpui::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, FocusHandle, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _, Subscription,
    TitlebarOptions, Window, WindowBounds, WindowKind, WindowOptions, div, point, px, relative,
    size,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconName, Root, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use music_core::{PlaybackStatus, format_duration};

use crate::actions::with_main_window;
use crate::app::{MusicApp, read_app, update_app};
use crate::components::artwork::{ArtworkCache, artwork};
use crate::icons::AppIcon;
use crate::settings::WindowGeometry;

pub const SIZE: (f32, f32) = (400., 124.);

pub struct MiniPlayer {
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl MusicApp {
    pub fn toggle_mini_player(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.mini.take() {
            handle
                .update(cx, |_, window, _| window.remove_window())
                .ok();
            cx.notify();
            return;
        }
        let geometry = self.settings.read(cx).settings().mini_player;
        // Opening a window builds its view, which reads the app: do it after this update.
        cx.defer(move |cx| {
            let handle = open(geometry, cx);
            update_app(cx, |app, cx| {
                app.mini = handle;
                cx.notify();
            });
        });
    }
}

fn open(geometry: Option<WindowGeometry>, cx: &mut App) -> Option<AnyWindowHandle> {
    let size = size(px(SIZE.0), px(SIZE.1));
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
            title: Some("Mini player".into()),
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
            let view = cx.new(|cx| MiniPlayer::new(window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .map_err(|e| eprintln!("music-gpui: mini player: {e}"))
        .ok()?;
    Some(window.into())
}

impl MiniPlayer {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
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
        Self {
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }
}

impl Render for MiniPlayer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let info = read_app(cx, |app, _| {
            let now = std::time::Instant::now();
            (
                app.playback.track.clone(),
                app.playback.status.clone(),
                app.clock.position(now),
                app.playback.duration,
                app.queue.has_next(),
                app.engine.is_some(),
            )
        });
        let Some((track, status, position, duration, has_next, has_engine)) = info else {
            return div().size_full().into_any_element();
        };
        let art = artwork(
            track.as_ref().and_then(|t| t.artwork_url(226)),
            px(72.),
            px(8.),
            cx,
        );
        let theme = cx.theme();
        let (fg, muted, bg, track_bg) = (
            theme.foreground,
            theme.muted_foreground,
            theme.background,
            theme.muted,
        );
        let accent = crate::theme::AppColors::get(cx).accent;
        let playing = status == PlaybackStatus::Playing;
        let can_toggle = has_engine
            && matches!(
                status,
                PlaybackStatus::Playing | PlaybackStatus::Paused | PlaybackStatus::Ended
            );
        let total = duration.unwrap_or_default();
        let frac = if total.is_zero() {
            0.
        } else {
            (position.as_secs_f32() / total.as_secs_f32()).clamp(0., 1.)
        };
        let (title, artists) = match &track {
            Some(t) => (t.title.clone(), t.artists.clone()),
            None => ("Not playing".into(), "Music".into()),
        };

        div()
            .id("mini-player")
            .track_focus(&self.focus)
            .size_full()
            .bg(bg)
            .text_color(fg)
            .child(
                // Title-bar strip (draggable natively; the close button sits at its left).
                h_flex().h(px(28.)).pl(px(72.)).pr_1().justify_end().child(
                    Button::new("mini-expand")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Maximize)
                        .tooltip("Show the main window")
                        .on_click(|_, _, cx| {
                            cx.activate(true);
                            with_main_window(cx, |_, window, _| window.activate_window());
                        }),
                ),
            )
            .child(
                h_flex()
                    .px_3()
                    .gap_3()
                    .child(art)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                div()
                                    .truncate()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .child(div().truncate().text_sm().text_color(muted).child(artists))
                            .child(
                                h_flex()
                                    .pt_1()
                                    .gap_2()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format_duration(position.as_secs()))
                                    .child(
                                        div().flex_1().h(px(4.)).rounded_full().bg(track_bg).child(
                                            div()
                                                .h_full()
                                                .w(relative(frac))
                                                .rounded_full()
                                                .bg(accent),
                                        ),
                                    )
                                    .child(if total.is_zero() {
                                        "--:--".to_owned()
                                    } else {
                                        format_duration(total.as_secs())
                                    }),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_1()
                            .child(
                                Button::new("mini-prev")
                                    .ghost()
                                    .small()
                                    .icon(AppIcon::Previous)
                                    .disabled(track.is_none())
                                    .on_click(|_, _, cx| {
                                        update_app(cx, |app, cx| app.previous_track(cx));
                                    }),
                            )
                            .child(
                                Button::new("mini-play")
                                    .primary()
                                    .rounded(px(999.))
                                    .icon(if playing {
                                        IconName::Pause
                                    } else {
                                        IconName::Play
                                    })
                                    .loading(status == PlaybackStatus::Loading)
                                    .disabled(!can_toggle)
                                    .on_click(|_, _, cx| {
                                        update_app(cx, |app, cx| app.toggle_playback(cx));
                                    }),
                            )
                            .child(
                                Button::new("mini-next")
                                    .ghost()
                                    .small()
                                    .icon(AppIcon::Next)
                                    .disabled(!has_next)
                                    .on_click(|_, _, cx| {
                                        update_app(cx, |app, cx| app.next_track(cx));
                                    }),
                            ),
                    ),
            )
            .into_any_element()
    }
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
