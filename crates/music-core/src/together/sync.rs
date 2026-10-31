// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/together/
// TogetherClock.kt and TogetherPlaybackSync.kt, © Rukamori and ArchiveTune
// contributors, GPL-3.0.
//! Clock sync and playback sync for guests.
//!
//! Every side counts milliseconds from its own start ("elapsed realtime"). A guest
//! pings, the server answers with its clock, and [`TogetherClock`] keeps a smoothed
//! estimate of `offset = server − client` (NTP-style: the server's reading is taken
//! to be at the midpoint of the round trip) and of the round-trip time.
//!
//! The host stamps each [`RoomState`] with its clock; a guest converts the stamp
//! into its own clock (`sent_at − offset`), adds the age of the message to the
//! host's position and seeks only when the drift exceeds a threshold
//! ([`GuestSync::plan`]).
//!
//! (ArchiveTune adds the offset where it should subtract it; that only works there
//! because the age is clamped to 0–5 s. Fixed here, see the tests.)

use super::messages::RoomState;

/// How often the host re-broadcasts the room state while nothing changes.
pub const BROADCAST_INTERVAL_MS: u64 = 500;
const MAX_REMOTE_AGE_MS: i64 = 5_000;
const PLAYING_SEEK_THRESHOLD_MS: u64 = 700;
const PAUSED_SEEK_THRESHOLD_MS: u64 = 150;
const ECHO_SUPPRESSION_MS: u64 = 700;
/// Don't bother seeking into the first moment of a freshly loaded track.
const MIN_LOAD_SEEK_MS: u64 = 1_500;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClockSnapshot {
    /// server − client, ms.
    pub offset_ms: i64,
    pub rtt_ms: u64,
    /// Pongs seen so far (0 = no estimate yet).
    pub samples: u32,
}

/// Smoothed offset / RTT estimate from heartbeat pongs.
#[derive(Debug, Clone, Default)]
pub struct TogetherClock {
    offset_ms: f64,
    rtt_ms: f64,
    samples: u32,
}

impl TogetherClock {
    /// `sent` / `received`: client clock around the ping; `server`: the pong's reading.
    pub fn on_pong(&mut self, sent: u64, received: u64, server: u64) -> ClockSnapshot {
        let rtt = received.saturating_sub(sent);
        let mid = sent as f64 + rtt as f64 / 2.0;
        let offset = server as f64 - mid;
        if self.samples == 0 {
            self.rtt_ms = rtt as f64;
            self.offset_ms = offset;
        } else {
            self.rtt_ms += (rtt as f64 - self.rtt_ms) * 0.15;
            // Jumps (a sleeping laptop, a new host) converge faster.
            let alpha = if (offset - self.offset_ms).abs() > 1500.0 {
                0.6
            } else {
                0.2
            };
            self.offset_ms += (offset - self.offset_ms) * alpha;
        }
        self.samples = self.samples.saturating_add(1);
        self.snapshot()
    }

    pub fn snapshot(&self) -> ClockSnapshot {
        ClockSnapshot {
            offset_ms: self.offset_ms.round() as i64,
            rtt_ms: self.rtt_ms.round() as u64,
            samples: self.samples,
        }
    }
}

/// Where the host is "now" (guest clock `now_ms`): its stamped position plus the
/// message's age when playing. Without a clock estimate the age is unknown (0).
pub fn target_position_ms(state: &RoomState, clock: Option<ClockSnapshot>, now_ms: u64) -> u64 {
    if !state.is_playing {
        return state.position_ms;
    }
    let age = match clock.filter(|c| c.samples > 0) {
        Some(clock) if state.sent_at_elapsed_realtime_ms > 0 => {
            let sent_local = state.sent_at_elapsed_realtime_ms as i64 - clock.offset_ms;
            (now_ms as i64 - sent_local).clamp(0, MAX_REMOTE_AGE_MS)
        }
        _ => 0,
    };
    state.position_ms + age as u64
}

pub fn should_seek_for_drift(current_ms: u64, target_ms: u64, playing: bool) -> bool {
    let threshold = if playing {
        PLAYING_SEEK_THRESHOLD_MS
    } else {
        PAUSED_SEEK_THRESHOLD_MS
    };
    current_ms.abs_diff(target_ms) > threshold
}

/// The guest's player as the app sees it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalPlayback {
    /// Video ids of the local queue, in order.
    pub queue_ids: Vec<String>,
    pub current_id: Option<String>,
    pub playing: bool,
    /// A track is loading / buffering: hold off seeks and play/pause.
    pub loading: bool,
    pub position_ms: u64,
}

/// What the guest should do to follow the host.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncPlan {
    /// Replace the local queue with the room's.
    pub replace_queue: bool,
    /// Start the room's track at this index (after `replace_queue`).
    pub play_index: Option<usize>,
    pub seek_ms: Option<u64>,
    pub set_playing: Option<bool>,
}

impl SyncPlan {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Guest-side state between room-state messages: drops stale / out-of-order states
/// and suppresses corrections right after one was applied (the local player needs a
/// moment to report the new position).
#[derive(Debug, Clone, Default)]
pub struct GuestSync {
    last_sent_at: u64,
    suppress_until: u64,
}

impl GuestSync {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// `None` if `remote` is older than one already applied.
    pub fn plan(
        &mut self,
        local: &LocalPlayback,
        remote: &RoomState,
        clock: Option<ClockSnapshot>,
        now_ms: u64,
    ) -> Option<SyncPlan> {
        let sent = remote.sent_at_elapsed_realtime_ms;
        if sent > 0 && self.last_sent_at > 0 && sent < self.last_sent_at {
            return None;
        }
        self.last_sent_at = sent;
        let mut plan = SyncPlan::default();
        let Some(current) = remote.current() else {
            if local.playing {
                plan.set_playing = Some(false);
            }
            return Some(plan);
        };
        let remote_ids: Vec<&str> = remote.queue.iter().map(|t| t.id.as_str()).collect();
        plan.replace_queue = local
            .queue_ids
            .iter()
            .map(String::as_str)
            .ne(remote_ids.iter().copied());
        let target = target_position_ms(remote, clock, now_ms);
        if local.current_id.as_deref() != Some(current.id.as_str()) {
            plan.play_index = Some(remote.current_index);
            if target >= MIN_LOAD_SEEK_MS {
                plan.seek_ms = Some(target);
            }
            self.suppress_until = now_ms + ECHO_SUPPRESSION_MS;
            return Some(plan);
        }
        if local.loading || now_ms < self.suppress_until {
            return Some(plan);
        }
        if should_seek_for_drift(local.position_ms, target, remote.is_playing) {
            plan.seek_ms = Some(target);
        }
        if local.playing != remote.is_playing {
            plan.set_playing = Some(remote.is_playing);
        }
        if plan.seek_ms.is_some() || plan.set_playing.is_some() {
            self.suppress_until = now_ms + ECHO_SUPPRESSION_MS;
        }
        Some(plan)
    }
}

/// What a guest does when it leaves a session, or the session ends under it (the
/// host ended it, kicked it, the connection dropped): "Keep playing here when I
/// leave" (phase 3 of `docs/plans/cross-device-resume.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeavePlan {
    /// Nothing to carry on with (no room state yet, an empty queue), or already
    /// stopped.
    Nothing,
    /// The setting is off: stop here.
    Pause,
    /// Carry on locally with the room's queue from where the room was.
    Continue {
        /// Put the room's queue in the local queue (current track first).
        replace_queue: bool,
        /// The local player isn't on the room's track yet: start it (index into the
        /// room's queue).
        play_index: Option<usize>,
        seek_ms: Option<u64>,
        /// Playing or paused, as the room was.
        playing: bool,
        /// The room's repeat mode (0 off, 1 one, 2 all).
        repeat_mode: u8,
    },
}

/// [`LeavePlan`] from the last room state the guest had and its own player.
pub fn leave_plan(
    keep_playing: bool,
    local: &LocalPlayback,
    room: Option<&RoomState>,
    clock: Option<ClockSnapshot>,
    now_ms: u64,
) -> LeavePlan {
    if !keep_playing {
        return if local.playing {
            LeavePlan::Pause
        } else {
            LeavePlan::Nothing
        };
    }
    let Some((room, current)) = room.and_then(|r| Some((r, r.current()?))) else {
        return LeavePlan::Nothing;
    };
    let target = target_position_ms(room, clock, now_ms);
    let replace_queue = local
        .queue_ids
        .iter()
        .map(String::as_str)
        .ne(room.queue.iter().map(|t| t.id.as_str()));
    let on_track = local.current_id.as_deref() == Some(current.id.as_str());
    let (play_index, seek_ms) = if on_track {
        let drifted =
            !local.loading && should_seek_for_drift(local.position_ms, target, room.is_playing);
        (None, drifted.then_some(target))
    } else {
        (
            Some(room.current_index),
            (target >= MIN_LOAD_SEEK_MS).then_some(target),
        )
    };
    LeavePlan::Continue {
        replace_queue,
        play_index,
        seek_ms,
        playing: room.is_playing,
        repeat_mode: room.repeat_mode,
    }
}

#[cfg(test)]
mod tests {
    use super::super::messages::TogetherTrack;
    use super::*;

    fn room(ids: &[&str], index: usize, playing: bool, pos: u64, sent: u64) -> RoomState {
        RoomState {
            queue: ids
                .iter()
                .map(|id| TogetherTrack {
                    id: id.to_string(),
                    ..TogetherTrack::default()
                })
                .collect(),
            current_index: index,
            is_playing: playing,
            position_ms: pos,
            sent_at_elapsed_realtime_ms: sent,
            ..RoomState::default()
        }
    }

    #[test]
    fn clock_estimates_offset_and_rtt() {
        // Server clock is 10 s ahead; 40 ms round trips, symmetric.
        let mut clock = TogetherClock::default();
        let s = clock.on_pong(1_000, 1_040, 11_020);
        assert_eq!((s.offset_ms, s.rtt_ms, s.samples), (10_000, 40, 1));
        // An asymmetric outlier moves the estimate only a little.
        let s = clock.on_pong(2_000, 2_200, 12_020);
        assert!((s.offset_ms - 10_000).abs() <= 20, "{s:?}");
        assert!(s.rtt_ms > 40 && s.rtt_ms < 80, "{s:?}");
        // A big jump converges quickly.
        let mut clock = TogetherClock::default();
        clock.on_pong(0, 10, 5);
        let s = clock.on_pong(100, 110, 20_105);
        assert!(s.offset_ms > 10_000, "{s:?}");
    }

    #[test]
    fn target_position_uses_the_offset_with_the_right_sign() {
        // Host clock = guest clock + 10 000. Host stamped at its 50 000 (= guest
        // 40 000) with position 30 s; the guest looks at guest-time 40 300.
        let clock = ClockSnapshot {
            offset_ms: 10_000,
            rtt_ms: 20,
            samples: 3,
        };
        let state = room(&["a"], 0, true, 30_000, 50_000);
        assert_eq!(target_position_ms(&state, Some(clock), 40_300), 30_300);
        // Age is capped, and never negative.
        assert_eq!(target_position_ms(&state, Some(clock), 99_000), 35_000);
        assert_eq!(target_position_ms(&state, Some(clock), 39_000), 30_000);
        // Paused: exactly the host's position; no estimate yet: no age.
        let paused = room(&["a"], 0, false, 30_000, 50_000);
        assert_eq!(target_position_ms(&paused, Some(clock), 40_300), 30_000);
        assert_eq!(target_position_ms(&state, None, 40_300), 30_000);
    }

    #[test]
    fn guest_follows_track_queue_drift_and_play_state() {
        let clock = Some(ClockSnapshot {
            offset_ms: 0,
            rtt_ms: 10,
            samples: 5,
        });
        let mut sync = GuestSync::default();
        let local = LocalPlayback::default();
        // Fresh guest: take the queue, load the track, seek into it.
        let plan = sync
            .plan(
                &local,
                &room(&["a", "b"], 1, true, 20_000, 1_000),
                clock,
                1_100,
            )
            .unwrap();
        assert_eq!(
            plan,
            SyncPlan {
                replace_queue: true,
                play_index: Some(1),
                seek_ms: Some(20_100),
                set_playing: None,
            }
        );
        // Older state arriving late is ignored.
        assert_eq!(
            sync.plan(&local, &room(&["a"], 0, true, 0, 900), clock, 1_200),
            None
        );
        let mut local = LocalPlayback {
            queue_ids: vec!["a".into(), "b".into()],
            current_id: Some("b".into()),
            playing: true,
            loading: false,
            position_ms: 20_400,
        };
        // In sync (within 700 ms) after the suppression window: nothing to do.
        let plan = sync
            .plan(
                &local,
                &room(&["a", "b"], 1, true, 20_500, 2_000),
                clock,
                2_000,
            )
            .unwrap();
        assert!(plan.is_empty(), "{plan:?}");
        // 2 s behind: seek.
        local.position_ms = 18_000;
        let plan = sync
            .plan(
                &local,
                &room(&["a", "b"], 1, true, 20_000, 3_000),
                clock,
                3_000,
            )
            .unwrap();
        assert_eq!(plan.seek_ms, Some(20_000));
        // Within the echo-suppression window, no new correction.
        let plan = sync
            .plan(
                &local,
                &room(&["a", "b"], 1, true, 20_500, 3_500),
                clock,
                3_500,
            )
            .unwrap();
        assert!(plan.is_empty());
        // Host paused: pause (and seek precisely when paused).
        local.position_ms = 20_900;
        let plan = sync
            .plan(
                &local,
                &room(&["a", "b"], 1, false, 21_000, 5_000),
                clock,
                5_000,
            )
            .unwrap();
        assert_eq!(plan.set_playing, Some(false));
        assert_eq!(plan.seek_ms, None, "100 ms off while paused is fine");
        // Host added a track: queue replaced, same track keeps playing.
        local.playing = false;
        let plan = sync
            .plan(
                &local,
                &room(&["a", "b", "c"], 1, false, 20_900, 6_000),
                clock,
                6_000,
            )
            .unwrap();
        assert!(plan.replace_queue && plan.play_index.is_none());
        // While loading nothing but the track itself is corrected.
        local.loading = true;
        local.position_ms = 0;
        let plan = sync
            .plan(
                &local,
                &room(&["a", "b"], 1, true, 50_000, 7_000),
                clock,
                7_000,
            )
            .unwrap();
        assert_eq!((plan.seek_ms, plan.set_playing), (None, None));
        // Empty room: stop.
        local.loading = false;
        local.playing = true;
        let plan = sync
            .plan(&local, &room(&[], 0, false, 0, 8_000), clock, 8_000)
            .unwrap();
        assert_eq!(plan.set_playing, Some(false));
    }

    #[test]
    fn leaving_keeps_playing_from_where_the_room_was() {
        let clock = Some(ClockSnapshot {
            offset_ms: 0,
            rtt_ms: 10,
            samples: 5,
        });
        let mut state = room(&["a", "b", "c"], 1, true, 30_000, 10_000);
        state.repeat_mode = 2;
        let following = LocalPlayback {
            queue_ids: vec!["a".into(), "b".into(), "c".into()],
            current_id: Some("b".into()),
            playing: true,
            loading: false,
            position_ms: 30_400,
        };
        // In sync: keep going as is, with the room's repeat mode.
        assert_eq!(
            leave_plan(true, &following, Some(&state), clock, 10_500),
            LeavePlan::Continue {
                replace_queue: false,
                play_index: None,
                seek_ms: None,
                playing: true,
                repeat_mode: 2,
            }
        );
        // Still loading the room's track (or on another one): start it at the room's
        // position; the queue becomes the room's.
        let behind = LocalPlayback {
            queue_ids: vec!["a".into()],
            current_id: Some("a".into()),
            playing: false,
            loading: true,
            position_ms: 0,
        };
        assert_eq!(
            leave_plan(true, &behind, Some(&state), clock, 11_000),
            LeavePlan::Continue {
                replace_queue: true,
                play_index: Some(1),
                seek_ms: Some(31_000),
                playing: true,
                repeat_mode: 2,
            }
        );
        // Drifted on the right track: seek.
        let drifted = LocalPlayback {
            position_ms: 20_000,
            ..following.clone()
        };
        assert!(matches!(
            leave_plan(true, &drifted, Some(&state), clock, 10_000),
            LeavePlan::Continue {
                seek_ms: Some(30_000),
                ..
            }
        ));
        // The room was paused: stay paused.
        let paused = room(&["a", "b", "c"], 1, false, 30_000, 10_000);
        assert!(matches!(
            leave_plan(true, &following, Some(&paused), clock, 10_500),
            LeavePlan::Continue { playing: false, .. }
        ));
        // Nothing to carry on with.
        assert_eq!(
            leave_plan(true, &following, None, clock, 0),
            LeavePlan::Nothing
        );
        assert_eq!(
            leave_plan(true, &following, Some(&room(&[], 0, false, 0, 0)), clock, 0),
            LeavePlan::Nothing
        );
        // The setting off: stop (if playing).
        assert_eq!(
            leave_plan(false, &following, Some(&state), clock, 10_500),
            LeavePlan::Pause
        );
        assert_eq!(
            leave_plan(false, &behind, Some(&state), clock, 10_500),
            LeavePlan::Nothing
        );
    }
}
