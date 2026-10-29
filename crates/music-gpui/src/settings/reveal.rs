//! Scroll anchors on settings rows, and "reveal": scrolling a row into view,
//! flashing it and focusing its control after a settings search pick.
//!
//! A row opts in with an id (`row_at("playback.quality", …)`, `group_at`, or
//! `.anchor(id)` on any `Div`); the ids are the `anchor`s of the search index
//! (`search_index.rs`), and a test checks that every index anchor is passed to
//! exactly one of those calls.
//!
//! An anchor costs nothing while no reveal is pending: [`Anchor::anchor`] only
//! compares the id with the pending target and the flashing row. The target
//! row gets an invisible `canvas` that, during prepaint, reads the row's
//! bounds against the content's [`ScrollHandle`] and sets the scroll offset
//! (drawn on the next frame); the panel polls [`take_revealed`] to redraw,
//! start the flash and move focus. The state is a thread-local: rendering is on
//! the main thread, and the row helpers have no access to the panel.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationExt as _, Bounds, Div, FocusHandle, Hsla, InteractiveElement as _,
    IntoElement as _, ParentElement as _, Pixels, ScrollHandle, Styled as _, canvas, div, point,
    px,
};

/// How long a revealed row stays highlighted.
pub const FLASH: Duration = Duration::from_millis(1500);

#[derive(Default)]
struct State {
    /// The anchor to scroll into view when it is next prepainted.
    target: Option<&'static str>,
    scroll: Option<ScrollHandle>,
    /// Set when the target was found (and scrolled to); cleared by [`take_revealed`].
    revealed: Option<&'static str>,
    /// The highlighted row: id, start, a generation for the animation's id.
    flash: Option<(&'static str, Instant, u32)>,
    color: Option<Hsla>,
    /// Whether the flash fades (full motion) or just shows and goes.
    animate: bool,
    /// A tab stop on the revealed row, so the focus can move to its control.
    focus: Option<(&'static str, FocusHandle)>,
    generation: u32,
    /// Panel renders since the target was set (it is missing after two).
    renders: u32,
    /// Ids rendered since [`start_audit`] (script check).
    audit: Option<Vec<&'static str>>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// Starts a reveal of `id`: scroll it into view in `scroll`'s content when it is
/// next drawn, then flash it in `color`; `focus` becomes a tab stop on the row.
pub fn start(
    id: &'static str,
    scroll: ScrollHandle,
    focus: FocusHandle,
    color: Hsla,
    animate: bool,
) {
    STATE.with_borrow_mut(|s| {
        s.target = Some(id);
        s.renders = 0;
        s.scroll = Some(scroll);
        s.revealed = None;
        s.flash = None;
        s.color = Some(color);
        s.animate = animate;
        s.focus = Some((id, focus));
    });
}

/// Looks for another anchor instead (the target isn't on screen, e.g. a row
/// that only shows with some option on).
pub fn retarget(id: &'static str) {
    STATE.with_borrow_mut(|s| {
        s.target = Some(id);
        s.renders = 0;
        if let Some((_, handle)) = s.focus.take() {
            s.focus = Some((id, handle));
        }
    });
}

/// Called when the panel renders: counts frames while a target is pending.
pub fn rendering() {
    STATE.with_borrow_mut(|s| {
        if s.target.is_some() {
            s.renders = s.renders.saturating_add(1);
        }
    });
}

/// Panel renders since the pending target was set (it was drawn, if there,
/// by the second; the first may still show the previous section).
pub fn renders_since_target() -> u32 {
    STATE.with_borrow(|s| s.renders)
}

/// The anchor still waiting to be found.
pub fn pending() -> Option<&'static str> {
    STATE.with_borrow(|s| s.target)
}

/// Gives up on the pending target.
pub fn cancel() {
    STATE.with_borrow_mut(|s| {
        s.target = None;
        s.focus = None;
    });
}

/// The anchor that was found and scrolled to since the last call, if any; its
/// flash starts now.
pub fn take_revealed() -> Option<&'static str> {
    STATE.with_borrow_mut(|s| {
        let id = s.revealed.take()?;
        s.generation = s.generation.wrapping_add(1);
        s.flash = Some((id, Instant::now(), s.generation));
        Some(id)
    })
}

/// The tab stop placed on the revealed row.
pub fn focus_handle() -> Option<FocusHandle> {
    STATE.with_borrow(|s| s.focus.as_ref().map(|(_, h)| h.clone()))
}

/// Ends the flash (after [`FLASH`]).
pub fn end_flash() {
    STATE.with_borrow_mut(|s| s.flash = None);
}

/// The row now highlighted.
pub fn flashing() -> Option<&'static str> {
    STATE.with_borrow(|s| s.flash.map(|f| f.0))
}

/// Records the ids of the anchors drawn from now on (`@settings-anchors`).
pub fn start_audit() {
    STATE.with_borrow_mut(|s| s.audit = Some(Vec::new()));
}

/// The ids drawn since [`start_audit`] (the recording stops).
pub fn take_audit() -> Vec<&'static str> {
    STATE.with_borrow_mut(|s| s.audit.take().unwrap_or_default())
}

/// Opacity of the flash at `t` (0–1 of [`FLASH`]): on at once (the row is
/// found at a glance), held, then a soft fade.
fn flash_curve(t: f32) -> f32 {
    if t < 0.3 {
        1.
    } else {
        let f = ((t - 0.3) / 0.7).min(1.);
        (1. - f) * (1. - f)
    }
}

/// Prepaint of the target's probe: scroll so the row (`bounds`, window
/// coordinates) is in view, unless it already is.
fn found(id: &'static str, bounds: Bounds<Pixels>) {
    STATE.with_borrow_mut(|s| {
        if s.target != Some(id) {
            return;
        }
        let Some(handle) = s.scroll.clone() else {
            return;
        };
        let view = handle.bounds();
        if view.size.height <= px(1.) {
            // The content hasn't been laid out yet: try again next frame.
            return;
        }
        let offset = handle.offset();
        let fits = bounds.top() >= view.top() && bounds.bottom() <= view.bottom();
        if !fits {
            // Content coordinates, then a fifth of the viewport above the row.
            let top = bounds.top() - view.top() - offset.y;
            let margin = (view.size.height * 0.2).min(px(96.));
            let y = (margin - top).clamp(-handle.max_offset().y, px(0.));
            handle.set_offset(point(offset.x, y));
        }
        s.target = None;
        s.revealed = Some(id);
    });
}

/// A settings element that search results can scroll to (see the module docs).
pub trait Anchor: Sized {
    fn anchor(self, id: &'static str) -> Self;
}

impl Anchor for Div {
    fn anchor(self, id: &'static str) -> Self {
        let (target, flash, color, animate, focus) = STATE.with_borrow_mut(|s| {
            if let Some(audit) = &mut s.audit {
                audit.push(id);
            }
            (
                s.target == Some(id),
                s.flash.filter(|f| f.0 == id),
                s.color,
                s.animate,
                s.focus
                    .as_ref()
                    .filter(|(at, _)| *at == id)
                    .map(|(_, h)| h.clone()),
            )
        });
        if !target && flash.is_none() && focus.is_none() {
            return self;
        }
        let mut el = self.relative();
        if let Some(handle) = &focus {
            el = el.track_focus(handle);
        }
        if let (Some((_, _, generation)), Some(color)) = (flash, color) {
            let overlay = div()
                .absolute()
                .top(px(-6.))
                .bottom(px(-6.))
                .left(px(-8.))
                .right(px(-8.))
                .rounded(crate::theme::radius(8.))
                .bg(color);
            let overlay = if animate {
                overlay
                    .with_animation(
                        ("settings-flash", generation as usize),
                        Animation::new(FLASH),
                        |el, t| el.opacity(flash_curve(t)),
                    )
                    .into_any_element()
            } else {
                overlay.into_any_element()
            };
            el = el.child(overlay);
        }
        if target {
            el = el.child(
                canvas(move |bounds, _, _| found(id, bounds), |_, _, _, _| {})
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            );
        }
        el
    }
}

#[cfg(test)]
mod tests {
    use super::flash_curve;

    #[test]
    fn flash_rises_holds_and_fades() {
        assert_eq!(flash_curve(0.), 1.);
        assert_eq!(flash_curve(0.25), 1.);
        assert!(flash_curve(0.8) < 0.5);
        assert_eq!(flash_curve(1.), 0.);
    }
}
