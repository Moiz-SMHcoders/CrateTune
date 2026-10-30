//! Horizontally scrolling rows (shelves, the quick-picks grid, chip rows) and every
//! way to move them sideways, as on YouTube Music and Spotify desktop:
//!
//! * **‹ › buttons** at the right of a shelf's header ([`HRow::header`]), or on the
//!   row's edges while it is hovered (rows without a header, e.g. chips). A click
//!   pages about one viewport, snapping to an item's edge ([`page_target`]), gliding
//!   for ~280 ms (`Reduced` motion: ~160 ms, `Off`: at once). A button is disabled
//!   at its end; neither shows while everything fits.
//! * **Wheel / trackpad**: horizontal deltas (trackpad swipes, tilt wheels, and
//!   Shift + wheel, which every platform layer or [`HRow`] itself turns into
//!   horizontal deltas) move the hovered row and stop there, so the page doesn't
//!   also move. A plain vertical wheel goes on to the page. Trackpad gestures keep
//!   the axis they started on ([`horizontal`]). On Windows / Linux, line-based
//!   notches reach rows as the glided pixel deltas of `smooth_scroll.rs`.
//! * **Click and drag** pans the row with a closed-hand cursor once the pointer has
//!   moved [`DRAG_THRESHOLD`] px ([`Pan`]); the click that would follow on release
//!   (opening or playing a card) is then swallowed.
//! * **Scrollbar**: a thin horizontal one under the row while it is hovered.
//!
//! Why this isn't plain `overflow_x_scroll()`: GPUI measures a scroll container's
//! content as the union of its *direct* children's bounds. The rows used to wrap
//! their items in one `h_flex`, which a block container stretches to its own width,
//! so the content never measured wider than the view and the maximum offset stayed
//! 0: no wheel, trackpad or scrollbar could move it. Here the items are the scroll
//! container's children. And GPUI's own listener, on an x-only scroller, turns a
//! vertical wheel into sideways scrolling without stopping the event, so a
//! scrollable row would hijack the page's wheel: the container uses
//! `overflow_x_hidden` (clipping, offset, content size) and [`HRow`] handles the
//! wheel itself.
//!
//! The scroll position lives in a [`RowScroll`]; pages that skip off-screen
//! sections (`cull.rs`) hand one in so it outlives the skipping, other rows keep one
//! in GPUI's element state.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Bounds, CursorStyle, DispatchPhase, ElementId, EntityId,
    InteractiveElement as _, IntoElement, MouseButton, MouseMoveEvent, MouseUpEvent,
    ParentElement as _, Pixels, Point, RenderOnce, ScrollEvent, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, TouchPhase, Window, canvas, div, point,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _,
    button::{Button, ButtonRounded, ButtonVariants as _},
    h_flex,
    scroll::{Scrollbar, ScrollbarMode},
    v_flex,
};

use super::section_header;
use crate::effects::Motion;

/// How far the pointer moves (px) before a press on a row becomes a pan.
pub const DRAG_THRESHOLD: f32 = 6.;
/// Space under the row for the scrollbar (it overlaps the cards' own padding).
const BAR_SPACE: f32 = 10.;
/// A trackpad gesture keeps its axis while its events come closer than this.
const AXIS_LOCK: Duration = Duration::from_millis(200);
/// Group name of a row (edge arrows show while it is hovered).
const GROUP: &str = "hrow";

/// How long a ‹ › page glides.
fn glide_duration(motion: Motion) -> Duration {
    match motion {
        Motion::Full => Duration::from_millis(280),
        Motion::Reduced => Duration::from_millis(160),
        Motion::Off => Duration::ZERO,
    }
}

// ---- the maths (unit-tested) ------------------------------------------------------

/// Where a ‹ / › page from `scroll` lands. `items` are the items' (left, right)
/// edges in content coordinates, in order; `view` the visible width; `max` the
/// largest scroll offset.
///
/// Forward: the first item cut off on the right becomes the first one shown.
/// Back: the last item cut off on the left becomes the last one shown fully, and
/// the row starts at the first item that keeps it in view. An item wider than the
/// view, or no items, pages by the view's width. Always within `0..=max`.
pub fn page_target(items: &[(f32, f32)], scroll: f32, view: f32, max: f32, forward: bool) -> f32 {
    const EPS: f32 = 1.;
    if max <= 0. || view <= 0. {
        return 0.;
    }
    let target = if forward {
        let end = scroll + view;
        match items.iter().find(|(_, right)| *right > end + EPS) {
            Some(&(left, _)) if left > scroll + EPS => left,
            _ => end,
        }
    } else {
        match items.iter().rev().find(|(left, _)| *left < scroll - EPS) {
            Some(&(_, right)) => {
                let start = right - view;
                match items.iter().find(|(left, _)| *left >= start - EPS) {
                    Some(&(left, _)) if left < scroll - EPS => left,
                    _ => scroll - view,
                }
            }
            None => 0.,
        }
    };
    target.clamp(0., max)
}

/// A press on a row that may turn into a pan.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pan {
    start: (f32, f32),
    /// The row's scroll offset at the press.
    from: f32,
    /// Moved past [`DRAG_THRESHOLD`]: the row follows and the click is swallowed.
    pub active: bool,
}

impl Pan {
    pub fn new(at: (f32, f32), from: f32) -> Self {
        Self {
            start: at,
            from,
            active: false,
        }
    }

    /// The pointer moved to `at`: the row's new scroll offset once panning
    /// (dragging right shows what is on the left), else `None`.
    pub fn moved(&mut self, at: (f32, f32)) -> Option<f32> {
        let (dx, dy) = (at.0 - self.start.0, at.1 - self.start.1);
        if !self.active && dx.hypot(dy) >= DRAG_THRESHOLD {
            self.active = true;
        }
        self.active.then_some(self.from - dx)
    }
}

/// Whether a wheel / trackpad delta moves the row (`true`) or the page. A
/// gesture's later events keep its axis (`lock`: the last decision and when),
/// unless a new gesture starts.
pub fn horizontal(
    dx: f32,
    dy: f32,
    phase: TouchPhase,
    lock: Option<(bool, Instant)>,
    now: Instant,
) -> bool {
    match lock {
        Some((axis, at))
            if phase != TouchPhase::Started && now.saturating_duration_since(at) < AXIS_LOCK =>
        {
            axis
        }
        _ => dx.abs() > dy.abs(),
    }
}

/// A ‹ › glide: ease-out cubic from `from` to `to`.
#[derive(Clone, Copy, Debug)]
struct Glide {
    from: f32,
    to: f32,
    start: Instant,
    duration: Duration,
}

impl Glide {
    /// The offset at `now` and whether the glide is over.
    fn at(&self, now: Instant) -> (f32, bool) {
        if self.duration.is_zero() {
            return (self.to, true);
        }
        let t = (now.saturating_duration_since(self.start).as_secs_f32()
            / self.duration.as_secs_f32())
        .min(1.);
        let eased = 1. - (1. - t).powi(3);
        (self.from + (self.to - self.from) * eased, t >= 1.)
    }
}

// ---- state -------------------------------------------------------------------------

/// What the ‹ › buttons show.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Edges {
    /// The row is wider than its view.
    pub overflow: bool,
    pub can_back: bool,
    pub can_forward: bool,
}

#[derive(Default)]
struct Inner {
    handle: ScrollHandle,
    /// The view the row was last rendered in (notified when it moves).
    view: Cell<Option<EntityId>>,
    glide: Cell<Option<Glide>>,
    pan: Cell<Option<Pan>>,
    /// The wheel's last axis decision ([`horizontal`]).
    axis: Cell<Option<(bool, Instant)>>,
    /// [`Edges`] the last render showed; a layout that changes them asks for a frame.
    shown: Cell<Edges>,
}

/// The scroll position of one row (cheap to clone).
#[derive(Clone, Default)]
pub struct RowScroll(Rc<Inner>);

impl RowScroll {
    /// How far the row is scrolled (≥ 0).
    pub fn x(&self) -> f32 {
        -f32::from(self.0.handle.offset().x)
    }

    /// The largest scroll offset, as of the last layout.
    pub fn max_x(&self) -> f32 {
        f32::from(self.0.handle.max_offset().x).max(0.)
    }

    /// Width of the visible part.
    pub fn view_width(&self) -> f32 {
        f32::from(self.0.handle.bounds().size.width)
    }

    pub fn edges(&self) -> Edges {
        let (x, max) = (self.x(), self.max_x());
        Edges {
            overflow: max > 0.5,
            can_back: x > 0.5,
            can_forward: x < max - 0.5,
        }
    }

    fn set_x(&self, x: f32) {
        let x = x.clamp(0., self.max_x());
        let y = self.0.handle.offset().y;
        self.0.handle.set_offset(point(px(-x), y));
    }

    fn notify(&self, cx: &mut App) {
        if let Some(view) = self.0.view.get() {
            cx.notify(view);
        }
    }

    /// The items' (left, right) edges in content coordinates, from the last layout.
    fn item_edges(&self) -> Vec<(f32, f32)> {
        let handle = &self.0.handle;
        let origin = f32::from(handle.bounds().left());
        (0..handle.children_count())
            .filter_map(|i| handle.bounds_for_item(i))
            .map(|b| (f32::from(b.left()) - origin, f32::from(b.right()) - origin))
            .collect()
    }

    /// ‹ / ›: pages one view back or forward, snapped to an item's edge.
    pub fn page(&self, forward: bool, window: &mut Window, cx: &mut App) {
        let from = match self.0.glide.get() {
            // Clicks in quick succession continue from where the glide is heading.
            Some(glide) => glide.to,
            None => self.x(),
        };
        let to = page_target(
            &self.item_edges(),
            from,
            self.view_width(),
            self.max_x(),
            forward,
        );
        self.glide_to(to, window, cx);
    }

    /// Moves to `to` (clamped), gliding unless motion is off.
    pub fn glide_to(&self, to: f32, window: &mut Window, cx: &mut App) {
        let to = to.clamp(0., self.max_x());
        let duration = glide_duration(crate::effects::motion());
        if duration.is_zero() {
            self.0.glide.set(None);
            self.set_x(to);
            self.notify(cx);
            return;
        }
        let running = self.0.glide.get().is_some();
        self.0.glide.set(Some(Glide {
            from: self.x(),
            to,
            start: Instant::now(),
            duration,
        }));
        if !running {
            self.tick(window);
        }
    }

    fn tick(&self, window: &Window) {
        let this = self.clone();
        window.on_next_frame(move |window, cx| {
            let Some(glide) = this.0.glide.get() else {
                return;
            };
            let (x, done) = glide.at(Instant::now());
            this.set_x(x);
            if done {
                this.0.glide.set(None);
            } else {
                this.tick(window);
            }
            this.notify(cx);
        });
    }

    /// A wheel / trackpad event over the row (bubble phase).
    fn wheel(&self, event: &ScrollEvent, window: &Window, cx: &mut App) {
        if !self.edges().overflow {
            return;
        }
        let delta = event.delta.pixel_delta(window.line_height());
        let (mut dx, mut dy) = (f32::from(delta.x), f32::from(delta.y));
        // Shift + wheel: macOS, Windows and X11 / Wayland already report it as a
        // horizontal delta; anything else that sends it vertically, here.
        if event.modifiers.shift && dx == 0. {
            (dx, dy) = (dy, 0.);
        }
        let now = Instant::now();
        let sideways = horizontal(dx, dy, event.touch_phase, self.0.axis.get(), now);
        self.0.axis.set(Some((sideways, now)));
        if !sideways {
            return;
        }
        // The page doesn't move while a row does (or is pushed against its end).
        cx.stop_propagation();
        if dx != 0. {
            self.0.glide.set(None);
            self.set_x(self.x() - dx);
            self.notify(cx);
        }
    }

    /// Left button down on the row.
    fn press(&self, at: Point<Pixels>) {
        // Not on the scrollbar, which overlaps the bottom of the row.
        let bottom = f32::from(self.0.handle.bounds().bottom());
        if !self.edges().overflow || f32::from(at.y) > bottom - (16. - BAR_SPACE) {
            return;
        }
        self.0.glide.set(None);
        self.0
            .pan
            .set(Some(Pan::new((f32::from(at.x), f32::from(at.y)), self.x())));
    }

    /// Registers the pan's pointer listeners, the cursor, and asks for a frame when
    /// the layout changed what the ‹ › buttons should show. Called while painting,
    /// after the row's items, so the mouse-up listener runs after theirs.
    fn paint(&self, id: &SharedString, window: &mut Window) {
        if self.edges() != self.0.shown.get()
            && let Some(view) = self.0.view.get()
        {
            window.on_next_frame(move |_, cx| cx.notify(view));
        }
        if self.0.pan.get().is_some_and(|p| p.active) {
            window.set_window_cursor_style(CursorStyle::ClosedHand);
        }
        // Always registered (a press can be followed by moves before the next frame).
        {
            let this = self.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                if phase != DispatchPhase::Capture {
                    return;
                }
                let Some(mut pan) = this.0.pan.get() else {
                    return;
                };
                if event.pressed_button != Some(MouseButton::Left) {
                    // Released outside the window.
                    this.0.pan.set(None);
                    this.notify(cx);
                    return;
                }
                let was_active = pan.active;
                let at = (f32::from(event.position.x), f32::from(event.position.y));
                let moved = pan.moved(at);
                this.0.pan.set(Some(pan));
                if let Some(x) = moved {
                    this.set_x(x);
                }
                if moved.is_some() || pan.active != was_active {
                    this.notify(cx);
                }
            });
            let this = self.clone();
            window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                if phase != DispatchPhase::Capture || event.button != MouseButton::Left {
                    return;
                }
                if this.0.pan.take().is_some_and(|p| p.active) {
                    // The items' capture listeners ran first (they were registered
                    // earlier); stopping here keeps their click from firing.
                    cx.stop_propagation();
                    this.notify(cx);
                }
            });
        }
        register(id, self, self.0.handle.bounds());
    }
}

// ---- the element ---------------------------------------------------------------------

/// A shelf header above an [`HRow`]: the ‹ › buttons go after `trailing`.
pub struct RowHeader {
    pub title: SharedString,
    pub subtitle: Option<SharedString>,
    pub trailing: Option<AnyElement>,
}

/// A horizontally scrolling row of items (see the module docs).
#[derive(IntoElement)]
pub struct HRow {
    id: SharedString,
    scroll: Option<RowScroll>,
    items: Vec<AnyElement>,
    gap: Pixels,
    header: Option<RowHeader>,
    items_start: bool,
}

impl HRow {
    /// Items must not shrink (`flex_none`, or a fixed width).
    pub fn new(id: impl Into<SharedString>, items: impl IntoIterator<Item = AnyElement>) -> Self {
        Self {
            id: id.into(),
            scroll: None,
            items: items.into_iter().collect(),
            gap: px(8.),
            header: None,
            items_start: false,
        }
    }

    /// Keeps the scroll position in `scroll` instead of the element state.
    pub fn scroll(mut self, scroll: Option<&RowScroll>) -> Self {
        self.scroll = scroll.cloned();
        self
    }

    pub fn gap(mut self, gap: Pixels) -> Self {
        self.gap = gap;
        self
    }

    /// A shelf header with the ‹ › buttons (without one, they show on the edges).
    pub fn header(mut self, header: RowHeader) -> Self {
        self.header = Some(header);
        self
    }

    /// Items aligned to the top (columns of different heights).
    pub fn items_start(mut self) -> Self {
        self.items_start = true;
        self
    }
}

fn arrow(id: SharedString, forward: bool, enabled: bool, scroll: &RowScroll) -> Button {
    let scroll = scroll.clone();
    Button::new(id)
        .small()
        .icon(if forward {
            IconName::ChevronRight
        } else {
            IconName::ChevronLeft
        })
        .tooltip(if forward {
            t!("common.scroll_forward")
        } else {
            t!("common.scroll_back")
        })
        .disabled(!enabled)
        .on_click(move |_, window, cx| scroll.page(forward, window, cx))
}

impl RenderOnce for HRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let id = self.id;
        let scroll = match self.scroll {
            Some(scroll) => scroll,
            None => window
                .use_keyed_state(ElementId::Name(format!("{id}-hrow").into()), cx, |_, _| {
                    RowScroll::default()
                })
                .read(cx)
                .clone(),
        };
        scroll.0.view.set(Some(window.current_view()));
        let edges = scroll.edges();
        scroll.0.shown.set(edges);

        let row = div()
            .id(id.clone())
            .w_full()
            .flex()
            .flex_row()
            .when(self.items_start, |this| this.items_start())
            .gap(self.gap)
            .overflow_x_hidden()
            .track_scroll(&scroll.0.handle)
            .children(self.items)
            .on_scroll({
                let scroll = scroll.clone();
                move |event, window, cx| scroll.wheel(event, window, cx)
            })
            .on_mouse_down(MouseButton::Left, {
                let scroll = scroll.clone();
                move |event, _, _| scroll.press(event.position)
            });
        let edge_arrows = self.header.is_none() && edges.overflow;
        // Rows without a header: ‹ › over the row's ends while it is hovered.
        let (raised, border) = {
            let theme = cx.theme();
            (theme.popover, theme.border)
        };
        let edge = |forward: bool| {
            div()
                .absolute()
                .top_0()
                .bottom(px(BAR_SPACE))
                .map(|d| if forward { d.right_0() } else { d.left_0() })
                .flex()
                .items_center()
                .invisible()
                .group_hover(GROUP, |s| s.visible())
                .child(
                    arrow(
                        format!("{id}-edge-{}", if forward { "next" } else { "prev" }).into(),
                        forward,
                        true,
                        &scroll,
                    )
                    .ghost()
                    .bg(raised)
                    .border_1()
                    .border_color(border)
                    .shadow_md()
                    .rounded(ButtonRounded::Size(px(14.))),
                )
        };
        let painter = {
            let (scroll, id) = (scroll.clone(), id.clone());
            canvas(
                |_, _, _| (),
                move |_, _, window, _| scroll.paint(&id, window),
            )
            .absolute()
            .size_0()
        };
        let body = div()
            .relative()
            .w_full()
            .pb(px(BAR_SPACE))
            .group(GROUP)
            .child(row)
            .when(edges.overflow, |this| {
                this.child(
                    div().absolute().inset_0().child(
                        Scrollbar::horizontal(&scroll.0.handle)
                            .id(SharedString::from(format!("{id}-bar")))
                            .mode(ScrollbarMode::Hover)
                            .viewport_from_layout(),
                    ),
                )
            })
            .when(edge_arrows && edges.can_back, |this| {
                this.child(edge(false))
            })
            .when(edge_arrows && edges.can_forward, |this| {
                this.child(edge(true))
            })
            .child(painter);
        match self.header {
            None => body.into_any_element(),
            Some(header) => {
                let arrows = edges.overflow.then(|| {
                    h_flex()
                        .gap_1()
                        .child(
                            arrow(format!("{id}-prev").into(), false, edges.can_back, &scroll)
                                .ghost(),
                        )
                        .child(
                            arrow(
                                format!("{id}-next").into(),
                                true,
                                edges.can_forward,
                                &scroll,
                            )
                            .ghost(),
                        )
                });
                let trailing = (header.trailing.is_some() || arrows.is_some()).then(|| {
                    h_flex()
                        .flex_none()
                        .gap_2()
                        .children(header.trailing)
                        .children(arrows)
                        .into_any_element()
                });
                v_flex()
                    .w_full()
                    .gap_3()
                    .child(section_header(header.title, header.subtitle, trailing, cx))
                    .child(body)
                    .into_any_element()
            }
        }
    }
}

// ---- scripts (`@shelf-status`, `@shelf-scroll`) ------------------------------------------

/// A row painted while recording: id, state, bounds.
type Painted = (SharedString, Weak<Inner>, Bounds<Pixels>);

thread_local! {
    /// Rows painted since [`record_painted`] (`None`: not recording).
    static PAINTED: RefCell<Option<Vec<Painted>>> =
        const { RefCell::new(None) };
}

fn register(id: &SharedString, scroll: &RowScroll, bounds: Bounds<Pixels>) {
    PAINTED.with_borrow_mut(|rows| {
        if let Some(rows) = rows {
            rows.push((id.clone(), Rc::downgrade(&scroll.0), bounds));
        }
    });
}

/// A row on screen: id, state, bounds.
pub type PaintedRow = (SharedString, RowScroll, Bounds<Pixels>);

/// Starts recording the rows that are painted (scripts: record, draw, take).
pub fn record_painted() {
    PAINTED.set(Some(Vec::new()));
}

/// The rows painted since [`record_painted`], top to bottom.
pub fn take_painted() -> Vec<PaintedRow> {
    let mut rows: Vec<_> = PAINTED
        .take()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(id, inner, bounds)| Some((id, RowScroll(inner.upgrade()?), bounds)))
        .collect();
    rows.sort_by(|a, b| {
        f32::from(a.2.top())
            .total_cmp(&f32::from(b.2.top()))
            .then(f32::from(a.2.left()).total_cmp(&f32::from(b.2.left())))
    });
    rows.dedup_by(|a, b| a.0 == b.0);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ten 100 px items, 10 px apart.
    fn items() -> Vec<(f32, f32)> {
        (0..10)
            .map(|i| {
                let left = i as f32 * 110.;
                (left, left + 100.)
            })
            .collect()
    }

    // Content 1090 px wide, view 350 px: max 740.
    const VIEW: f32 = 350.;
    const MAX: f32 = 740.;

    #[test]
    fn forward_starts_at_the_first_cut_off_item() {
        // 0..350 shows items 0-2 fully; item 3 (330..430) is cut off.
        assert_eq!(page_target(&items(), 0., VIEW, MAX, true), 330.);
        assert_eq!(page_target(&items(), 330., VIEW, MAX, true), 660.);
        // Near the end: clamped to the end.
        assert_eq!(page_target(&items(), 660., VIEW, MAX, true), MAX);
        assert_eq!(page_target(&items(), MAX, VIEW, MAX, true), MAX);
        // From the middle of an item: that item's edge.
        assert_eq!(page_target(&items(), 50., VIEW, MAX, true), 330.);
    }

    #[test]
    fn back_keeps_the_first_cut_off_item_as_the_last_one_shown() {
        // At 660 item 5 (550..650) is the last hidden on the left; showing it
        // fully at the right edge means starting at item 3 (330).
        assert_eq!(page_target(&items(), 660., VIEW, MAX, false), 330.);
        assert_eq!(page_target(&items(), 330., VIEW, MAX, false), 0.);
        assert_eq!(page_target(&items(), 0., VIEW, MAX, false), 0.);
        // From the end (740: item 6 at 660 is cut off on the left).
        assert_eq!(page_target(&items(), MAX, VIEW, MAX, false), 440.);
        // Every page back then forward lands on item edges.
        let edges: Vec<f32> = items().iter().map(|i| i.0).chain([MAX]).collect();
        let mut x = 0.;
        for _ in 0..5 {
            x = page_target(&items(), x, VIEW, MAX, true);
            assert!(edges.contains(&x), "{x}");
        }
        for _ in 0..5 {
            x = page_target(&items(), x, VIEW, MAX, false);
            assert!(edges.contains(&x), "{x}");
        }
        assert_eq!(x, 0.);
    }

    #[test]
    fn wide_items_and_no_overflow_page_by_the_view() {
        let wide = [(0., 500.), (510., 1010.)];
        assert_eq!(page_target(&wide, 0., VIEW, 660., true), 350.);
        assert_eq!(page_target(&wide, 350., VIEW, 660., true), 510.);
        assert_eq!(page_target(&wide, 510., VIEW, 660., false), 160.);
        assert_eq!(page_target(&[], 100., VIEW, 660., true), 450.);
        assert_eq!(page_target(&items(), 0., 2000., 0., true), 0.);
    }

    #[test]
    fn a_press_pans_only_past_the_threshold() {
        let mut pan = Pan::new((100., 100.), 200.);
        assert_eq!(pan.moved((103., 102.)), None, "jitter of a click");
        assert!(!pan.active);
        assert_eq!(pan.moved((95., 100.)), None, "5 px");
        // 6 px: a pan; the row follows the pointer from the press on.
        assert_eq!(pan.moved((94., 100.)), Some(206.));
        assert!(pan.active);
        // Once panning, coming back near the start keeps panning.
        assert_eq!(pan.moved((101., 100.)), Some(199.));
        // Dragging right shows what is on the left.
        assert_eq!(pan.moved((150., 100.)), Some(150.));
        // A mostly vertical move also passes the threshold (no click after it).
        let mut vertical = Pan::new((0., 0.), 0.);
        assert_eq!(vertical.moved((1., 8.)), Some(-1.));
    }

    #[test]
    fn wheel_axis_follows_the_gesture() {
        let now = Instant::now();
        let soon = now + Duration::from_millis(50);
        let later = now + Duration::from_millis(500);
        // Plain vertical wheel: the page.
        assert!(!horizontal(0., -120., TouchPhase::Moved, None, now));
        // Horizontal swipe / tilt / Shift + wheel: the row.
        assert!(horizontal(-30., 4., TouchPhase::Moved, None, now));
        // A sideways gesture that drifts vertical stays on the row…
        let lock = Some((true, now));
        assert!(horizontal(2., -9., TouchPhase::Moved, lock, soon));
        // …until a new gesture starts or it pauses.
        assert!(!horizontal(2., -9., TouchPhase::Started, lock, soon));
        assert!(!horizontal(2., -9., TouchPhase::Moved, lock, later));
        // And a vertical one stays on the page.
        assert!(!horizontal(
            -20.,
            3.,
            TouchPhase::Moved,
            Some((false, now)),
            soon
        ));
    }

    #[test]
    fn glide_eases_out_and_ends_on_target() {
        let start = Instant::now();
        let glide = Glide {
            from: 0.,
            to: 300.,
            start,
            duration: Duration::from_millis(280),
        };
        let (early, done) = glide.at(start + Duration::from_millis(70));
        assert!(!done && early > 75. && early < 300., "{early}");
        assert_eq!(glide.at(start + Duration::from_millis(300)), (300., true));
        let off = Glide {
            duration: Duration::ZERO,
            ..glide
        };
        assert_eq!(off.at(start), (300., true));
    }
}
