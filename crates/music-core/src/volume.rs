//! The volume slider's curve: a slider position (0..=1, what the UI shows and
//! saves) to the linear gain [`crate::Player::set_volume`] takes, and back.
//!
//! The curve is the square, `gain = position²` (Deviload's choice; Limusic uses
//! `10^(−(1−p)^1.5)`). Loudness follows the level in dB, not the linear gain, so a
//! straight slider puts nearly all of its audible change in its lowest fifth.
//! With the square the middle of the slider is −12 dB (about half as loud),
//! a tenth of it −40 dB, and it still reaches exact silence at 0 and unity at 1.
//! Limusic's curve was not taken: it ends at −20 dB (0.1) at the bottom, so it
//! needs a jump to silence and leaves no quiet settings below −20 dB.

/// Linear gain for a slider `position` (clamped to 0..=1; not finite → 1).
pub fn gain(position: f32) -> f32 {
    let p = if position.is_finite() {
        position.clamp(0.0, 1.0)
    } else {
        1.0
    };
    p * p
}

/// The slider position that gives `gain` (the inverse of [`gain`]); used to
/// carry a volume saved as a linear gain over to the curve.
pub fn position(gain: f32) -> f32 {
    if gain.is_finite() {
        gain.clamp(0.0, 1.0).sqrt()
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ends_and_middle() {
        assert_eq!(gain(0.0), 0.0);
        assert_eq!(gain(1.0), 1.0);
        let mid_db = 20.0 * gain(0.5).log10();
        assert!((mid_db + 12.04).abs() < 0.01, "{mid_db}");
        assert!((20.0 * gain(0.1).log10() + 40.0).abs() < 0.01);
        assert_eq!(gain(-1.0), 0.0);
        assert_eq!(gain(3.0), 1.0);
        assert_eq!(gain(f32::NAN), 1.0);
    }

    #[test]
    fn position_inverts_gain() {
        for i in 0..=100 {
            let p = i as f32 / 100.0;
            assert!((position(gain(p)) - p).abs() < 1e-6, "{p}");
        }
        // A saved linear volume keeps its loudness on the new slider.
        for g in [0.0, 0.1, 0.25, 0.4, 0.8, 1.0] {
            assert!((gain(position(g)) - g).abs() < 1e-6, "{g}");
        }
        assert_eq!(position(f32::NAN), 1.0);
    }

    #[test]
    fn is_monotonic() {
        let mut last = -1.0;
        for i in 0..=1000 {
            let g = gain(i as f32 / 1000.0);
            assert!(g > last || (i == 0 && g == 0.0));
            last = g;
        }
    }
}
