//! Skipping page sections that are far from the visible part of a scrolling page.
//!
//! GPUI builds, lays out and paints every element of a view on every frame it
//! draws, visible or not. On a long page (Home with its endless feed, an artist's
//! shelves) that made each full frame cost the same however little was on screen,
//! and more the further the feed was scrolled: ~10 ms of CPU per frame on a
//! Ryzen 7 6800H, so scrolling and page transitions ran at 40–75 fps on a 180 Hz
//! display (`@bench`, `MUSIC_TIMINGS`).
//!
//! [`Sections::cull`] wraps one section. A tiny `canvas` records where the section
//! sits in the page and how tall it is each time it is drawn; while it is more than
//! one viewport above or below what is visible, the next frames draw an empty box
//! of that height instead of building it. The page's layout and scrollbar stay the
//! same, and the box keeps recording its position, so sections above it can grow
//! or shrink. A section is always built when its position is unknown or stale:
//! the first frame, after its content changed (`key`), after the viewport width or
//! the font size changed.
//!
//! Horizontal shelves inside a culled section lose GPUI's element state while
//! skipped; [`Sections::x_scroll`] hands out handles that keep their offset.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, IntoElement as _, ParentElement as _, Pixels, ScrollHandle,
    SharedString, Styled as _, Window, canvas, div, px,
};

use super::items::{ShelfModel, ShelfStyle, shelf_view_scrolled};

/// Where a section was last drawn, in page coordinates (0 = top of the content).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Placed {
    top: f32,
    height: f32,
    /// Viewport width and rem size it was measured at.
    width: f32,
    rem: f32,
    key: u64,
}

impl Placed {
    /// Whether the section can be skipped: measured for this content, width and
    /// font size, and entirely more than one viewport away from the visible range
    /// (`scroll_top` .. `scroll_top + view_height`).
    fn far_off(&self, key: u64, width: f32, rem: f32, scroll_top: f32, view_height: f32) -> bool {
        if self.key != key || (self.width - width).abs() > 0.5 || (self.rem - rem).abs() > 0.01 {
            return false;
        }
        if view_height <= 0. {
            return false;
        }
        let above = self.top + self.height < scroll_top - view_height;
        let below = self.top > scroll_top + 2. * view_height;
        above || below
    }
}

/// The culling state of one scrolling page (cheap to clone).
#[derive(Clone, Default)]
pub struct Sections(Rc<RefCell<State>>);

#[derive(Default)]
struct State {
    placed: HashMap<SharedString, Placed>,
    x_scroll: HashMap<SharedString, ScrollHandle>,
}

impl Sections {
    /// Builds section `id` with `build`, or an empty box of its last height while
    /// it is far outside `scroll`'s viewport. `key` identifies the content (e.g.
    /// the item count): a section whose key changed is always built and measured.
    pub fn cull(
        &self,
        id: impl Into<SharedString>,
        key: u64,
        scroll: &ScrollHandle,
        window: &Window,
        build: impl FnOnce() -> AnyElement,
    ) -> AnyElement {
        let id = id.into();
        let view = scroll.bounds();
        let width = f32::from(view.size.width);
        let rem = f32::from(window.rem_size());
        let scroll_top = -f32::from(scroll.offset().y);
        let skip = self
            .0
            .borrow()
            .placed
            .get(&id)
            .copied()
            .filter(|p| p.far_off(key, width, rem, scroll_top, f32::from(view.size.height)));
        let state = self.0.clone();
        let handle = scroll.clone();
        let measure = canvas(
            move |bounds: Bounds<Pixels>, window, _| {
                let view = handle.bounds();
                let top = bounds.top() - view.top() - handle.offset().y;
                state.borrow_mut().placed.insert(
                    id,
                    Placed {
                        top: f32::from(top),
                        height: f32::from(bounds.size.height),
                        width: f32::from(view.size.width),
                        rem: f32::from(window.rem_size()),
                        key,
                    },
                );
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let section = div().w_full().relative();
        match skip {
            Some(placed) => section.h(px(placed.height)),
            None => section.child(build()),
        }
        .child(measure)
        .into_any_element()
    }

    /// A titled shelf (`items::shelf_view`), culled.
    pub fn shelf(
        &self,
        id: &str,
        model: &ShelfModel,
        style: ShelfStyle,
        scroll: &ScrollHandle,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let x_scroll = self.x_scroll(id);
        self.cull(
            id.to_owned(),
            model.layout_key(style),
            scroll,
            window,
            || shelf_view_scrolled(id, model, style, Some(&x_scroll), cx),
        )
    }

    /// A scroll handle for horizontal shelf `id` that outlives culling.
    pub fn x_scroll(&self, id: &str) -> ScrollHandle {
        self.0
            .borrow_mut()
            .x_scroll
            .entry(SharedString::from(id.to_owned()))
            .or_default()
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::Placed;

    fn placed(top: f32, height: f32) -> Placed {
        Placed {
            top,
            height,
            width: 1000.,
            rem: 16.,
            key: 7,
        }
    }

    #[test]
    fn keeps_what_is_near_the_viewport() {
        // Viewport 800 px tall, scrolled to 2000: sections between 1200 and 3600 stay.
        let (top, view) = (2000., 800.);
        assert!(
            !placed(2100., 300.).far_off(7, 1000., 16., top, view),
            "visible"
        );
        assert!(
            !placed(1000., 300.).far_off(7, 1000., 16., top, view),
            "ends 100 px into the margin"
        );
        assert!(
            !placed(3500., 300.).far_off(7, 1000., 16., top, view),
            "starts in the margin below"
        );
        assert!(
            placed(500., 300.).far_off(7, 1000., 16., top, view),
            "a viewport above"
        );
        assert!(
            placed(3700., 300.).far_off(7, 1000., 16., top, view),
            "two viewports below"
        );
    }

    #[test]
    fn rebuilds_when_the_measurement_is_stale() {
        let far = placed(10_000., 300.);
        assert!(far.far_off(7, 1000., 16., 0., 800.));
        assert!(!far.far_off(8, 1000., 16., 0., 800.), "content changed");
        assert!(!far.far_off(7, 1200., 16., 0., 800.), "window resized");
        assert!(!far.far_off(7, 1000., 18., 0., 800.), "font size changed");
        assert!(
            !far.far_off(7, 1000., 16., 0., 0.),
            "viewport not laid out yet"
        );
    }
}
