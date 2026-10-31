//! The app-wide artwork look's backdrop (Appearance → App-wide look →
//! Gradient wash / Glow): a few quads with linear gradients behind the
//! sidebar, header, page and queue panel, in the current track's colours.
//!
//! It's its own view, like the player bar: a new song's colours cross-fade
//! once (400 ms, as the dynamic theme), play / pause dims it once, and the
//! optional drift redraws at ~13 fps while playing, "React to the music"
//! (`ambient.rs`) at ≤ 30 fps while playing. While any of that runs the
//! app embeds the shell (and the bar) as cached views, so a backdrop frame
//! repaints the gradient and composites the cached content, without rebuilding
//! a page. Static otherwise: no frames, no timers.
//!
//! Readability: the colours are darkened until the page's muted text keeps at
//! least 4.5:1 contrast on the strongest spot ([`readable_wash`]), so no scrim
//! is needed; the sidebar and queue panel keep a translucent surface on top.

use std::cell::Cell;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, AppContext as _, Context, Hsla, IntoElement, ParentElement as _, Render, Rgba,
    Styled as _, Subscription, Task, Window, div, linear_color_stop, linear_gradient, relative,
};
use gpui_component::ActiveTheme as _;
use music_core::PlaybackStatus;

use super::ambient::{self, Reactor};
use super::bar_fx::{ArtTones, GRADIENT_INTERVAL};
use crate::anim::{Frame, Spec, Tween, ease_in_out_cubic};
use crate::app::{MusicApp, app_entity, read_app};
use crate::now_playing::HiRes;
use crate::settings::look_prefs::{AppLookStyle, EffectiveLook};
use crate::theme::{TRANSITION_MS, contrast};

/// Playing ↔ paused and on ↔ off.
const MIX_FADE: Spec = Spec::ms(350, ease_in_out_cubic);
/// A new song's colours (the dynamic theme's cross-fade length).
const COLOR_FADE: Spec = Spec::ms(TRANSITION_MS, ease_in_out_cubic);
/// Strength of a dimmed (paused) backdrop.
const PAUSED_MIX: f32 = 0.55;
/// The page's muted text keeps at least this contrast on the backdrop.
pub const MIN_TEXT_CONTRAST: f32 = 4.5;
/// One slow drift cycle every ~16 s.
const DRIFT_RATE: f32 = std::f32::consts::TAU / 16.;

/// `c` at `alpha` over `bg` (sRGB, as the GPU blends it).
pub fn over(bg: Hsla, c: Hsla, alpha: f32) -> Hsla {
    let (b, f) = (Rgba::from(bg), Rgba::from(c));
    let a = alpha.clamp(0., 1.);
    Rgba {
        r: b.r + (f.r - b.r) * a,
        g: b.g + (f.g - b.g) * a,
        b: b.b + (f.b - b.b) * a,
        a: 1.,
    }
    .into()
}

/// `c` darkened (lightened under a light theme's dark text; hue and chroma
/// kept) until `text` has `min` contrast on `c` drawn at `alpha` over `bg`.
pub fn readable_wash(c: Hsla, alpha: f32, bg: Hsla, text: Hsla, min: f32) -> Hsla {
    use crate::palette::{Lch, solve, tone_of};
    if contrast(text, over(bg, c, alpha)) >= min {
        return c;
    }
    let step = if crate::theme::is_light_color(text) {
        -2.
    } else {
        2.
    };
    let lch = Lch::from_hsla(c);
    let mut tone = tone_of(c);
    let mut out = c;
    while contrast(text, over(bg, out, alpha)) < min {
        let next = (tone + step).clamp(0., 100.);
        if next == tone {
            break;
        }
        tone = next;
        out = solve(lch.h, lch.c, tone);
    }
    out
}

/// The three colours of a backdrop (gradient start / end, glow), readable at
/// `alpha` (the strongest the look gets).
pub fn backdrop_colors(tones: &ArtTones, alpha: f32, bg: Hsla, text: Hsla) -> [Hsla; 3] {
    [tones.start, tones.end, tones.light]
        .map(|c| readable_wash(c, alpha, bg, text, MIN_TEXT_CONTRAST))
}

pub struct AppBackdrop {
    mix: Tween<f32>,
    colors: [Tween<Hsla>; 3],
    /// The last style drawn (kept while fading out).
    style: AppLookStyle,
    drift: f32,
    last: Option<Instant>,
    /// The last render asked for more frames soon.
    animating: Cell<bool>,
    /// "React to the music": the tap's reader and the energy envelope.
    react: Reactor,
    wake: Option<(Instant, Task<()>)>,
    _observe: Option<Subscription>,
}

/// What the backdrop needs from the app for one frame.
struct State {
    look: EffectiveLook,
    playing: bool,
    has_track: bool,
    tones: ArtTones,
    /// The window's background opacity over a window material.
    see_through: Option<f32>,
}

impl AppBackdrop {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let observe = app_entity(cx).map(|app| cx.observe(&app, |_, _, cx| cx.notify()));
        let grey = gpui::hsla(0., 0., 0.04, 1.);
        Self {
            mix: Tween::new(0.),
            colors: [Tween::new(grey), Tween::new(grey), Tween::new(grey)],
            style: AppLookStyle::Off,
            drift: 0.,
            last: None,
            animating: Cell::new(false),
            react: Reactor::default(),
            wake: None,
            _observe: observe,
        }
    }

    /// Redraws often right now (a fade, the drift): the app caches the rest.
    pub fn is_live(&self) -> bool {
        self.animating.get()
    }

    /// Something is (still) drawn.
    pub fn visible(&self) -> bool {
        self.mix.value(Instant::now()) > 0.001 || self.mix.is_running(Instant::now())
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

impl Render for AppBackdrop {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut frame = Frame::new();
        let now = frame.now;
        let theme = cx.theme();
        let (bg, muted) = (theme.background, theme.muted_foreground);
        let Some(state) = read_app(cx, |app, cx| {
            let settings = app.settings.read(cx).settings();
            let art = app
                .current_video_id()
                .and_then(|id| match app.hires.get(id) {
                    Some(HiRes::Ready { art_colors, .. }) => *art_colors,
                    _ => None,
                })
                .or(app.theme_state.art);
            State {
                look: settings.app_look.effective(&settings.player_bar),
                playing: app.playback.status == PlaybackStatus::Playing,
                has_track: app.playback.track.is_some(),
                tones: ArtTones::new(art.as_ref(), &crate::theme::AppPalette::get(cx)),
                see_through: app.window_see_through(cx),
            }
        }) else {
            return div().into_any_element();
        };
        let dt = self.last.map_or(0., |t| {
            now.saturating_duration_since(t).as_secs_f32().min(0.2)
        });
        self.last = Some(now);
        let look = state.look;
        let on = look.draws_backdrop() && state.has_track;
        if on {
            self.style = look.style;
        }
        let target = match () {
            _ if !on => 0.,
            _ if state.playing || !look.dim_when_paused => 1.,
            _ => PAUSED_MIX,
        };
        self.mix.animate_to(target, MIX_FADE, now);
        let mix = frame.get(&self.mix);
        let intensity = look.intensity.clamp(0.1, 1.);
        // Readable at the strongest this look gets (full mix, this intensity).
        let peak = match self.style {
            AppLookStyle::Glow => 0.9 * intensity,
            _ => intensity,
        };
        let targets = backdrop_colors(&state.tones, peak, bg, muted);
        for (tween, c) in self.colors.iter_mut().zip(targets) {
            // Dimmed: less saturated as well as fainter.
            let c = if !state.playing && look.dim_when_paused {
                gpui::hsla(c.h, c.s * 0.6, c.l, 1.)
            } else {
                c
            };
            tween.animate_to(c, COLOR_FADE, now);
        }
        let [mut a, mut b, light] = self.colors.each_ref().map(|t| frame.get(t));
        // The fade layers end in the window's background (translucent over a
        // window material, so the desktop keeps showing through).
        let base = bg.opacity(state.see_through.unwrap_or(1.));

        // React to the music: the strength follows the energy (never above the
        // look's own, so the readable colours above stay readable).
        let ambient_ok = crate::effects::ambient_motion();
        let energy = if look.reacts() && ambient_ok && mix > 0.001 {
            let listening = state.playing;
            if listening {
                read_app(cx, |app, _| app.want_spectrum());
            }
            let e = self.react.step(now, listening, || {
                read_app(cx, |app, _| {
                    app.audio_player().map(|p| p.spectrum_analyzer())
                })
                .flatten()
            });
            if let Some(after) =
                ambient::next_frame(true, state.playing, ambient_ok, &self.react.envelope)
            {
                frame.wake_in(after);
            }
            Some(e)
        } else {
            self.react.reset();
            None
        };
        let boost = energy.map_or(1., ambient::strength);

        let mut layers: Vec<AnyElement> = Vec::new();
        if mix > 0.001 {
            match self.style {
                AppLookStyle::Gradient => {
                    let mut angle = look.direction.angle();
                    let (mut lo, mut hi) = (0., 1.);
                    if look.animates() {
                        if state.playing && mix >= 0.999 && crate::effects::ambient_motion() {
                            self.drift = (self.drift + dt * DRIFT_RATE) % std::f32::consts::TAU;
                            frame.wake_in(GRADIENT_INTERVAL);
                        }
                        let (s, c) = self.drift.sin_cos();
                        angle += 12. * s;
                        lo = 0.1 * (1. + c) / 2.;
                        hi = 1. - 0.1 * (1. - c) / 2.;
                        a.h = (a.h + 0.015 * s).rem_euclid(1.);
                        b.h = (b.h - 0.015 * s).rem_euclid(1.);
                    }
                    // The artwork's two main colours along the direction...
                    layers.push(
                        div()
                            .absolute()
                            .inset_0()
                            .opacity(peak * mix * boost)
                            .bg(linear_gradient(
                                angle,
                                linear_color_stop(a, lo),
                                linear_color_stop(b, hi),
                            ))
                            .into_any_element(),
                    );
                    // ...fading into the normal background towards the far end.
                    layers.push(
                        div()
                            .absolute()
                            .inset_0()
                            .bg(linear_gradient(
                                angle,
                                linear_color_stop(base.opacity(0.), 0.15),
                                linear_color_stop(base, 1.),
                            ))
                            .into_any_element(),
                    );
                }
                AppLookStyle::Glow => {
                    // A band of light along the top, strongest at the top-left
                    // (reaching further down on loud passages when reacting).
                    let reach = energy.map_or(0.6, |e| 0.5 + 0.2 * e);
                    layers.push(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .h(relative(reach))
                            .opacity(peak * mix * boost)
                            .bg(linear_gradient(
                                180.,
                                linear_color_stop(light, 0.),
                                linear_color_stop(light.opacity(0.), 1.),
                            ))
                            .into_any_element(),
                    );
                    layers.push(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .h(relative(reach))
                            .opacity(0.35 * mix)
                            .bg(linear_gradient(
                                90.,
                                linear_color_stop(base.opacity(0.), 0.35),
                                linear_color_stop(base, 1.),
                            ))
                            .into_any_element(),
                    );
                }
                AppLookStyle::Off | AppLookStyle::Tint => {}
            }
        }
        let element = div()
            .absolute()
            .inset_0()
            .overflow_hidden()
            .children(layers)
            .into_any_element();
        let animating = frame.is_animating();
        let wake = frame.finish(window);
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

impl AppBackdrop {
    /// "React to the music": frames analyzed so far and the current energy.
    pub fn react_status(&self) -> (u64, f32) {
        (self.react.frames, self.react.envelope.energy())
    }
}

impl MusicApp {
    /// The backdrop view, when the look draws one (or one is fading out).
    pub(crate) fn app_backdrop(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Entity<AppBackdrop>> {
        let settings = self.settings.read(cx).settings();
        let wanted = settings
            .app_look
            .effective(&settings.player_bar)
            .draws_backdrop();
        if !wanted
            && !self
                .look
                .backdrop
                .as_ref()
                .is_some_and(|b| b.read(cx).visible())
        {
            return None;
        }
        Some(
            self.look
                .backdrop
                .get_or_insert_with(|| cx.new(AppBackdrop::new))
                .clone(),
        )
    }

    /// The backdrop redraws often right now (see [`AppBackdrop::is_live`]).
    pub(crate) fn backdrop_is_live(&self, cx: &gpui::App) -> bool {
        self.look
            .backdrop
            .as_ref()
            .is_some_and(|b| b.read(cx).is_live())
    }

    /// The backdrop shows through the sidebar and queue panel.
    pub(crate) fn backdrop_shown(&self, cx: &gpui::App) -> bool {
        let settings = self.settings.read(cx).settings();
        self.playback.track.is_some()
            && settings
                .app_look
                .effective(&settings.player_bar)
                .draws_backdrop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::hsla;

    #[test]
    fn washes_keep_muted_text_readable() {
        let bg = hsla(0., 0., 0.039, 1.);
        let muted = hsla(0., 0., 0.64, 1.);
        for rgb in [
            [230, 40, 40],
            [250, 230, 60],
            [40, 200, 255],
            [255, 255, 255],
            [90, 40, 200],
        ] {
            let art = crate::palette::art_colors_from_pixels(&vec![rgb; 900]);
            let tones = ArtTones::new(
                art.as_ref(),
                &crate::theme::AppPalette::from_resolved(&crate::theme::Resolved::default()),
            );
            for alpha in [0.3, 0.55, 0.9, 1.0] {
                let bright = [tones.start, tones.end, tones.light].map(|c| {
                    // Even a light colour at this alpha.
                    hsla(c.h, c.s, 0.75, 1.)
                });
                for c in bright {
                    let w = readable_wash(c, alpha, bg, muted, MIN_TEXT_CONTRAST);
                    assert!(
                        contrast(muted, over(bg, w, alpha)) >= MIN_TEXT_CONTRAST - 0.01,
                        "{rgb:?} {alpha}"
                    );
                }
                for c in backdrop_colors(&tones, alpha, bg, muted) {
                    assert!(contrast(muted, over(bg, c, alpha)) >= MIN_TEXT_CONTRAST - 0.01);
                }
            }
        }
        // Dark colours are kept as they are.
        let dark = hsla(0.6, 0.5, 0.12, 1.);
        assert_eq!(readable_wash(dark, 0.5, bg, muted, 4.5), dark);
    }

    #[test]
    fn light_washes_keep_dark_text_readable() {
        // gpui-component's light background and muted text (neutral-500).
        let bg = hsla(0., 0., 1., 1.);
        let muted = hsla(0., 0., 0.451, 1.);
        let mut palette =
            crate::theme::AppPalette::from_resolved(&crate::theme::Resolved::default());
        palette.light = true;
        for rgb in [
            [230, 40, 40],
            [250, 230, 60],
            [40, 200, 255],
            [0, 0, 0],
            [90, 40, 200],
        ] {
            let art = crate::palette::art_colors_from_pixels(&vec![rgb; 900]);
            let tones = ArtTones::new(art.as_ref(), &palette);
            for alpha in [0.3, 0.55, 0.9, 1.0] {
                // Even a dark colour at this alpha.
                let dark = [tones.start, tones.end, tones.light].map(|c| hsla(c.h, c.s, 0.2, 1.));
                for c in dark {
                    let w = readable_wash(c, alpha, bg, muted, MIN_TEXT_CONTRAST);
                    assert!(
                        contrast(muted, over(bg, w, alpha)) >= MIN_TEXT_CONTRAST - 0.01,
                        "{rgb:?} {alpha}"
                    );
                }
                for c in backdrop_colors(&tones, alpha, bg, muted) {
                    assert!(contrast(muted, over(bg, c, alpha)) >= MIN_TEXT_CONTRAST - 0.01);
                }
            }
            // The bar's gradient ends and light are pale enough for its text as they are.
            let fg = hsla(0., 0., 0.039, 1.);
            for c in [tones.start, tones.end, tones.light] {
                assert!(contrast(fg, over(bg, c, 0.9)) >= 4.5, "{rgb:?}");
            }
        }
    }

    #[test]
    fn over_blends() {
        let black = hsla(0., 0., 0., 1.);
        let white = hsla(0., 0., 1., 1.);
        assert!((Rgba::from(over(black, white, 0.5)).r - 0.5).abs() < 1e-4);
        assert_eq!(over(black, white, 0.), black);
    }
}
