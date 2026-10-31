//! Interface zoom: ⌘ / Ctrl + `=` (or `+`), `-`, `0` (editable in Settings →
//! Keyboard shortcuts; also in the View menu) and ⌘ / Ctrl + mouse wheel. It
//! steps the existing text size setting (`appearance.font_scale`, GPUI's rem
//! size, which gpui-component's controls and the app's text follow) through
//! [`STEPS`] and saves it like the Appearance buttons do.
//!
//! The wheel: `smooth_scroll.rs`'s capture listener hands Ctrl / ⌘ + wheel
//! here unless a control under the pointer reads the wheel itself (volume, seek
//! bars: `claim_wheel`), which keeps it. A mouse notch is one step; trackpad
//! pixels add up to a step every [`PIXELS_PER_STEP`]. Nothing else in the app
//! uses Ctrl / ⌘ + wheel.

use gpui::{Context, ScrollDelta, Window};
use gpui_component::WindowExt as _;

use crate::app::MusicApp;

/// Zoom levels (the Appearance buttons are the middle five).
pub const STEPS: [f32; 7] = [0.75, 0.875, 1.0, 1.125, 1.25, 1.375, 1.5];
/// Trackpad scrolling per zoom step.
const PIXELS_PER_STEP: f32 = 60.;

/// The level nearest to `scale`.
pub fn snap(scale: f32) -> f32 {
    if !scale.is_finite() {
        return 1.0;
    }
    STEPS
        .into_iter()
        .min_by(|a, b| (a - scale).abs().total_cmp(&(b - scale).abs()))
        .unwrap_or(1.0)
}

/// The level after `steps` steps from `current` (0 = back to 100 %), clamped.
pub fn stepped(current: f32, steps: i32) -> f32 {
    if steps == 0 {
        return 1.0;
    }
    let i = STEPS.iter().position(|s| *s == snap(current)).unwrap_or(2) as i32;
    STEPS[(i + steps).clamp(0, STEPS.len() as i32 - 1) as usize]
}

/// Wheel deltas → zoom steps.
#[derive(Debug, Default)]
pub struct WheelZoom {
    pixels: f32,
}

impl WheelZoom {
    /// Steps for one wheel event (up / away = zoom in).
    pub fn push(&mut self, delta: ScrollDelta) -> i32 {
        match delta {
            ScrollDelta::Lines(l) => {
                self.pixels = 0.;
                if l.y > 0. {
                    1
                } else if l.y < 0. {
                    -1
                } else {
                    0
                }
            }
            ScrollDelta::Pixels(p) => {
                // A reversal starts over.
                let y = f32::from(p.y);
                if y * self.pixels < 0. {
                    self.pixels = 0.;
                }
                self.pixels += y;
                let steps = (self.pixels / PIXELS_PER_STEP).trunc();
                self.pixels -= steps * PIXELS_PER_STEP;
                steps as i32
            }
        }
    }
}

/// The zoom indicator replaces itself instead of stacking.
struct ZoomToast;

impl MusicApp {
    /// ⌘= / ⌘- / ⌘0 and the wheel: `steps` levels in or out, 0 = 100 %.
    pub(crate) fn zoom_step(&mut self, steps: i32, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.settings.read(cx).settings().appearance.font_scale;
        let next = stepped(current, steps);
        if next != current {
            self.settings
                .update(cx, |panel, cx| panel.set_font_scale(next, cx));
        }
        let message = t!("zoom.toast", percent = (next * 100.).round());
        window.push_notification(
            gpui_component::notification::Notification::new()
                .message(message)
                .id::<ZoomToast>(),
            cx,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px};

    #[test]
    fn steps_clamp_and_reset() {
        assert_eq!(stepped(1.0, 1), 1.125);
        assert_eq!(stepped(1.0, -1), 0.875);
        assert_eq!(stepped(1.5, 1), 1.5);
        assert_eq!(stepped(0.75, -3), 0.75);
        assert_eq!(stepped(1.375, 0), 1.0);
        assert_eq!(stepped(1.2, 1), 1.375, "from the nearest level");
        assert_eq!(snap(f32::NAN), 1.0);
        // The Appearance buttons are zoom levels.
        for s in crate::theme::FONT_SCALES {
            assert!(STEPS.contains(&s));
        }
    }

    #[test]
    fn wheel_notches_and_trackpad_pixels() {
        let mut w = WheelZoom::default();
        assert_eq!(w.push(ScrollDelta::Lines(point(0., 3.))), 1);
        assert_eq!(w.push(ScrollDelta::Lines(point(0., -1.))), -1);
        assert_eq!(w.push(ScrollDelta::Lines(point(2., 0.))), 0);
        let px_delta = |y: f32| ScrollDelta::Pixels(point(px(0.), px(y)));
        assert_eq!(w.push(px_delta(40.)), 0);
        assert_eq!(w.push(px_delta(40.)), 1);
        assert_eq!(w.push(px_delta(130.)), 2);
        // Reversing drops what had built up.
        assert_eq!(w.push(px_delta(-50.)), 0);
        assert_eq!(w.push(px_delta(-20.)), -1);
    }

    #[test]
    fn saved_scales_snap_to_a_level() {
        let mut s: crate::settings::file::Settings =
            serde_json::from_str(r#"{"appearance": {"font_scale": 1.49}}"#).unwrap();
        s.normalize();
        assert_eq!(s.appearance.font_scale, 1.5);
        let mut s: crate::settings::file::Settings =
            serde_json::from_str(r#"{"appearance": {"font_scale": 0.7}}"#).unwrap();
        s.normalize();
        assert_eq!(s.appearance.font_scale, 0.75);
    }
}
