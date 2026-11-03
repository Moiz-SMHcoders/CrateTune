//! The equalizer's response graph (Settings → Equalizer, after Zuno's
//! `Equalizer.tsx`): the curve the filters really make (not a line through the
//! bands), a 0 dB line, the ten bands as points that can be dragged up and
//! down, and the part of the curve above 0 dB tinted while it could clip.
//!
//! One custom element; the curve's points come from the settings panel, which
//! recomputes them only when the gains change, so a frame draws one path of
//! ~160 segments and a few quads, and nothing draws at rest.

use std::cell::Cell;
use std::panic::Location;
use std::rc::Rc;

use gpui::{
    App, Bounds, CursorStyle, Element, ElementId, GlobalElementId, Hitbox, HitboxBehavior, Hsla,
    InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PathBuilder, Pixels, Point, Style, Window, fill, point, px, relative, size,
};

use super::file::{BANDS, EQ_RANGE_DB};
use super::sound::band_x;

/// The plot covers ±this many dB (neighbouring boosts stack past a band's ±12).
const DISPLAY_DB: f32 = 15.0;
const GRID_DB: [f32; 5] = [12.0, 6.0, 0.0, -6.0, -12.0];

/// A band moved on the graph: (band, gain in dB). `true` when the drag ends.
pub type GraphHandler = Rc<dyn Fn(usize, f64, bool, &mut Window, &mut App)>;

#[derive(Clone, Copy)]
pub struct GraphColors {
    pub line: Hsla,
    /// The curve above 0 dB while nothing makes room for it.
    pub over: Hsla,
    pub grid: Hsla,
    pub zero: Hsla,
    pub point: Hsla,
    pub point_ring: Hsla,
}

pub struct EqGraph {
    id: ElementId,
    height: Pixels,
    points: Rc<[f32]>,
    gains: [f32; BANDS],
    enabled: bool,
    /// Tint the part over 0 dB (no automatic pre-gain).
    warn: bool,
    colors: GraphColors,
    drag: Rc<Cell<Option<usize>>>,
    on_change: Option<GraphHandler>,
}

impl EqGraph {
    pub fn new(
        id: impl Into<ElementId>,
        points: Rc<[f32]>,
        gains: [f64; BANDS],
        colors: GraphColors,
        drag: Rc<Cell<Option<usize>>>,
    ) -> Self {
        Self {
            id: id.into(),
            height: px(140.),
            points,
            gains: gains.map(|g| g as f32),
            enabled: true,
            warn: false,
            colors,
            drag,
            on_change: None,
        }
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn warn(mut self, warn: bool) -> Self {
        self.warn = warn;
        self
    }

    pub fn on_change(mut self, handler: GraphHandler) -> Self {
        self.on_change = Some(handler);
        self
    }
}

impl IntoElement for EqGraph {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

/// The plot's area inside the element (room for the points at the edges).
fn plot(bounds: Bounds<Pixels>) -> Bounds<Pixels> {
    let pad = px(8.);
    Bounds::new(
        point(bounds.left() + pad, bounds.top() + pad),
        size(bounds.size.width - pad * 2., bounds.size.height - pad * 2.),
    )
}

fn y_of(plot: Bounds<Pixels>, db: f32) -> f32 {
    let top = f32::from(plot.top());
    let h = f32::from(plot.size.height);
    top + h * (0.5 - db.clamp(-DISPLAY_DB, DISPLAY_DB) / DISPLAY_DB / 2.)
}

fn db_at(plot: Bounds<Pixels>, y: Pixels) -> f64 {
    let top = f32::from(plot.top());
    let h = f32::from(plot.size.height).max(1.);
    let db = (0.5 - (f32::from(y) - top) / h) * 2. * DISPLAY_DB;
    (db as f64).clamp(-EQ_RANGE_DB, EQ_RANGE_DB)
}

fn x_of(plot: Bounds<Pixels>, frac: f32) -> f32 {
    f32::from(plot.left()) + f32::from(plot.size.width) * frac
}

/// The band whose column holds `x` (the columns split halfway between centres).
fn band_at(plot: Bounds<Pixels>, x: Pixels) -> usize {
    let x = f32::from(x);
    (0..BANDS)
        .min_by(|&a, &b| {
            let da = (x_of(plot, band_x(a)) - x).abs();
            let db = (x_of(plot, band_x(b)) - x).abs();
            da.total_cmp(&db)
        })
        .unwrap_or(0)
}

fn hline(window: &mut Window, plot: Bounds<Pixels>, y: f32, thick: f32, color: Hsla) {
    window.paint_quad(fill(
        Bounds::new(
            point(plot.left(), px(y - thick / 2.)),
            size(plot.size.width, px(thick)),
        ),
        color,
    ));
}

fn dot(window: &mut Window, c: Point<Pixels>, r: f32, color: Hsla) {
    window.paint_quad(
        fill(
            Bounds::new(
                point(c.x - px(r), c.y - px(r)),
                size(px(r * 2.), px(r * 2.)),
            ),
            color,
        )
        .corner_radii(px(r)),
    );
}

impl Element for EqGraph {
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
        style.size.height = self.height.into();
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
        let c = self.colors;
        let area = plot(bounds);
        for db in GRID_DB {
            let (color, thick) = if db == 0. {
                (c.zero, 1.5)
            } else {
                (c.grid, 1.)
            };
            hline(window, area, y_of(area, db), thick, color);
        }
        for band in 0..BANDS {
            let x = x_of(area, band_x(band));
            window.paint_quad(fill(
                Bounds::new(
                    point(px(x - 0.5), area.top()),
                    size(px(1.), area.size.height),
                ),
                c.grid,
            ));
        }
        // The response curve; where it rises above 0 dB without pre-gain, those
        // stretches again in the warning colour.
        let n = self.points.len();
        let at = |i: usize| {
            point(
                px(x_of(area, i as f32 / (n.max(2) - 1) as f32)),
                px(y_of(area, self.points[i])),
            )
        };
        let line_color = if self.enabled { c.line } else { c.grid };
        if n >= 2 {
            let mut path = PathBuilder::stroke(px(2.));
            path.move_to(at(0));
            for i in 1..n {
                path.line_to(at(i));
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, line_color);
            }
            if self.warn && self.enabled {
                let mut over = PathBuilder::stroke(px(2.5));
                let mut drawing = false;
                for i in 0..n {
                    if self.points[i] > 0.05 {
                        if drawing {
                            over.line_to(at(i));
                        } else {
                            over.move_to(at(i.saturating_sub(1)));
                            over.line_to(at(i));
                            drawing = true;
                        }
                    } else {
                        drawing = false;
                    }
                }
                if let Ok(path) = over.build() {
                    window.paint_path(path, c.over);
                }
            }
        }
        let dragged = self.drag.get();
        for band in 0..BANDS {
            let center = point(
                px(x_of(area, band_x(band))),
                px(y_of(area, self.gains[band])),
            );
            let r = if dragged == Some(band) { 6.5 } else { 5. };
            dot(window, center, r + 1.5, c.point_ring);
            dot(
                window,
                center,
                r,
                if self.enabled { c.point } else { c.grid },
            );
        }

        let Some(handler) = self.on_change.clone() else {
            return;
        };
        window.set_cursor_style(CursorStyle::PointingHand, hitbox);
        let hit = hitbox.clone();
        {
            let (drag, handler, hit) = (self.drag.clone(), handler.clone(), hit.clone());
            window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                if !phase.bubble() || e.button != MouseButton::Left || !hit.is_hovered(window) {
                    return;
                }
                let band = band_at(area, e.position.x);
                drag.set(Some(band));
                window.capture_pointer(hit.id);
                cx.stop_propagation();
                handler(band, db_at(area, e.position.y), false, window, cx);
            });
        }
        {
            let (drag, handler) = (self.drag.clone(), handler.clone());
            window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                if !phase.bubble() {
                    return;
                }
                if let Some(band) = drag.get() {
                    handler(band, db_at(area, e.position.y), false, window, cx);
                }
            });
        }
        let drag = self.drag.clone();
        window.on_mouse_event(move |e: &MouseUpEvent, phase, window, cx| {
            if !phase.bubble() || e.button != MouseButton::Left {
                return;
            }
            let Some(band) = drag.take() else {
                return;
            };
            window.release_pointer();
            handler(band, db_at(area, e.position.y), true, window, cx);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Bounds<Pixels> {
        plot(Bounds::new(point(px(0.), px(0.)), size(px(516.), px(136.))))
    }

    #[test]
    fn y_and_db_invert_and_clamp() {
        let a = area();
        for db in [-12.0f32, -3.5, 0.0, 6.0, 12.0] {
            let y = y_of(a, db);
            assert!((db_at(a, px(y)) - db as f64).abs() < 1e-3, "{db}");
        }
        // Past the plot: clamped to the bands' range.
        assert_eq!(db_at(a, px(-50.)), EQ_RANGE_DB);
        assert_eq!(db_at(a, px(500.)), -EQ_RANGE_DB);
        assert!(y_of(a, 40.) >= f32::from(a.top()));
    }

    #[test]
    fn columns_pick_the_nearest_band() {
        let a = area();
        for band in 0..BANDS {
            let x = x_of(a, band_x(band));
            assert_eq!(band_at(a, px(x)), band);
            assert_eq!(band_at(a, px(x + 3.)), band);
        }
        assert_eq!(band_at(a, px(-10.)), 0);
        assert_eq!(band_at(a, px(9_999.)), BANDS - 1);
    }
}
