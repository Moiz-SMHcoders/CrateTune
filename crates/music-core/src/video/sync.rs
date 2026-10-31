//! A/V sync decisions: which decoded frame belongs to the audio clock's
//! position, which are too late to show, and when the decoder has to be
//! moved instead of caught up with.
//!
//! The audio is the clock and is never adjusted. The picture follows it:
//! the newest frame not later than the clock is shown (frames passed over are
//! dropped, never shown late), nothing newer is shown early, and a clock that
//! jumped away from what was decoded (a seek, a restart, a long stall)
//! repositions the decoder at the clock instead of letting it run to catch
//! up. While the clock stands still (paused, buffering) the frame of that
//! moment stays on screen.

use std::time::Duration;

/// A frame may be shown this much before its time (half a frame at 60 fps:
/// redraws don't land exactly on frame times).
pub const EARLY: Duration = Duration::from_millis(8);
/// Decoded frames this far ahead of the clock mean the clock went back.
pub const AHEAD_LIMIT: Duration = Duration::from_millis(600);
/// The decoder this far behind the clock is moved rather than caught up with.
pub const BEHIND_LIMIT: Duration = Duration::from_millis(900);

/// What to do for one redraw, given the presentation times of the decoded
/// frames waiting (ascending) and the video time the clock is at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// Show frame `index`; the ones before it are late and dropped.
    Show { index: usize },
    /// Keep the frame on screen (the next one isn't due yet, or is still
    /// being decoded).
    Keep,
    /// Reposition the decoder at this time (the frame on screen stays until
    /// the new ones arrive).
    Seek(Duration),
}

/// The decoder's side, as the presenter sees it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DecoderPosition {
    /// Presentation time of the newest frame decoded since the last
    /// reposition (`None`: none yet).
    pub decoded_to: Option<Duration>,
    /// A reposition requested and not finished yet.
    pub seeking_to: Option<Duration>,
    /// The decoder is past the last frame of the video.
    pub ended: bool,
}

pub fn choose(pending: &[Duration], clock: Duration, decoder: DecoderPosition) -> Choice {
    if let Some(target) = decoder.seeking_to {
        // Already on its way: only a clock that moved far from it again
        // (another seek meanwhile) asks for a new one.
        if target.abs_diff(clock) > BEHIND_LIMIT {
            return Choice::Seek(clock);
        }
        return show_due(pending, clock).unwrap_or(Choice::Keep);
    }
    // What's decoded lies far ahead: the clock went back.
    if let Some(first) = pending.first()
        && *first > clock + AHEAD_LIMIT
    {
        return Choice::Seek(clock);
    }
    // ...or far behind: the clock jumped ahead (or decoding fell behind);
    // catching up frame by frame would show the wrong moment for seconds.
    let newest = pending.last().copied().or(decoder.decoded_to);
    if !decoder.ended && newest.is_some_and(|newest| newest + BEHIND_LIMIT < clock) {
        return Choice::Seek(clock);
    }
    show_due(pending, clock).unwrap_or(Choice::Keep)
}

/// The newest frame that is due, if any.
fn show_due(pending: &[Duration], clock: Duration) -> Option<Choice> {
    let due = pending.partition_point(|pts| *pts <= clock + EARLY);
    due.checked_sub(1).map(|index| Choice::Show { index })
}

/// How long until the next frame is due at `speed` (for scheduling the next
/// redraw), at least `min_gap` (the frame-rate cap).
pub fn next_due(
    pending: &[Duration],
    clock: Duration,
    speed: f32,
    min_gap: Duration,
) -> Option<Duration> {
    let next = pending.iter().find(|pts| **pts > clock + EARLY)?;
    let wait = (*next - clock).div_f32(speed.max(0.1));
    Some(wait.max(min_gap))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    fn frames(from: u64, n: u64) -> Vec<Duration> {
        (0..n).map(|i| ms(from + i * 40)).collect()
    }

    const IDLE: DecoderPosition = DecoderPosition {
        decoded_to: None,
        seeking_to: None,
        ended: false,
    };

    #[test]
    fn shows_the_newest_due_frame_and_drops_the_late_ones() {
        let q = frames(1000, 8); // 1000, 1040, … 1280
        assert_eq!(choose(&q, ms(1000), IDLE), Choice::Show { index: 0 });
        assert_eq!(choose(&q, ms(1031), IDLE), Choice::Show { index: 0 });
        // A slightly early redraw still gets the frame about to be due.
        assert_eq!(choose(&q, ms(1035), IDLE), Choice::Show { index: 1 });
        // A redraw that came late skips straight to the right frame.
        assert_eq!(choose(&q, ms(1130), IDLE), Choice::Show { index: 3 });
        // Nothing is shown before its time.
        assert_eq!(choose(&q, ms(900), IDLE), Choice::Keep);
    }

    #[test]
    fn a_jump_repositions_instead_of_drifting() {
        let q = frames(1000, 8);
        // Seek back: what's decoded is far ahead of the clock.
        assert_eq!(choose(&q, ms(200), IDLE), Choice::Seek(ms(200)));
        // Seek forward: everything decoded is long past.
        let decoder = DecoderPosition {
            decoded_to: Some(ms(1280)),
            ..IDLE
        };
        assert_eq!(choose(&[], ms(5000), decoder), Choice::Seek(ms(5000)));
        // A frame or two behind (decoding the next one): wait for it.
        assert_eq!(choose(&[], ms(1300), decoder), Choice::Keep);
        // Past the end of the video: the last frame stays.
        let ended = DecoderPosition {
            ended: true,
            ..decoder
        };
        assert_eq!(choose(&[], ms(9000), ended), Choice::Keep);
    }

    #[test]
    fn a_pending_seek_is_not_repeated_but_a_new_jump_is() {
        let seeking = DecoderPosition {
            seeking_to: Some(ms(5000)),
            ..IDLE
        };
        assert_eq!(choose(&[], ms(5100), seeking), Choice::Keep);
        // The first frames after the reposition arrive.
        let q = frames(5000, 4);
        assert_eq!(choose(&q, ms(5100), seeking), Choice::Show { index: 2 });
        // The user seeked again meanwhile.
        assert_eq!(choose(&q, ms(60_000), seeking), Choice::Seek(ms(60_000)));
    }

    #[test]
    fn a_queue_left_far_behind_is_not_shown() {
        // Frames up to 1.28 s, the clock at 2.5 s (a seek forward that landed
        // while the queue was full): reposition, don't show 1.28 s.
        let q = frames(1000, 8);
        assert_eq!(choose(&q, ms(2500), IDLE), Choice::Seek(ms(2500)));
        // Half a second behind: still shown, the decoder catches up.
        assert_eq!(choose(&q, ms(1700), IDLE), Choice::Show { index: 7 });
    }

    #[test]
    fn paused_holds_the_frame_of_the_moment() {
        let q = frames(1000, 8);
        for _ in 0..3 {
            assert_eq!(choose(&q, ms(1100), IDLE), Choice::Show { index: 2 });
        }
    }

    #[test]
    fn next_frame_timing_follows_speed_and_the_cap() {
        let q = frames(1000, 4);
        assert_eq!(next_due(&q, ms(1000), 1.0, ms(10)), Some(ms(40)));
        assert_eq!(next_due(&q, ms(1000), 2.0, ms(10)), Some(ms(20)));
        assert_eq!(next_due(&q, ms(1000), 2.0, ms(33)), Some(ms(33)));
        assert_eq!(next_due(&q, ms(2000), 1.0, ms(10)), None);
    }
}
