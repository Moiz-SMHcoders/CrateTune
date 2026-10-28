//! Cached sub-views for parts of a screen that re-renders at display rate.
//!
//! GPUI re-renders a view (and everything it builds inline) whenever it is
//! notified; a child *entity* embedded with `.cached(style)` is reused — layout,
//! prepaint and paint copied from the previous frame — until that child itself
//! is notified. [`Snapshot`] is such a child: it renders from a plain value (the
//! "snapshot" of whatever it shows) and is only notified when the value changes,
//! so e.g. the Now Playing header with its menu buttons costs nothing per frame
//! while the lyrics animate.
//!
//! The owner calls [`sync`] every render with the current value. A change is
//! drawn on the next frame (a notification during a draw lands in the next one),
//! so `sync` asks the caller's [`Frame`] for one more frame when it updates.

use gpui::{AnyElement, App, AppContext as _, Context, Entity, IntoElement, Render, Window};

use crate::anim::Frame;

/// Renders `state` with `render`; see the module docs.
pub struct Snapshot<S> {
    pub state: S,
    render: fn(&S, &mut Window, &mut App) -> AnyElement,
}

impl<S: 'static> Render for Snapshot<S> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        (self.render)(&self.state, window, cx)
    }
}

/// Creates the view in `slot`, or updates it if `state` changed.
pub fn sync<S: PartialEq + 'static>(
    slot: &mut Option<Entity<Snapshot<S>>>,
    state: S,
    render: fn(&S, &mut Window, &mut App) -> AnyElement,
    frame: &mut Frame,
    cx: &mut App,
) -> Entity<Snapshot<S>> {
    match slot {
        Some(view) => {
            if view.read(cx).state != state {
                view.update(cx, |view, cx| {
                    view.state = state;
                    cx.notify();
                });
                frame.keep_alive();
            }
            view.clone()
        }
        None => {
            let view = cx.new(|_| Snapshot { state, render });
            *slot = Some(view.clone());
            view
        }
    }
}
