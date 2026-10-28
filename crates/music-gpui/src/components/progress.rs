// The wavy style follows ArchiveTune's WavySliderExpressive.kt (sine wave on the
// played part that calms down to a line when paused, a gap around a pill thumb,
// a stop dot at the end) — © Rukamori and contributors, GPL-3.0.
//! The progress / volume track used by the player bar and the mini player: one
//! custom element that draws every [`ProgressStyle`] with quads and paths
//! (Standard, Thick, Simple, Wavy, Equalizer) and reports pointer input
//! (hover, press, drag, release, scroll) to its owner, which keeps the state
//! that has to survive frames (tweens, wave phase, spectrum, drag).
//!
//! The element itself is stateless and cheap: a few quads, or one path of
//! ~width / 2 segments for the wave. It never asks for frames; the owner does,
//! only while something moves.

use std::cell::{Cell, RefCell};
use std::panic::Location;
use std::rc::Rc;

use gpui::{
    App, Bounds, CursorStyle, Element, ElementId, GlobalElementId, Hitbox, HitboxBehavior, Hsla,
    InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PathBuilder, Pixels, ScrollEvent, Style, Window, fill, point, px, relative, size,
};
use music_core::Spectrum;

use crate::settings::bar_prefs::ProgressStyle;

/// Wavy: wavelength and amplitude at scale 1 (logical px); the owner moves the
/// phase by π / s, i.e. half a wavelength per second (ArchiveTune's speed).
const WAVELENGTH: f32 = 20.0;
const AMPLITUDE: f32 = 2.6;
/// Equalizer: bar width and gap at scale 1.
const EQ_BAR: f32 = 3.0;
const EQ_GAP: f32 = 2.0;

/// Colours of a track (from the theme, or white-on-artwork in the mini player).
#[derive(Clone, Copy)]
pub struct TrackLook {
    pub played: Hsla,
    pub rail: Hsla,
    pub thumb: Hsla,
    /// Between the played part and the pointer while hovering.
    pub ghost: Hsla,
}

/// What one frame of the track shows.
#[derive(Clone)]
pub struct TrackParams {
    pub style: ProgressStyle,
    /// Played fraction 0..=1 (the scrub position while dragging).
    pub frac: f32,
    /// Pointer position as a fraction while hovering (not dragging).
    pub hover: Option<f32>,
    /// Hover / drag emphasis 0..=1 (thumb size, thicker rail).
    pub reveal: f32,
    /// Wavy: 0 = flat line (paused), 1 = full wave (playing).
    pub amplitude: f32,
    /// Wavy: phase in radians (advances while playing).
    pub phase: f32,
    /// Equalizer: the current spectrum (`None` = bars at rest).
    pub spectrum: Option<Rc<RefCell<Spectrum>>>,
    pub peak_caps: bool,
    pub enabled: bool,
    pub look: TrackLook,
    /// Height of the element (= the hit area); the track is centred in it.
    pub height: Pixels,
    /// 1 = player bar; the mini player uses a little less.
    pub scale: f32,
}

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct TrackInput {
    pub hovered: bool,
    pub dragging: bool,
}

/// Pointer input on the track; positions are fractions of its width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TrackEvent {
    Hover(Option<f32>),
    Press(f32),
    Drag(f32),
    Release(f32),
    /// Scroll wheel over the track: lines, positive = up / away from the user.
    Scroll(f32),
}

pub type TrackHandler = Rc<dyn Fn(TrackEvent, &mut Window, &mut App)>;

/// How many Equalizer bars fit into `width` (also the band count to analyze).
pub fn eq_bar_count(width: Pixels, scale: f32) -> usize {
    let step = (EQ_BAR + EQ_GAP) * scale;
    (((f32::from(width) + EQ_GAP * scale) / step).floor() as usize).clamp(8, 160)
}

pub struct ProgressTrack {
    id: ElementId,
    params: TrackParams,
    input: Rc<Cell<TrackInput>>,
    on_event: Option<TrackHandler>,
    bounds_out: Option<Rc<Cell<Bounds<Pixels>>>>,
}

impl ProgressTrack {
    pub fn new(id: impl Into<ElementId>, params: TrackParams, input: Rc<Cell<TrackInput>>) -> Self {
        Self {
            id: id.into(),
            params,
            input,
            on_event: None,
            bounds_out: None,
        }
    }

    pub fn on_event(mut self, handler: TrackHandler) -> Self {
        self.on_event = Some(handler);
        self
    }

    /// Where the track was laid out (window coordinates), for the owner's tooltip.
    pub fn bounds_into(mut self, cell: Rc<Cell<Bounds<Pixels>>>) -> Self {
        self.bounds_out = Some(cell);
        self
    }
}

impl IntoElement for ProgressTrack {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for ProgressTrack {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = self.params.height.into();
        style.flex_grow = 1.;
        style.min_size.width = px(0.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) -> Hitbox {
        if let Some(cell) = &self.bounds_out {
            cell.set(bounds);
        }
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        hitbox: &mut Hitbox,
        window: &mut Window,
        _: &mut App,
    ) {
        paint_track(&self.params, bounds, window);
        if !self.params.enabled {
            return;
        }
        window.set_cursor_style(CursorStyle::PointingHand, hitbox);
        let Some(handler) = self.on_event.clone() else {
            return;
        };
        let frac_at = move |x: Pixels| {
            let w = f32::from(bounds.size.width).max(1.);
            (f32::from(x - bounds.left()) / w).clamp(0., 1.)
        };
        let input = self.input.clone();
        let hit = hitbox.clone();
        {
            let (input, handler, hit) = (input.clone(), handler.clone(), hit.clone());
            window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                if !phase.bubble() {
                    return;
                }
                let mut state = input.get();
                if state.dragging {
                    handler(TrackEvent::Drag(frac_at(e.position.x)), window, cx);
                    return;
                }
                let hovered = hit.is_hovered(window);
                if hovered {
                    state.hovered = true;
                    input.set(state);
                    handler(TrackEvent::Hover(Some(frac_at(e.position.x))), window, cx);
                } else if state.hovered {
                    state.hovered = false;
                    input.set(state);
                    handler(TrackEvent::Hover(None), window, cx);
                }
            });
        }
        {
            let (input, handler, hit) = (input.clone(), handler.clone(), hit.clone());
            window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                if !phase.bubble() || e.button != MouseButton::Left || !hit.is_hovered(window) {
                    return;
                }
                input.set(TrackInput {
                    hovered: true,
                    dragging: true,
                });
                window.capture_pointer(hit.id);
                cx.stop_propagation();
                handler(TrackEvent::Press(frac_at(e.position.x)), window, cx);
            });
        }
        {
            let (input, handler) = (input.clone(), handler.clone());
            window.on_mouse_event(move |e: &MouseUpEvent, phase, window, cx| {
                if !phase.bubble() || e.button != MouseButton::Left || !input.get().dragging {
                    return;
                }
                input.set(TrackInput {
                    hovered: bounds.contains(&e.position),
                    dragging: false,
                });
                window.release_pointer();
                handler(TrackEvent::Release(frac_at(e.position.x)), window, cx);
            });
        }
        window.on_mouse_event(move |e: &ScrollEvent, phase, window, cx| {
            if !phase.bubble() || !hit.is_hovered(window) {
                return;
            }
            let lines = match e.delta {
                gpui::ScrollDelta::Lines(p) => p.y,
                gpui::ScrollDelta::Pixels(p) => f32::from(p.y) / 20.,
            };
            if lines != 0. {
                cx.stop_propagation();
                handler(TrackEvent::Scroll(lines), window, cx);
            }
        });
    }
}

fn quad(window: &mut Window, b: Bounds<Pixels>, radius: Pixels, color: Hsla) {
    if color.a > 0. && b.size.width > px(0.) && b.size.height > px(0.) {
        window.paint_quad(fill(b, color).corner_radii(radius));
    }
}

/// A horizontal rounded bar from `x0` to `x1`, `thick` tall, centred on `cy`.
fn line(window: &mut Window, x0: f32, x1: f32, cy: f32, thick: f32, color: Hsla) {
    if x1 - x0 <= 0.01 {
        return;
    }
    quad(
        window,
        Bounds::new(
            point(px(x0), px(cy - thick / 2.)),
            size(px(x1 - x0), px(thick)),
        ),
        px(thick / 2.),
        color,
    );
}

fn dot(window: &mut Window, x: f32, cy: f32, w: f32, h: f32, color: Hsla) {
    quad(
        window,
        Bounds::new(point(px(x - w / 2.), px(cy - h / 2.)), size(px(w), px(h))),
        px(w.min(h) / 2.),
        color,
    );
}

/// Draws the track for `p` into `bounds` (no input handling; also used by tests).
pub fn paint_track(p: &TrackParams, bounds: Bounds<Pixels>, window: &mut Window) {
    let s = p.scale;
    let left = f32::from(bounds.left());
    let width = f32::from(bounds.size.width);
    let cy = f32::from(bounds.top()) + f32::from(bounds.size.height) / 2.;
    let frac = if p.enabled { p.frac.clamp(0., 1.) } else { 0. };
    let xp = left + width * frac;
    let reveal = if p.enabled {
        p.reveal.clamp(0., 1.)
    } else {
        0.
    };
    let hover_x = p.hover.filter(|_| p.enabled).map(|h| left + width * h);
    let look = p.look;
    match p.style {
        ProgressStyle::Standard | ProgressStyle::Thick | ProgressStyle::Simple => {
            let (base, grow) = match p.style {
                ProgressStyle::Thick => (8., 2.),
                ProgressStyle::Simple => (2., 1.),
                _ => (4., 2.),
            };
            let thick = (base + grow * reveal) * s;
            line(window, left, left + width, cy, thick, look.rail);
            if let Some(hx) = hover_x.filter(|&hx| hx > xp) {
                line(window, xp - thick / 2., hx, cy, thick, look.ghost);
            }
            if frac > 0. {
                line(window, left, xp.max(left + thick), cy, thick, look.played);
            }
            if reveal > 0.01 {
                match p.style {
                    ProgressStyle::Thick => {
                        let (w, h) = (4. * s, (10. + 8. * reveal) * s);
                        dot(
                            window,
                            xp.clamp(left + w / 2., left + width - w / 2.),
                            cy,
                            w,
                            h,
                            look.thumb,
                        );
                    }
                    _ => {
                        let r = if p.style == ProgressStyle::Simple {
                            4.5
                        } else {
                            6.
                        } * s
                            * reveal;
                        dot(window, xp, cy, r * 2., r * 2., look.thumb);
                    }
                }
            }
        }
        ProgressStyle::Wavy => {
            let thick = 3. * s;
            let gap = 4. * s;
            let thumb_w = 4. * s;
            let wave_end = (xp - gap - thumb_w / 2.).max(left);
            // Unplayed rail after the thumb, with the "stop" dot at the end.
            line(
                window,
                (xp + gap + thumb_w / 2.).min(left + width),
                left + width,
                cy,
                thick,
                look.rail,
            );
            dot(
                window,
                left + width - thick / 2.,
                cy,
                thick,
                thick,
                look.played,
            );
            let amp = AMPLITUDE * s * p.amplitude.clamp(0., 1.);
            if wave_end - left > 0.5 {
                if amp < 0.05 {
                    line(window, left, wave_end, cy, thick, look.played);
                } else {
                    let k = std::f32::consts::TAU / (WAVELENGTH * s);
                    let mut path = PathBuilder::stroke(px(thick));
                    let y = |x: f32| cy + amp * (k * (x - left) - p.phase).sin();
                    path.move_to(point(px(left + thick / 2.), px(y(left + thick / 2.))));
                    let step = 1.5;
                    let mut x = left + thick / 2. + step;
                    while x < wave_end - thick / 2. {
                        path.line_to(point(px(x), px(y(x))));
                        x += step;
                    }
                    let end = (wave_end - thick / 2.).max(left + thick / 2.);
                    path.line_to(point(px(end), px(y(end))));
                    if let Ok(path) = path.build() {
                        window.paint_path(path, look.played);
                    }
                    // Round caps (the stroker's are butt).
                    dot(
                        window,
                        left + thick / 2.,
                        y(left + thick / 2.),
                        thick,
                        thick,
                        look.played,
                    );
                    dot(window, end, y(end), thick, thick, look.played);
                }
            }
            if p.enabled {
                let h = (12. + 4. * reveal) * s;
                dot(
                    window,
                    xp.clamp(left + thumb_w / 2., left + width - thumb_w / 2.),
                    cy,
                    thumb_w,
                    h,
                    look.thumb,
                );
            }
        }
        ProgressStyle::Equalizer => {
            let n = eq_bar_count(bounds.size.width, s);
            let bar = EQ_BAR * s;
            let gap = if n > 1 {
                (width - bar * n as f32) / (n - 1) as f32
            } else {
                0.
            };
            let full = f32::from(bounds.size.height) - 2.;
            let rest = 3. * s;
            let spectrum = p.spectrum.as_ref().map(|s| s.borrow());
            let (bands, peaks): (&[f32], &[f32]) = match &spectrum {
                Some(sp) => (&sp.bands, &sp.peaks),
                None => (&[], &[]),
            };
            let at = |values: &[f32], i: usize| {
                if values.is_empty() {
                    0.
                } else {
                    values[(i * values.len() / n).min(values.len() - 1)]
                }
            };
            for i in 0..n {
                let x = left + i as f32 * (bar + gap);
                let center = x + bar / 2.;
                let v = at(bands, i).clamp(0., 1.);
                let h = rest + (full - rest) * v;
                let played = center <= xp;
                let hovered = hover_x.is_some_and(|hx| !played && center <= hx);
                let color = if played {
                    look.played
                } else if hovered {
                    look.ghost
                } else {
                    look.rail
                };
                dot(window, center, cy, bar, h, color);
                if p.peak_caps {
                    let peak = at(peaks, i).clamp(0., 1.);
                    let ph = rest + (full - rest) * peak;
                    if peak > 0.02 && ph > h + 2. * s {
                        let cap = (1.5 * s).max(1.);
                        let c = color.opacity(0.75);
                        quad(
                            window,
                            Bounds::new(point(px(x), px(cy - ph / 2.)), size(px(bar), px(cap))),
                            px(0.),
                            c,
                        );
                        quad(
                            window,
                            Bounds::new(
                                point(px(x), px(cy + ph / 2. - cap)),
                                size(px(bar), px(cap)),
                            ),
                            px(0.),
                            c,
                        );
                    }
                }
            }
            if reveal > 0.01 {
                let w = 2. * s;
                dot(
                    window,
                    xp.clamp(left + w / 2., left + width - w / 2.),
                    cy,
                    w,
                    full * (0.6 + 0.4 * reveal),
                    look.thumb.opacity(reveal),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_count_fills_the_width() {
        assert_eq!(eq_bar_count(px(5.), 1.), 8); // never fewer than 8
        // 98 px: 20 bars of 3 + 19 gaps of 2.
        assert_eq!(eq_bar_count(px(98.), 1.), 20);
        assert_eq!(eq_bar_count(px(100.), 1.), 20);
        assert_eq!(eq_bar_count(px(5000.), 1.), 160);
    }
}
