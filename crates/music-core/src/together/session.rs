//! Session rules that don't depend on the network (ours, on top of the ArchiveTune
//! protocol): who may do what in each [`SessionMode`] ([`permissions`]), how the
//! device that sequences a session turns everyone's requests into one order of events
//! ([`Sequencer`]: doubled skips collapse, the newest seek / play / pause wins), who
//! takes a Friends session over when that device goes ([`failover`]), and the limits
//! on reactions and activity notes ([`RateLimit`], [`NoteFeed`]).
//!
//! # Friends sessions
//!
//! A Friends session still runs on one device (it holds the queue and stamps the
//! room state, so there is never a conflict to resolve), but nobody sees which: the
//! interface shows no host. That device sends this app's guests a
//! [`Message::SessionRoster`](super::Message::SessionRoster) with who would take over,
//! in join order. When it leaves on purpose it hands the session to the first of
//! them (the existing host transfer, [`super::TogetherServer::transfer_host`]); when it
//! vanishes (quit, crash, network gone) every guest finds its own place in the roster
//! ([`failover`]): the first takes over with the room as it last had it (its own
//! player is already in step, so nothing jumps), the others reconnect to it, and if
//! it doesn't come up the next one takes over. The session ends only when the last
//! person leaves (or only ArchiveTune guests are left, which can't take over).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::link::JoinInfo;
use super::messages::{
    ControlAction, IntentMeta, NoteKind, QueueOp, RoomSettings, SessionMode, Successor,
};

/// What someone in a session may do.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Permissions {
    /// Play, pause, skip, seek, jump to a song, repeat and shuffle.
    pub control: bool,
    /// Put songs in the queue (as a suggestion unless `add_directly`).
    pub add: bool,
    /// Songs go straight in (no approval).
    pub add_directly: bool,
    /// Remove, reorder and clear queued songs; start a list or a radio for everyone.
    pub edit_queue: bool,
    /// Switch the session's mode.
    pub switch_mode: bool,
    /// Remove people.
    pub moderate: bool,
}

/// What a participant may do in a room with `settings` (`host`: the device the
/// session runs on, which may do everything).
pub fn permissions(settings: &RoomSettings, host: bool) -> Permissions {
    if host {
        return Permissions {
            control: true,
            add: true,
            add_directly: true,
            edit_queue: true,
            switch_mode: true,
            moderate: true,
        };
    }
    let friends = settings.mode == SessionMode::Friends;
    Permissions {
        control: settings.allow_guests_to_control_playback,
        add: settings.allow_guests_to_add_tracks,
        add_directly: settings.allow_guests_to_add_tracks
            && !settings.require_approval_for_suggestions,
        edit_queue: friends,
        switch_mode: friends,
        moderate: friends,
    }
}

/// Whether a guest may make a queue edit (`op`) in a room with `settings`: a jump is
/// playback control, every other edit needs a Friends session.
pub fn queue_op_allowed(op: &QueueOp, settings: &RoomSettings) -> bool {
    let p = permissions(settings, false);
    match op {
        QueueOp::Jump { .. } => p.control || p.edit_queue,
        _ => p.edit_queue,
    }
}

/// The song playing on the device that sequences the session, as the [`Sequencer`]
/// compares requests against it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Playing {
    pub track_id: Option<String>,
    pub uid: Option<u64>,
}

/// Turns requests from everyone (each stamped with what its sender saw, see
/// [`IntentMeta`]) into one order of events on the device the session runs on:
///
/// * a skip (next / previous) names the song it skips; once that song is gone, more
///   skips of it are dropped, so two people pressing Next together skip once;
/// * a seek names its song and only applies to it; the newest seek (by the time it
///   was asked, on the host's clock) wins over older ones that arrive later;
/// * play and pause are intents, not toggles: the newest one wins, and asking for
///   what already happens changes nothing.
///
/// Requests without a stamp (ArchiveTune guests) are applied as they come. The host's
/// own actions go through [`Self::local`] so they count as the newest too.
#[derive(Debug, Clone, Default)]
pub struct Sequencer {
    last_transport_ms: u64,
    last_seek: Option<(Option<String>, u64)>,
}

/// Which kind of request a [`Sequencer`] stamp belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stamp {
    Transport,
    Seek,
}

impl Sequencer {
    /// Forgets every stamp (a new device sequences now, with a new clock).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Whether to apply `action`, asked with `meta`, while `now` plays; `server_ms` is
    /// the host's clock now. Records what it admits.
    pub fn admit(
        &mut self,
        action: &ControlAction,
        meta: Option<&IntentMeta>,
        now: &Playing,
        server_ms: u64,
    ) -> bool {
        let at = meta.map_or(server_ms, |m| m.at_ms.min(server_ms));
        let same_song = |meta: Option<&IntentMeta>| match meta {
            None => true,
            Some(m) => {
                let id_ok = m.track_id.is_none() || m.track_id == now.track_id;
                let uid_ok = m.uid.is_none() || now.uid.is_none() || m.uid == now.uid;
                id_ok && uid_ok
            }
        };
        match action {
            ControlAction::SkipNext | ControlAction::SkipPrevious => same_song(meta),
            ControlAction::SeekTo { .. } => {
                if !same_song(meta) {
                    return false;
                }
                if let Some((track, last)) = &self.last_seek
                    && *track == now.track_id
                    && meta.is_some()
                    && at < *last
                {
                    return false;
                }
                self.last_seek = Some((now.track_id.clone(), at));
                true
            }
            ControlAction::Play | ControlAction::Pause => {
                if meta.is_some() && at < self.last_transport_ms {
                    return false;
                }
                self.last_transport_ms = self.last_transport_ms.max(at);
                true
            }
            ControlAction::SeekToTrack { track_id, .. } => {
                // Already there: a doubled jump.
                !(meta.is_some() && now.track_id.as_deref() == Some(track_id.as_str()))
            }
            ControlAction::SeekToIndex { .. }
            | ControlAction::SetRepeatMode { .. }
            | ControlAction::SetShuffleEnabled { .. } => true,
        }
    }

    /// Whether to apply a queue edit: a jump to the slot that already plays is a
    /// doubled one; everything else applies in arrival order.
    pub fn admit_queue(&mut self, op: &QueueOp, now: &Playing) -> bool {
        match op {
            QueueOp::Jump { uid } => now.uid != Some(*uid),
            _ => true,
        }
    }

    /// The host did something itself (at `server_ms`): it is the newest of its kind.
    pub fn local(&mut self, kind: Stamp, now: &Playing, server_ms: u64) {
        match kind {
            Stamp::Transport => self.last_transport_ms = self.last_transport_ms.max(server_ms),
            Stamp::Seek => self.last_seek = Some((now.track_id.clone(), server_ms)),
        }
    }
}

/// What a guest does when the device its Friends session runs on is gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failover {
    /// Not in the roster (an older build, or it joined a moment ago): the session is
    /// over here.
    End,
    /// Take the session over (first in line, or everyone before has been tried).
    TakeOver,
    /// Reconnect to those before this guest in line, in order (each where it would
    /// listen); take over if none answers.
    Follow(Vec<JoinInfo>),
}

/// This guest's part when the session's device is gone: `successors` from the last
/// roster, `me` this guest's client id, `join` where the session was (a relay URL
/// stays the same; on the LAN each successor listens at its own address).
pub fn failover(successors: &[Successor], me: &str, join: &JoinInfo) -> Failover {
    let Some(rank) = successors.iter().position(|s| s.client_id == me) else {
        return Failover::End;
    };
    if rank == 0 {
        return Failover::TakeOver;
    }
    let before: Vec<JoinInfo> = successors[..rank]
        .iter()
        .filter_map(|s| successor_join(s, join))
        .collect();
    if before.is_empty() {
        Failover::TakeOver
    } else {
        Failover::Follow(before)
    }
}

/// Where a successor's session will be: the same relay, or its LAN address.
pub fn successor_join(successor: &Successor, join: &JoinInfo) -> Option<JoinInfo> {
    if join.is_relay() {
        return Some(join.clone());
    }
    match (&successor.host, successor.port) {
        (Some(host), Some(port)) if !host.is_empty() && port > 0 => Some(JoinInfo {
            host: host.clone(),
            port,
            url: None,
            ..join.clone()
        }),
        _ => None,
    }
}

/// How long a guest tries to reach each successor before the next one (or itself)
/// takes over.
pub const FAILOVER_WAIT: Duration = Duration::from_secs(6);

/// At most `burst` events, then one every `every` (a token bucket): reactions per
/// person, activity notes on screen.
#[derive(Debug, Clone)]
pub struct RateLimit {
    every: Duration,
    burst: u32,
    tokens: f64,
    last: Option<Instant>,
}

impl RateLimit {
    pub fn new(every: Duration, burst: u32) -> Self {
        Self {
            every,
            burst: burst.max(1),
            tokens: f64::from(burst.max(1)),
            last: None,
        }
    }

    /// Reactions one person may send: 4 at once, then one every 1.5 s.
    pub fn reactions() -> Self {
        Self::new(Duration::from_millis(1_500), 4)
    }

    /// Whether an event at `now` is allowed (it is counted if so).
    pub fn allow(&mut self, now: Instant) -> bool {
        if let Some(last) = self.last {
            let refill = now.saturating_duration_since(last).as_secs_f64()
                / self.every.as_secs_f64().max(0.001);
            self.tokens = (self.tokens + refill).min(f64::from(self.burst));
        }
        self.last = Some(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// One activity note as shown ("Ana skipped", "Sam added 3 songs").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub who: String,
    pub kind: NoteKind,
    pub count: u32,
    pub title: Option<String>,
    /// When it was last added to (merged notes keep the newest time).
    pub at: Instant,
}

/// Activity notes that stay quiet: the same person doing the same thing within
/// [`NoteFeed::MERGE`] is one note ("Sam added 3 songs", not three), at most one new
/// note shows per [`NoteFeed::SPACING`], each for [`NoteFeed::SHOW`], and only the
/// newest few wait (older ones are dropped, never stacked up).
#[derive(Debug, Clone, Default)]
pub struct NoteFeed {
    waiting: VecDeque<Note>,
    shown: Option<(Note, Instant)>,
}

impl NoteFeed {
    pub const MERGE: Duration = Duration::from_secs(5);
    pub const SPACING: Duration = Duration::from_millis(1_500);
    pub const SHOW: Duration = Duration::from_secs(4);
    const WAITING: usize = 3;

    /// Adds a note (merged into a waiting or showing one of the same person and kind).
    pub fn push(
        &mut self,
        who: &str,
        kind: NoteKind,
        count: u32,
        title: Option<String>,
        now: Instant,
    ) {
        let count = count.max(1);
        let mergeable = |n: &Note| {
            n.who == who
                && n.kind == kind
                && matches!(
                    kind,
                    NoteKind::Added | NoteKind::Removed | NoteKind::Skipped | NoteKind::Moved
                )
                && now.saturating_duration_since(n.at) < Self::MERGE
        };
        if let Some(n) = self.waiting.iter_mut().find(|n| mergeable(n)) {
            n.count += count;
            n.at = now;
            n.title = title.or(n.title.take());
            return;
        }
        if let Some((n, since)) = &mut self.shown
            && mergeable(n)
        {
            n.count += count;
            n.at = now;
            // Show the bigger number for a while longer.
            *since = now;
            return;
        }
        self.waiting.push_back(Note {
            who: who.to_owned(),
            kind,
            count,
            title,
            at: now,
        });
        while self.waiting.len() > Self::WAITING {
            self.waiting.pop_front();
        }
    }

    /// The note to show at `now` (advancing the feed), if any.
    pub fn current(&mut self, now: Instant) -> Option<&Note> {
        let expired = self
            .shown
            .as_ref()
            .is_some_and(|(_, since)| now.saturating_duration_since(*since) >= Self::SHOW);
        let spaced = self
            .shown
            .as_ref()
            .is_none_or(|(_, since)| now.saturating_duration_since(*since) >= Self::SPACING);
        if (expired || (spaced && !self.waiting.is_empty())) && self.shown.is_some() {
            self.shown = None;
        }
        if self.shown.is_none()
            && let Some(next) = self.waiting.pop_front()
        {
            self.shown = Some((next, now));
        }
        self.shown.as_ref().map(|(n, _)| n)
    }

    /// When the feed next changes (to render then), if it will.
    pub fn next_change(&self) -> Option<Instant> {
        let (_, since) = self.shown.as_ref()?;
        Some(if self.waiting.is_empty() {
            *since + Self::SHOW
        } else {
            *since + Self::SPACING
        })
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(track: &str, at: u64) -> Option<IntentMeta> {
        Some(IntentMeta {
            track_id: Some(track.into()),
            uid: None,
            at_ms: at,
        })
    }

    fn playing(track: &str) -> Playing {
        Playing {
            track_id: Some(track.into()),
            uid: None,
        }
    }

    #[test]
    fn simultaneous_skips_collapse_into_one() {
        let mut seq = Sequencer::default();
        // Ana and Sam both skip song A; Ana's arrives first and moves on to B.
        assert!(seq.admit(
            &ControlAction::SkipNext,
            meta("A", 100).as_ref(),
            &playing("A"),
            120
        ));
        assert!(!seq.admit(
            &ControlAction::SkipNext,
            meta("A", 105).as_ref(),
            &playing("B"),
            130
        ));
        // A skip of B (the song now playing) still works.
        assert!(seq.admit(
            &ControlAction::SkipNext,
            meta("B", 140).as_ref(),
            &playing("B"),
            150
        ));
        // Previous too; a stale slot (same song queued twice) is caught by the uid.
        let now = Playing {
            track_id: Some("C".into()),
            uid: Some(7),
        };
        let stale = IntentMeta {
            track_id: Some("C".into()),
            uid: Some(6),
            at_ms: 160,
        };
        assert!(!seq.admit(&ControlAction::SkipPrevious, Some(&stale), &now, 170));
        // ArchiveTune guests send no stamp: applied as they come.
        assert!(seq.admit(&ControlAction::SkipNext, None, &playing("C"), 180));
    }

    #[test]
    fn the_newest_seek_and_play_pause_win() {
        let mut seq = Sequencer::default();
        let now = playing("A");
        // Sam's seek (asked at 150) arrives before Ana's (asked at 100): Ana's is older.
        assert!(seq.admit(
            &ControlAction::SeekTo {
                position_ms: 60_000
            },
            meta("A", 150).as_ref(),
            &now,
            200
        ));
        assert!(!seq.admit(
            &ControlAction::SeekTo {
                position_ms: 30_000
            },
            meta("A", 100).as_ref(),
            &now,
            210
        ));
        assert!(seq.admit(
            &ControlAction::SeekTo {
                position_ms: 90_000
            },
            meta("A", 220).as_ref(),
            &now,
            230
        ));
        // A seek meant for another song is dropped.
        assert!(!seq.admit(
            &ControlAction::SeekTo { position_ms: 1 },
            meta("Z", 300).as_ref(),
            &now,
            310
        ));
        // The host's own seek counts as the newest.
        seq.local(Stamp::Seek, &now, 400);
        assert!(!seq.admit(
            &ControlAction::SeekTo { position_ms: 2 },
            meta("A", 390).as_ref(),
            &now,
            410
        ));
        // Play / pause: the newest intent wins; a late older one is dropped.
        assert!(seq.admit(&ControlAction::Pause, meta("A", 500).as_ref(), &now, 505));
        assert!(!seq.admit(&ControlAction::Play, meta("A", 450).as_ref(), &now, 510));
        assert!(seq.admit(&ControlAction::Play, meta("A", 520).as_ref(), &now, 525));
        // A stamp from the future (a clock estimate off) counts as now.
        assert!(seq.admit(&ControlAction::Pause, meta("A", 99_999).as_ref(), &now, 600));
        assert!(seq.admit(&ControlAction::Play, meta("A", 601).as_ref(), &now, 610));
        // A doubled jump to the song already playing is dropped.
        let jump = ControlAction::SeekToTrack {
            track_id: "A".into(),
            position_ms: 0,
        };
        assert!(!seq.admit(&jump, meta("Q", 700).as_ref(), &now, 700));
        let jump_uid = QueueOp::Jump { uid: 3 };
        let on3 = Playing {
            track_id: Some("A".into()),
            uid: Some(3),
        };
        assert!(!seq.admit_queue(&jump_uid, &on3));
        assert!(seq.admit_queue(&jump_uid, &now));
        // A new device sequences: stamps start over.
        seq.reset();
        assert!(seq.admit(&ControlAction::Play, meta("A", 1).as_ref(), &now, 2));
    }

    #[test]
    fn permissions_per_mode() {
        let base = RoomSettings::default();
        let host = SessionMode::Host.apply(base);
        let friends = SessionMode::Friends.apply(RoomSettings {
            allow_guests_to_add_tracks: false,
            require_approval_for_suggestions: true,
            ..base
        });
        let dj = SessionMode::Dj.apply(RoomSettings {
            allow_guests_to_control_playback: true,
            require_approval_for_suggestions: true,
            ..base
        });
        // Host mode: as the host set it (here: suggestions yes, control no).
        let p = permissions(&host, false);
        assert!(p.add && p.add_directly && !p.control && !p.edit_queue && !p.switch_mode);
        // Friends: everything, straight in, whatever the host's own settings said.
        let p = permissions(&friends, false);
        assert!(
            p.control && p.add && p.add_directly && p.edit_queue && p.switch_mode && p.moderate
        );
        assert!(friends.allow_guests_to_control_playback && friends.allow_guests_to_add_tracks);
        // DJ: add (with approval when asked for), never control.
        let p = permissions(&dj, false);
        assert!(p.add && !p.add_directly && !p.control && !p.edit_queue);
        // The host may do everything in every mode.
        for s in [host, friends, dj] {
            assert!(permissions(&s, true).edit_queue);
        }
        // Queue edits: a jump is playback control; the rest needs Friends.
        let jump = QueueOp::Jump { uid: 1 };
        let remove = QueueOp::Remove { uid: 1 };
        assert!(queue_op_allowed(&jump, &friends) && queue_op_allowed(&remove, &friends));
        assert!(!queue_op_allowed(&jump, &host) && !queue_op_allowed(&remove, &host));
        let controlled = RoomSettings {
            allow_guests_to_control_playback: true,
            ..host
        };
        assert!(queue_op_allowed(&jump, &controlled) && !queue_op_allowed(&remove, &controlled));
        assert!(!queue_op_allowed(&remove, &dj));
    }

    #[test]
    fn failover_follows_the_roster() {
        let lan = JoinInfo {
            host: "10.0.0.1".into(),
            port: 42117,
            session_id: "s".into(),
            session_key: "k".into(),
            url: None,
        };
        let succ = |id: &str, port: u16| Successor {
            participant_id: format!("p-{id}"),
            client_id: id.into(),
            name: id.into(),
            host: Some(format!("10.0.0.{port}")),
            port: Some(port),
        };
        let roster = vec![succ("bob", 2), succ("cat", 3), succ("dan", 4)];
        assert_eq!(failover(&roster, "bob", &lan), Failover::TakeOver);
        let Failover::Follow(order) = failover(&roster, "dan", &lan) else {
            panic!()
        };
        assert_eq!(
            order
                .iter()
                .map(|j| (j.host.as_str(), j.port))
                .collect::<Vec<_>>(),
            [("10.0.0.2", 2), ("10.0.0.3", 3)]
        );
        assert_eq!(order[0].session_key, "k");
        assert_eq!(failover(&roster, "eve", &lan), Failover::End);
        // Relayed: everyone reconnects to the same relay, which routes to whoever
        // claimed the room.
        let relay = JoinInfo::via_relay("wss://relay.example/", "s".into(), "k".into()).unwrap();
        let Failover::Follow(order) = failover(&roster, "cat", &relay) else {
            panic!()
        };
        assert_eq!(order, vec![relay.clone()]);
        // A successor without an address (no LAN standby) can't be followed.
        let mut lonely = roster.clone();
        lonely[0].host = None;
        assert_eq!(failover(&lonely[..2], "cat", &lan), Failover::TakeOver);
    }

    #[test]
    fn reactions_are_rate_limited() {
        let start = Instant::now();
        let mut limit = RateLimit::reactions();
        // A burst of four, then the fifth is dropped.
        for i in 0..4 {
            assert!(limit.allow(start + Duration::from_millis(i * 50)), "{i}");
        }
        assert!(!limit.allow(start + Duration::from_millis(250)));
        assert!(!limit.allow(start + Duration::from_millis(900)));
        // One more every 1.5 s.
        assert!(limit.allow(start + Duration::from_millis(2_000)));
        assert!(!limit.allow(start + Duration::from_millis(2_100)));
        // After a quiet while, the burst is back (not more).
        let later = start + Duration::from_secs(60);
        for i in 0..4 {
            assert!(limit.allow(later + Duration::from_millis(i)), "{i}");
        }
        assert!(!limit.allow(later + Duration::from_millis(10)));
    }

    #[test]
    fn notes_merge_and_never_stack_up() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut feed = NoteFeed::default();
        // Sam adds three songs in a row: one note, "3".
        for i in 0..3 {
            feed.push("Sam", NoteKind::Added, 1, None, ms(i * 300));
        }
        let shown = feed.current(ms(1_000)).unwrap().clone();
        assert_eq!(
            (shown.who.as_str(), shown.kind, shown.count),
            ("Sam", NoteKind::Added, 3)
        );
        // More while it shows: merged into it.
        feed.push("Sam", NoteKind::Added, 2, None, ms(1_200));
        assert_eq!(feed.current(ms(1_300)).unwrap().count, 5);
        // Many different notes at once: only the newest few wait, one at a time.
        for (i, who) in ["A", "B", "C", "D", "E"].iter().enumerate() {
            feed.push(who, NoteKind::Skipped, 1, None, ms(2_000 + i as u64));
        }
        // Still Sam's until it has been up for the spacing.
        assert_eq!(feed.current(ms(2_100)).unwrap().who, "Sam");
        assert_eq!(feed.current(ms(2_800)).unwrap().who, "C");
        assert_eq!(feed.current(ms(3_000)).unwrap().who, "C");
        assert_eq!(feed.current(ms(4_400)).unwrap().who, "D");
        assert_eq!(feed.current(ms(6_000)).unwrap().who, "E");
        // Nothing waiting: shown for its time, then gone.
        assert!(feed.next_change().is_some());
        assert!(feed.current(ms(10_100)).is_none());
        assert_eq!(feed.next_change(), None);
        // Different kinds of the same person don't merge; play / pause never do.
        feed.push("Ana", NoteKind::Paused, 1, None, ms(20_000));
        feed.push("Ana", NoteKind::Paused, 1, None, ms(20_100));
        assert_eq!(feed.current(ms(20_200)).unwrap().count, 1);
    }
}
