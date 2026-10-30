//! Smooth mouse-wheel scrolling (Windows, Linux).
//!
//! A wheel notch arrives as [`ScrollDelta::Lines`] (Windows: notches × the system's
//! "lines to scroll at a time", 3 by default), which every GPUI scroll container
//! turns into one jump. Here, one capture-phase listener for the whole main window
//! (painted after everything else in it, so it runs after the other capture
//! listeners) stops such events and replays them as pixel deltas over the next
//! frames: [`LINE_PX`] per line (108 px per default notch), eased out over
//! ~140 ms (`Reduced` motion: ~90 ms, `Off`: all at once on the next frame).
//! Notches in quick succession add up and keep the glide going; reversing drops
//! what was left. Every container (`overflow_y_scroll` divs, `uniform_list`,
//! `list`, gpui-component's scroll areas, the lyrics) gets the same smooth deltas
//! without knowing about it.
//!
//! Left alone: precise pixel deltas (trackpads, touch), Ctrl / ⌘ + wheel, and
//! controls that read the wheel themselves (volume, seek bars) — they
//! [`claim_wheel`] in the capture phase first and keep getting the notch as is.
//! Nothing runs between glides: frames are asked for only while one is in flight.
//!
//! macOS is unchanged (its wheel events already come with acceleration); elsewhere
//! `MUSIC_SMOOTH_WHEEL=0` turns this off and `=1` turns it on (also on macOS, for
//! testing with `@wheel` in scripts).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, DispatchPhase, IntoElement as _, Modifiers, ParentElement as _, PlatformInput,
    Point, ScrollDelta, ScrollEvent, ScrollSource, Styled as _, TouchPhase, Window, canvas, div,
    point, px,
};

use crate::effects::Motion;

/// Pixels per wheel line: 3 lines (one default Windows notch) = 108 px, close to
/// what browsers scroll per notch.
pub const LINE_PX: f32 = 36.;
/// A delta this large means "one screen per notch" (Windows reports
/// `WHEEL_PAGESCROLL` as u32::MAX lines).
const PAGE_LINES: f32 = 1000.;

/// Whether wheel notches glide (see the module docs).
pub fn enabled() -> bool {
    match std::env::var("MUSIC_SMOOTH_WHEEL").as_deref() {
        Ok("0") => false,
        Ok("1") => true,
        _ => !cfg!(target_os = "macos"),
    }
}

/// How long one notch glides.
fn duration(motion: Motion) -> Duration {
    match motion {
        Motion::Full => Duration::from_millis(140),
        Motion::Reduced => Duration::from_millis(90),
        Motion::Off => Duration::ZERO,
    }
}

/// The distance still to scroll and how it eases out (ease-out cubic from the
/// latest notch: the glide starts fast and settles).
#[derive(Debug, Default)]
struct Glide {
    /// Pixels not yet sent (x, y).
    left: (f32, f32),
    /// `left` when the latest notch came in.
    from: (f32, f32),
    start: Option<Instant>,
    duration: Duration,
}

impl Glide {
    fn active(&self) -> bool {
        self.start.is_some()
    }

    /// Adds a notch of `(dx, dy)` pixels; a reversal on an axis drops what was left.
    fn push(&mut self, (dx, dy): (f32, f32), now: Instant, duration: Duration) {
        let add = |left: f32, d: f32| if left * d < 0. { d } else { left + d };
        self.left = (add(self.left.0, dx), add(self.left.1, dy));
        self.from = self.left;
        self.start = Some(now);
        self.duration = duration;
    }

    /// The pixels to send for a frame at `now`.
    fn step(&mut self, now: Instant) -> (f32, f32) {
        let Some(start) = self.start else {
            return (0., 0.);
        };
        let t = if self.duration.is_zero() {
            1.
        } else {
            (now.saturating_duration_since(start).as_secs_f32() / self.duration.as_secs_f32())
                .min(1.)
        };
        if t >= 1. {
            let all = self.left;
            *self = Self::default();
            return all;
        }
        let rest = (1. - t).powi(3);
        let target = (self.from.0 * rest, self.from.1 * rest);
        let step = (self.left.0 - target.0, self.left.1 - target.1);
        self.left = target;
        step
    }
}

#[derive(Default)]
struct State {
    glide: Glide,
    modifiers: Modifiers,
}

/// The main window's wheel state (one per `MusicApp`).
#[derive(Clone, Default)]
pub struct Wheel(Rc<RefCell<State>>);

thread_local! {
    /// Where the event under dispatch was claimed by a wheel control.
    static CLAIMED: Cell<Option<Point<gpui::Pixels>>> = const { Cell::new(None) };
}

/// For elements that read the wheel themselves (call while painting): when
/// `hovered` says the pointer is on the element, wheel notches reach it unchanged.
pub fn claim_wheel(window: &mut Window, hovered: impl Fn(&Window) -> bool + 'static) {
    if !enabled() {
        return;
    }
    window.on_mouse_event(move |event: &ScrollEvent, phase, window, _| {
        if phase == DispatchPhase::Capture
            && matches!(event.delta, ScrollDelta::Lines(_))
            && hovered(window)
        {
            CLAIMED.set(Some(event.position));
        }
    });
}

/// A zero-cost element filling its parent that [`claim_wheel`]s its bounds.
pub fn claim_area() -> Option<AnyElement> {
    enabled().then(|| {
        canvas(
            |_, _, _| (),
            |bounds, _, window, _| {
                claim_wheel(window, move |window| {
                    bounds.contains(&window.mouse_position())
                })
            },
        )
        .absolute()
        .inset_0()
        .into_any_element()
    })
}

impl Wheel {
    /// A notch is still gliding (frames keep coming; `MUSIC_TIMINGS`).
    pub fn gliding(&self) -> bool {
        self.0.borrow().glide.active()
    }

    /// The listener, as an element to add last to the main window's root.
    pub fn listener(&self) -> Option<AnyElement> {
        if !enabled() {
            return None;
        }
        let state = self.0.clone();
        Some(
            div()
                .absolute()
                .size_0()
                .child(
                    canvas(
                        |_, _, _| (),
                        move |_, _, window, _| {
                            let state = state.clone();
                            window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
                                if phase == DispatchPhase::Capture {
                                    intercept(&state, event, window, cx);
                                }
                            });
                        },
                    )
                    .size_0(),
                )
                .into_any_element(),
        )
    }
}

fn intercept(
    state: &Rc<RefCell<State>>,
    event: &ScrollEvent,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    let claimed = CLAIMED.take() == Some(event.position);
    let ScrollDelta::Lines(lines) = event.delta else {
        return;
    };
    if claimed || event.modifiers.control || event.modifiers.platform {
        return;
    }
    cx.stop_propagation();
    let page = f32::from(window.viewport_size().height) * 0.875;
    let to_px = |l: f32| {
        if l.abs() >= PAGE_LINES {
            page.copysign(l)
        } else {
            l * LINE_PX
        }
    };
    let mut s = state.borrow_mut();
    let idle = !s.glide.active();
    s.glide.push(
        (to_px(lines.x), to_px(lines.y)),
        Instant::now(),
        duration(crate::effects::motion()),
    );
    s.modifiers = event.modifiers;
    drop(s);
    if idle {
        schedule(state.clone(), window);
    }
}

/// Sends the next step on the next frame, and asks for another while gliding.
fn schedule(state: Rc<RefCell<State>>, window: &Window) {
    window.on_next_frame(move |window, cx| {
        let (step, more, modifiers) = {
            let mut s = state.borrow_mut();
            let step = s.glide.step(Instant::now());
            (step, s.glide.active(), s.modifiers)
        };
        if step != (0., 0.) {
            window.dispatch_event(
                PlatformInput::Scroll(ScrollEvent {
                    position: window.mouse_position(),
                    delta: ScrollDelta::Pixels(point(px(step.0), px(step.1))),
                    modifiers,
                    touch_phase: TouchPhase::Moved,
                    source: ScrollSource::Pointer,
                }),
                cx,
            );
        }
        if more {
            schedule(state, window);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    fn run(glide: &mut Glide, start: Instant, frames: u32) -> (f32, f32) {
        let mut sum = (0., 0.);
        for i in 1..=frames {
            let (dx, dy) = glide.step(start + MS * (16 * i));
            sum = (sum.0 + dx, sum.1 + dy);
        }
        sum
    }

    #[test]
    fn a_notch_glides_its_whole_distance_and_stops() {
        let start = Instant::now();
        let mut glide = Glide::default();
        glide.push((0., -3. * LINE_PX), start, MS * 140);
        let first = glide.step(start + MS * 16).1;
        // Ease-out: the first frame moves the most, but not everything.
        assert!(first < -20. && first > -3. * LINE_PX, "{first}");
        let rest = run(&mut glide, start, 12).1;
        assert!((first + rest + 3. * LINE_PX).abs() < 0.01);
        assert!(!glide.active());
        assert_eq!(glide.step(start + MS * 500), (0., 0.));
    }

    #[test]
    fn quick_notches_add_up_and_reversal_drops_the_rest() {
        let start = Instant::now();
        let mut glide = Glide::default();
        glide.push((0., 100.), start, MS * 140);
        let moved = glide.step(start + MS * 16).1;
        glide.push((0., 100.), start + MS * 20, MS * 140);
        let rest = run(&mut glide, start + MS * 20, 20).1;
        assert!((moved + rest - 200.).abs() < 0.01);

        glide.push((0., 100.), start, MS * 140);
        let moved = glide.step(start + MS * 16).1;
        glide.push((0., -50.), start + MS * 20, MS * 140);
        let rest = run(&mut glide, start + MS * 20, 20).1;
        assert!(moved > 0.);
        assert!((rest + 50.).abs() < 0.01, "{rest}");
    }

    #[test]
    fn off_sends_everything_on_the_next_frame() {
        let start = Instant::now();
        let mut glide = Glide::default();
        glide.push((-36., 0.), start, duration(Motion::Off));
        assert_eq!(glide.step(start), (-36., 0.));
        assert!(!glide.active());
    }
}
