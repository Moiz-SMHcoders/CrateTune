// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/playback/SleepTimer.kt
// — © Rukamori and ArchiveTune contributors, GPL-3.0
//! Sleep timer: pause after a duration, or when the current track ends.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// What [`crate::Player::set_sleep_timer`] arms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SleepTimer {
    /// Pause once this much wall-clock time has passed.
    After(Duration),
    /// Pause when the current track ends (the queue moves on to the next
    /// track, which waits paused at its start).
    EndOfTrack,
}

/// The armed sleep timer, as seen in [`crate::PlaybackState::sleep_timer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SleepTimerState {
    /// Time left for [`SleepTimer::After`]; `None` for end of track.
    pub remaining: Option<Duration>,
    pub end_of_track: bool,
}

/// Timer bookkeeping, driven by the engine tick with explicit instants.
#[derive(Debug, Default, Clone)]
pub(crate) struct SleepClock {
    deadline: Option<Instant>,
    end_of_track: bool,
}

impl SleepClock {
    /// Arms `timer`, replacing whatever was armed (like ArchiveTune's `start`).
    pub(crate) fn start(&mut self, timer: Option<SleepTimer>, now: Instant) {
        *self = Self::default();
        match timer {
            Some(SleepTimer::After(d)) => self.deadline = Some(now + d),
            Some(SleepTimer::EndOfTrack) => self.end_of_track = true,
            None => {}
        }
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn is_active(&self) -> bool {
        self.deadline.is_some() || self.end_of_track
    }

    /// True when playback should pause at the end of the current track.
    pub(crate) fn pause_at_end(&self) -> bool {
        self.end_of_track
    }

    /// True (once) when the duration has run out; the timer is cleared.
    pub(crate) fn take_expired(&mut self, now: Instant) -> bool {
        if self.deadline.is_some_and(|d| now >= d) {
            self.clear();
            true
        } else {
            false
        }
    }

    /// The track ended (or the queue advanced): true (once) if the timer
    /// was waiting for that; the timer is cleared.
    pub(crate) fn take_track_end(&mut self) -> bool {
        if self.end_of_track {
            self.clear();
            true
        } else {
            false
        }
    }

    pub(crate) fn state(&self, now: Instant) -> Option<SleepTimerState> {
        self.is_active().then(|| SleepTimerState {
            remaining: self.deadline.map(|d| d.saturating_duration_since(now)),
            end_of_track: self.end_of_track,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_timer_counts_down_and_fires_once() {
        let t0 = Instant::now();
        let mut clock = SleepClock::default();
        assert_eq!(clock.state(t0), None);
        clock.start(Some(SleepTimer::After(Duration::from_secs(60))), t0);
        let s = clock.state(t0 + Duration::from_secs(15)).unwrap();
        assert_eq!(s.remaining, Some(Duration::from_secs(45)));
        assert!(!s.end_of_track);
        assert!(!clock.take_expired(t0 + Duration::from_secs(59)));
        assert!(!clock.take_track_end(), "not an end-of-track timer");
        assert!(clock.take_expired(t0 + Duration::from_secs(60)));
        assert!(!clock.take_expired(t0 + Duration::from_secs(61)));
        assert_eq!(clock.state(t0 + Duration::from_secs(61)), None);
    }

    #[test]
    fn end_of_track_timer_and_replacement() {
        let t0 = Instant::now();
        let mut clock = SleepClock::default();
        clock.start(Some(SleepTimer::EndOfTrack), t0);
        assert!(clock.pause_at_end());
        assert_eq!(
            clock.state(t0),
            Some(SleepTimerState {
                remaining: None,
                end_of_track: true
            })
        );
        assert!(!clock.take_expired(t0 + Duration::from_secs(3600)));
        // Re-arming replaces the previous mode.
        clock.start(Some(SleepTimer::After(Duration::from_secs(5))), t0);
        assert!(!clock.pause_at_end());
        clock.start(Some(SleepTimer::EndOfTrack), t0);
        assert!(clock.take_track_end());
        assert!(!clock.is_active());
        clock.start(Some(SleepTimer::After(Duration::from_secs(5))), t0);
        clock.start(None, t0);
        assert!(!clock.is_active());
    }
}
