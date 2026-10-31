//! Theater mode (after Limusic's): the playing song filling the window, chrome
//! free. The music video when there is one (`video.rs`), else the artwork over
//! its blurred backdrop; the current lyric line over it; and a few controls
//! (leave, title, video toggle, progress, transport) that fade out after
//! [`CONTROLS_IDLE`] of no pointer movement while the music plays, and come
//! back with any movement. Entered from Now Playing's header button or the
//! "Theater mode" shortcut (⌘⇧T / Ctrl+Shift+T); Esc or the button leave, back
//! to wherever it was entered from.
//!
//! Cheaper than Now Playing: while it is open nothing else of the window is
//! built (no shell, no Now Playing underneath), and a render is the picture,
//! two text lines and the controls.
//!
//! Script actions: `@theater[=on|off]` (no argument toggles), `@theater-move`
//! (a pointer movement: the controls show), `@theater-status`.

use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, MouseMoveEvent,
    ObjectFit, ParentElement as _, Styled as _, StyledImage as _, Window, div, img,
    linear_color_stop, linear_gradient, prelude::FluentBuilder as _, px, relative,
};
use gpui_component::{
    IconName, Selectable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

use crate::anim::{Frame, Spec, Tween, ease_in_out_cubic};
use crate::app::{MusicApp, update_app};
use crate::icons::Lucide;
use crate::now_playing::{HiRes, ink};

/// Controls fade out this long after the last pointer movement (while playing).
pub const CONTROLS_IDLE: Duration = Duration::from_millis(3000);
const FADE: Spec = Spec::ms(400, ease_in_out_cubic);

/// Open or not, and when the controls go.
pub struct Theater {
    pub open: bool,
    /// The last pointer movement or key.
    last_activity: Option<Instant>,
    /// Opacity of the controls, and where it's heading (shown or not).
    controls: Tween<f32>,
    shown: bool,
}

impl Default for Theater {
    fn default() -> Self {
        Self {
            open: false,
            last_activity: None,
            controls: Tween::new(1.),
            shown: true,
        }
    }
}

impl Theater {
    pub fn enter(&mut self, now: Instant) {
        self.open = true;
        self.last_activity = Some(now);
        self.controls = Tween::new(1.);
        self.shown = true;
    }

    pub fn leave(&mut self) {
        self.open = false;
        self.last_activity = None;
    }

    /// The pointer moved: the controls come back. Returns whether that changes
    /// what's on screen (they were hidden or fading).
    pub fn activity(&mut self, now: Instant) -> bool {
        self.last_activity = Some(now);
        let hidden = !self.shown;
        if hidden {
            self.shown = true;
            self.controls.animate_to(1., FADE, now);
        }
        hidden
    }

    /// Whether the controls should be on screen: always while paused (a still
    /// screen without controls reads as frozen), else until idle.
    pub fn controls_wanted(&self, now: Instant, playing: bool) -> bool {
        !playing
            || self
                .last_activity
                .is_none_or(|at| now.saturating_duration_since(at) < CONTROLS_IDLE)
    }

    /// When the controls go, if they are to (for a timed re-render).
    pub fn hide_at(&self, playing: bool) -> Option<Instant> {
        if !playing || !self.shown {
            return None;
        }
        self.last_activity.map(|at| at + CONTROLS_IDLE)
    }

    /// Drives the fade for this render; returns the controls' opacity.
    fn controls_opacity(&mut self, frame: &mut Frame, playing: bool) -> f32 {
        let want = self.controls_wanted(frame.now, playing);
        if want != self.shown {
            self.shown = want;
            self.controls
                .animate_to(if want { 1. } else { 0. }, FADE, frame.now);
        }
        if let Some(at) = self.hide_at(playing) {
            frame.wake_at(at);
        }
        frame.get(&self.controls)
    }
}

impl MusicApp {
    pub(crate) fn toggle_theater(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_theater(!self.theater.open, window, cx);
    }

    pub(crate) fn set_theater(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        if on == self.theater.open {
            return;
        }
        if on {
            let Some(track) = self.playback.track.clone() else {
                return;
            };
            self.ensure_lyrics(&track, cx);
            self.ensure_hires(&track, cx);
            self.theater.enter(std::time::Instant::now());
        } else {
            self.theater.leave();
        }
        // Keys (Esc, the shortcuts) go through the app's own context.
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn theater_activity(&mut self, cx: &mut Context<Self>) {
        if self.theater.activity(std::time::Instant::now()) {
            cx.notify();
        }
    }

    pub(crate) fn render_theater(
        &mut self,
        frame: &mut Frame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // A cinema: black in light mode too, so the Now Playing parts it reuses
        // draw their dark-mode colours.
        crate::theme::on_dark(|| self.render_theater_dark(frame, window, cx))
    }

    fn render_theater_dark(
        &mut self,
        frame: &mut Frame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let track = self.playback.track.clone();
        let position = self.clock.position(frame.now);
        let playing = self.clock.is_running();
        let viewport = window.viewport_size();
        let opacity = self.theater.controls_opacity(frame, playing);

        // The picture: the video, else the artwork on its blurred backdrop.
        let picture: AnyElement = match self.video_element(ObjectFit::Contain, frame.now) {
            Some(video) => div().absolute().inset_0().child(video).into_any_element(),
            None => {
                let hires = track.as_ref().and_then(|t| self.hires.get(&t.video_id));
                let backdrop = match hires {
                    Some(HiRes::Ready { backdrop, .. }) => Some(backdrop.clone()),
                    _ => None,
                };
                let art = self.art_image(track.as_ref(), cx);
                let side = (viewport.height * 0.62).min(viewport.width * 0.5);
                div()
                    .absolute()
                    .inset_0()
                    .children(backdrop.map(|b| {
                        img(b)
                            .absolute()
                            .inset_0()
                            .size_full()
                            .object_fit(ObjectFit::Cover)
                    }))
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .pb(viewport.height * 0.12)
                            .children(art.map(|art| {
                                img(art)
                                    .size(side)
                                    .rounded(side * 0.03)
                                    .object_fit(ObjectFit::Cover)
                            })),
                    )
                    .into_any_element()
            }
        };

        // The lyric line over the picture's lower part.
        let (now_line, next_line) = self
            .lyric_snippet(track.as_ref(), position, frame)
            .unwrap_or((None, None));
        let lyrics = v_flex()
            .absolute()
            .left_0()
            .right_0()
            .bottom(px(132.))
            .px(px(64.))
            .items_center()
            .gap_1()
            .children(now_line.map(|line| {
                div()
                    .max_w(px(1100.))
                    .text_center()
                    .text_size(px(34.))
                    .line_height(px(44.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(gpui::white())
                    .child(line)
            }))
            .children(next_line.map(|line| {
                div()
                    .max_w(px(1000.))
                    .text_center()
                    .text_size(px(20.))
                    .text_color(ink(0.55))
                    .child(line)
            }));

        let (title, artists) = Self::titles(track.as_ref());
        let video_toggle = self.video_available(cx).then(|| {
            let showing = !self.video.session_off;
            Button::new("theater-video")
                .ghost()
                .text_color(gpui::white())
                .rounded(px(999.))
                .icon(if showing {
                    Lucide::VideoOff
                } else {
                    Lucide::Video
                })
                .selected(showing)
                .tooltip(if showing {
                    t!("video.hide")
                } else {
                    t!("video.show")
                })
                .on_click(|_, _, cx| {
                    update_app(cx, |app, cx| app.toggle_video_session(cx));
                })
        });
        // macOS keeps its traffic lights in the custom bar's place: start after them.
        let lights = cfg!(target_os = "macos") && crate::titlebar::custom();
        let top = h_flex()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .pl(px(if lights { 86. } else { 20. }))
            .pr_5()
            .pt(px(if lights { 4. } else { 14. }))
            .pb_8()
            .gap_3()
            .bg(linear_gradient(
                180.,
                linear_color_stop(gpui::black().opacity(0.55), 0.),
                linear_color_stop(gpui::black().opacity(0.), 1.),
            ))
            .child(
                Button::new("theater-exit")
                    .ghost()
                    .text_color(gpui::white())
                    .rounded(px(999.))
                    .icon(IconName::Close)
                    .tooltip(t!("video.theater_exit"))
                    .on_click(|_, window, cx| {
                        update_app(cx, |app, cx| app.set_theater(false, window, cx));
                    }),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .truncate()
                            .child(title),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(ink(0.7))
                            .truncate()
                            .child(artists),
                    ),
            )
            .children(video_toggle);
        // Light mode's accent is a dark tone: on black, white reads.
        let fill = if crate::theme::AppPalette::get(cx).light {
            ink(0.92)
        } else {
            crate::theme::AppColors::get(cx).np_progress
        };
        let bottom = v_flex()
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .px(px(48.))
            .pt_8()
            .pb_4()
            .gap_1()
            .bg(linear_gradient(
                0.,
                linear_color_stop(gpui::black().opacity(0.6), 0.),
                linear_color_stop(gpui::black().opacity(0.), 1.),
            ))
            .child(self.progress_bar(position, px(4.), ink(0.25), fill, cx))
            .child(self.times_row(position, ink(0.65)))
            .child(
                h_flex()
                    .w_full()
                    .justify_center()
                    .child(self.transport_view(0.85, frame, cx)),
            );

        div()
            .id("theater")
            .key_context("Theater")
            .absolute()
            .inset_0()
            .occlude()
            .overflow_hidden()
            .bg(gpui::black())
            .text_color(gpui::white())
            .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, _, cx| {
                this.theater_activity(cx);
            }))
            .child(picture)
            // A shade under the lyric line, so it reads on a bright picture too.
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(relative(0.45))
                    .bg(linear_gradient(
                        0.,
                        linear_color_stop(gpui::black().opacity(0.55), 0.),
                        linear_color_stop(gpui::black().opacity(0.), 1.),
                    )),
            )
            .child(lyrics)
            .when(opacity > 0.01, |this| {
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .opacity(opacity)
                        .child(top)
                        .child(bottom),
                )
            })
            .into_any_element()
    }

    /// `@theater`, `@theater-move`, `@theater-status`; `false` if `name` isn't one.
    pub(crate) fn theater_script_action(
        &mut self,
        name: &str,
        arg: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match name {
            "theater" => match arg {
                None => self.toggle_theater(window, cx),
                Some(v) => self.set_theater(v != "off", window, cx),
            },
            "theater-move" => self.theater_activity(cx),
            "theater-status" => {
                let now = std::time::Instant::now();
                println!(
                    "music-gpui: theater {} controls {} video {}",
                    if self.theater.open { "open" } else { "closed" },
                    if self.theater.controls_wanted(now, self.clock.is_running()) {
                        "shown"
                    } else {
                        "hidden"
                    },
                    if self.video.showing() { "on" } else { "off" },
                );
            }
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_fade_while_playing_and_come_back() {
        let t0 = Instant::now();
        let mut t = Theater::default();
        assert!(!t.open);
        t.enter(t0);
        assert!(t.open);
        assert!(t.controls_wanted(t0, true));
        assert_eq!(t.hide_at(true), Some(t0 + CONTROLS_IDLE));
        let later = t0 + CONTROLS_IDLE + Duration::from_millis(1);
        assert!(!t.controls_wanted(later, true));
        // Paused: they stay.
        assert!(t.controls_wanted(later, false));
        assert_eq!(t.hide_at(false), None);
        // Hidden, then the pointer moves: back, and the idle time starts over.
        t.shown = false;
        assert!(t.activity(later));
        assert!(t.controls_wanted(later + Duration::from_secs(2), true));
        assert_eq!(t.hide_at(true), Some(later + CONTROLS_IDLE));
        // Moving while they show changes nothing on screen.
        assert!(!t.activity(later + Duration::from_millis(10)));
        t.leave();
        assert!(!t.open);
    }
}
