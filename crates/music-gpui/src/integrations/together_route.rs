//! Every control surface acting on a Music Together session: the player bar, media
//! keys and the OS's now-playing controls (MPRIS, SMTC, macOS remote commands), the
//! mini player, every Now Playing design, keyboard shortcuts, the menu-bar / tray
//! items, the queue panel, track and collection menus and scripts all end up in a
//! handful of `MusicApp` methods (`toggle_playback`, `next_track`, `seek_to`,
//! `play_list`, `queue_add`, `queue_remove`, …). Each of them first asks
//! [`MusicApp::together_route`] with an [`Intent`]:
//!
//! * not in a session, or this device runs it: the method does its work here (and,
//!   hosting, the sequencer and the activity notes hear of it);
//! * a guest that may: the intent goes to the host as a request (play / pause are
//!   explicit, a skip names the song it skips, a seek its target; see
//!   `music_core::together::session::Sequencer`), with play / pause and seeks also
//!   applied here at once so the button answers immediately;
//! * a guest that may not: nothing happens but a hint, at most once a while per kind
//!   (buttons show the same as a disabled state with a tooltip, [`MusicApp::transport_lock`]).
//!
//! Volume, mute, EQ / sound and the lyrics view never go through here: they stay local.
//! [`route`] is the decision alone (unit-tested); the rest applies it.

use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{Context, Window};
use music_core::Track;
use music_core::innertube::WatchTarget;
use music_core::together::session::{self as rules, Stamp, permissions};
use music_core::together::{
    AddTrackMode, ClearPart, ControlAction, IntentMeta, NoteKind, Participant, QueueOp, RoomState,
    SessionMode, TogetherTrack, is_shareable_id,
};

use super::together::{Role, SentState, SentSuggestion, repeat_code, repeat_of};
use crate::app::MusicApp;
use crate::queue::{QueueContext, Repeat, Source};

/// A hint of the same kind shows at most once per this long.
const HINT_EVERY: Duration = Duration::from_secs(20);
/// Suggestions a guest keeps track of (the Listen together page lists them).
const SUGGESTIONS_KEPT: usize = 12;

/// What someone asked for, on any surface.
#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    /// Play / pause, resolved to an explicit play or pause from what the session does.
    Toggle,
    Play,
    Pause,
    Next,
    Previous,
    Seek(Duration),
    Repeat(Repeat),
    Shuffle(bool),
    /// Play this queue slot.
    Jump(u64),
    Enqueue {
        tracks: Vec<Track>,
        mode: AddTrackMode,
    },
    /// Start a list at `index` (an album's Play, a click in a list).
    PlayList {
        tracks: Vec<Track>,
        index: usize,
        context: Option<String>,
        shuffle: bool,
    },
    PlayNow(Track),
    Radio(Track),
    RadioTarget {
        target: WatchTarget,
        label: String,
    },
    Remove(u64),
    Move {
        uid: u64,
        to_uid: u64,
    },
    Clear(ClearPart),
    /// Switching queue tabs (the session's queue can't be put away by a guest).
    QueueTab,
}

/// Why a guest can't do something here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Block {
    /// The host controls playback.
    NoControl,
    /// Only the host edits the queue.
    NoQueue,
    /// The host takes no songs.
    NoSuggestions,
    /// Waiting for the host to let this guest in.
    Waiting,
    /// Local files can't be shared.
    NotShareable,
    /// The session's queue can't be swapped for a queue tab.
    QueueTabs,
}

/// What to do with an intent.
#[derive(Debug, Clone, PartialEq)]
pub enum Routed {
    /// Here (not in a session, or this device runs it).
    Local,
    /// Ask the host to control playback.
    Control(ControlAction),
    /// Ask the host for a queue edit (Friends sessions).
    Queue(QueueOp),
    /// Ask the host to queue songs (straight in or as suggestions, the host decides).
    Suggest(Vec<TogetherTrack>, AddTrackMode),
    Blocked(Block),
}

/// Where this device stands, as [`route`] needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// Not in a session (or still connecting, or carrying a session on alone).
    Alone,
    /// This device runs the session.
    Host,
    Guest,
}

/// Shareable copies of `tracks` (local files can't travel).
fn shareable(tracks: &[Track]) -> Vec<TogetherTrack> {
    tracks
        .iter()
        .filter(|t| is_shareable_id(&t.video_id))
        .filter_map(TogetherTrack::from_track)
        .collect()
}

/// The decision for `intent` (`room`: the guest's last room state; `pending`:
/// waiting for approval; `me`: this guest's owner key, which marks its own songs).
pub fn route(
    intent: &Intent,
    standing: Standing,
    room: Option<&RoomState>,
    pending: bool,
    me: Option<&str>,
) -> Routed {
    match standing {
        Standing::Alone | Standing::Host => return Routed::Local,
        Standing::Guest => {}
    }
    if pending {
        return Routed::Blocked(Block::Waiting);
    }
    // Before the first room state: the default room (a host that takes songs).
    let settings = room.map(|r| r.settings).unwrap_or_default();
    let p = permissions(&settings, false);
    let friends = settings.mode == SessionMode::Friends;
    let control = |action: ControlAction| {
        if p.control {
            Routed::Control(action)
        } else {
            Routed::Blocked(Block::NoControl)
        }
    };
    let queue = |op: QueueOp| {
        if p.edit_queue {
            Routed::Queue(op)
        } else {
            Routed::Blocked(Block::NoQueue)
        }
    };
    // Removing or moving one song: any with full queue rights, the guest's own with
    // the right for those. Only songs the room names by slot: a host without our
    // queue edits (ArchiveTune) gives none.
    let song = |uid: u64, op: QueueOp| {
        let Some(track) = room.and_then(|r| r.queue.iter().find(|t| t.uid == Some(uid))) else {
            return Routed::Blocked(Block::NoQueue);
        };
        let owned = me.is_some() && track.added_by_key.as_deref() == me;
        if rules::may_edit_song(&settings, owned) {
            Routed::Queue(op)
        } else {
            Routed::Blocked(Block::NoQueue)
        }
    };
    // Songs where the host decides: straight in with Friends, suggested otherwise.
    let suggest = |tracks: &[Track], mode: AddTrackMode| {
        if !p.add {
            return Routed::Blocked(Block::NoSuggestions);
        }
        let shared = shareable(tracks);
        if shared.is_empty() {
            Routed::Blocked(Block::NotShareable)
        } else {
            Routed::Suggest(shared, mode)
        }
    };
    let playing = room.is_some_and(|r| r.is_playing);
    match intent {
        Intent::Toggle => control(if playing {
            ControlAction::Pause
        } else {
            ControlAction::Play
        }),
        Intent::Play => control(ControlAction::Play),
        Intent::Pause => control(ControlAction::Pause),
        Intent::Next => control(ControlAction::SkipNext),
        Intent::Previous => control(ControlAction::SkipPrevious),
        Intent::Seek(at) => control(ControlAction::SeekTo {
            position_ms: at.as_millis() as u64,
        }),
        Intent::Repeat(repeat) => control(ControlAction::SetRepeatMode {
            repeat_mode: repeat_code(*repeat),
        }),
        Intent::Shuffle(on) => control(ControlAction::SetShuffleEnabled {
            shuffle_enabled: *on,
        }),
        Intent::Jump(uid) => {
            if friends {
                return Routed::Queue(QueueOp::Jump { uid: *uid });
            }
            // A host without queue edits (Host mode, ArchiveTune): by position.
            let index = room.and_then(|r| r.queue.iter().position(|t| t.uid == Some(*uid)));
            match index {
                Some(index) => control(ControlAction::SeekToIndex {
                    index,
                    position_ms: 0,
                }),
                None if p.control => Routed::Blocked(Block::NoQueue),
                None => Routed::Blocked(Block::NoControl),
            }
        }
        Intent::Enqueue { tracks, mode } => suggest(tracks, *mode),
        Intent::PlayList {
            tracks,
            index,
            context,
            shuffle,
        } => {
            if friends {
                let shared: Vec<TogetherTrack> = tracks
                    .iter()
                    .filter(|t| is_shareable_id(&t.video_id))
                    .filter_map(TogetherTrack::from_track)
                    .collect();
                // The clicked song's place among the shareable ones.
                let start = tracks
                    .get(*index)
                    .and_then(|t| shared.iter().position(|s| s.id == t.video_id))
                    .unwrap_or(0);
                if shared.is_empty() {
                    return Routed::Blocked(Block::NotShareable);
                }
                return Routed::Queue(QueueOp::PlayList {
                    tracks: shared,
                    index: start,
                    context_name: context.clone(),
                    shuffle: *shuffle,
                });
            }
            // Elsewhere the clicked song is suggested to play next.
            match tracks.get(*index) {
                Some(track) => suggest(std::slice::from_ref(track), AddTrackMode::PlayNext),
                None => Routed::Blocked(Block::NotShareable),
            }
        }
        Intent::PlayNow(track) | Intent::Radio(track) => {
            if friends {
                let Some(shared) = shareable(std::slice::from_ref(track)).pop() else {
                    return Routed::Blocked(Block::NotShareable);
                };
                return Routed::Queue(match intent {
                    Intent::Radio(_) => QueueOp::Radio { track: shared },
                    _ => QueueOp::PlayNow { track: shared },
                });
            }
            suggest(std::slice::from_ref(track), AddTrackMode::PlayNext)
        }
        Intent::RadioTarget { target, label } if p.start_lists => {
            Routed::Queue(QueueOp::RadioTarget {
                video_id: target.video_id.clone(),
                playlist_id: target.playlist_id.clone(),
                params: target.params.clone(),
                label: label.clone(),
            })
        }
        Intent::RadioTarget { .. } => Routed::Blocked(Block::NoQueue),
        Intent::Remove(uid) => song(*uid, QueueOp::Remove { uid: *uid }),
        Intent::Move { uid, to_uid } => song(
            *uid,
            QueueOp::Move {
                uid: *uid,
                to_uid: *to_uid,
            },
        ),
        Intent::Clear(part) => queue(QueueOp::Clear { part: *part }),
        Intent::QueueTab => Routed::Blocked(Block::QueueTabs),
    }
}

/// The note an action leaves for the others (`None`: not worth one).
fn note_for(intent: &Intent, playing: bool) -> Option<(NoteKind, u32, Option<String>)> {
    Some(match intent {
        Intent::Toggle if playing => (NoteKind::Paused, 1, None),
        Intent::Toggle => (NoteKind::Played, 1, None),
        Intent::Pause if playing => (NoteKind::Paused, 1, None),
        Intent::Play if !playing => (NoteKind::Played, 1, None),
        Intent::Next => (NoteKind::Skipped, 1, None),
        Intent::Previous => (NoteKind::WentBack, 1, None),
        Intent::Enqueue { tracks, .. } => {
            let n = shareable(tracks).len() as u32;
            if n == 0 {
                return None;
            }
            (NoteKind::Added, n, None)
        }
        Intent::PlayList {
            tracks,
            index,
            context,
            ..
        } => (
            NoteKind::Started,
            1,
            context
                .clone()
                .or_else(|| tracks.get(*index).map(|t| t.title.clone())),
        ),
        Intent::PlayNow(track) | Intent::Radio(track) => {
            (NoteKind::Started, 1, Some(track.title.clone()))
        }
        Intent::RadioTarget { label, .. } => (NoteKind::Started, 1, Some(label.clone())),
        Intent::Remove(_) => (NoteKind::Removed, 1, None),
        Intent::Move { .. } => (NoteKind::Moved, 1, None),
        Intent::Clear(_) => (NoteKind::Cleared, 1, None),
        _ => return None,
    })
}

impl MusicApp {
    /// Where this device stands in a session, for [`route`].
    fn together_standing(&self) -> Standing {
        match &self.integrations.together.role {
            Role::Hosting(_) => Standing::Host,
            Role::Guest(_) => Standing::Guest,
            Role::Idle | Role::Joining(_) | Role::Recovering(_) => Standing::Alone,
        }
    }

    /// Every control surface asks here first. `true`: the session took the intent
    /// (sent to the host, or not allowed): the caller does nothing more. `false`: do
    /// it here (hosting, the sequencer and the notes have heard of it).
    pub(crate) fn together_route(&mut self, intent: Intent, cx: &mut Context<Self>) -> bool {
        let standing = self.together_standing();
        match standing {
            Standing::Alone => false,
            Standing::Host => {
                self.together_host_did(&intent, cx);
                false
            }
            Standing::Guest => {
                let (room, pending, me) = match &self.integrations.together.role {
                    Role::Guest(g) => (g.room.clone(), g.pending, g.owner_key.clone()),
                    _ => (None, false, String::new()),
                };
                let me = (!me.is_empty()).then_some(me.as_str());
                let routed = route(&intent, standing, room.as_ref(), pending, me);
                self.together_send(intent, routed, room.as_ref(), cx);
                true
            }
        }
    }

    /// Hosting: the host did something itself; the sequencer counts it as the newest
    /// of its kind and the others get a note.
    fn together_host_did(&mut self, intent: &Intent, cx: &mut Context<Self>) {
        let playing = self.playback.status == music_core::PlaybackStatus::Playing;
        let Role::Hosting(host) = &mut self.integrations.together.role else {
            return;
        };
        let now = host.server.elapsed_ms();
        let on = rules_playing(&self.queue);
        match intent {
            Intent::Toggle | Intent::Play | Intent::Pause => {
                host.sequencer.local(Stamp::Transport, &on, now)
            }
            Intent::Seek(_) => host.sequencer.local(Stamp::Seek, &on, now),
            _ => {}
        }
        let me = host.server.host_participant();
        if matches!(
            intent,
            Intent::PlayList { .. }
                | Intent::PlayNow(_)
                | Intent::Radio(_)
                | Intent::RadioTarget { .. }
        ) {
            host.context_by = Some((me.name.clone(), host.owner_key.clone()));
        }
        // A removed song is named in the note ("Ana removed “Song”").
        let removed = match intent {
            Intent::Remove(uid) => self
                .queue
                .upcoming()
                .iter()
                .find(|i| i.uid == *uid)
                .map(|i| i.track.title.clone()),
            _ => None,
        };
        if let Some((kind, count, title)) = note_for(intent, playing) {
            let title = title.or(removed);
            self.together_note(&me, kind, count, title, cx);
        }
    }

    /// A guest: sends what [`route`] decided (and answers at once where it can).
    fn together_send(
        &mut self,
        intent: Intent,
        routed: Routed,
        room: Option<&RoomState>,
        cx: &mut Context<Self>,
    ) {
        let current = room.and_then(RoomState::current);
        let host = self.integrations.together.host_name().unwrap_or_default();
        let Role::Guest(guest) = &mut self.integrations.together.role else {
            return;
        };
        let meta = guest
            .client
            .intent(current.map(|t| t.id.clone()), current.and_then(|t| t.uid));
        match routed {
            Routed::Local => {}
            Routed::Control(action) => {
                guest
                    .client
                    .request_control_with(action.clone(), Some(meta));
                // Play / pause and seeks answer here at once; the room confirms.
                let playing = self.playback.status == music_core::PlaybackStatus::Playing;
                match action {
                    ControlAction::Play | ControlAction::Pause => {
                        guest.hold_sync();
                        if (action == ControlAction::Play) != playing {
                            self.toggle_playback_local(cx);
                        }
                    }
                    ControlAction::SeekTo { position_ms } => {
                        guest.hold_sync();
                        self.seek_local(Duration::from_millis(position_ms), cx);
                    }
                    _ => {}
                }
            }
            Routed::Queue(op) => {
                guest.client.request_queue(op, Some(meta));
            }
            Routed::Suggest(tracks, mode) => {
                let friends = room.is_some_and(|r| r.settings.mode == SessionMode::Friends);
                let direct = room.is_some_and(|r| permissions(&r.settings, false).add_directly);
                let mut sent = Vec::new();
                for track in tracks {
                    if guest.client.request_add_track(track.clone(), mode) {
                        sent.push(SentSuggestion {
                            track,
                            state: SentState::Sent,
                        });
                    }
                }
                let n = sent.len();
                let title = sent
                    .first()
                    .map(|s| s.track.title.clone())
                    .unwrap_or_default();
                if !friends {
                    guest.suggestions.extend(sent);
                    while guest.suggestions.len() > SUGGESTIONS_KEPT {
                        guest.suggestions.pop_front();
                    }
                }
                let toast = match (friends || direct, n) {
                    (_, 0) => return,
                    // Straight into the shared queue: as the queue says it alone.
                    (true, 1) => match mode {
                        AddTrackMode::PlayNext => t!("queue.toast.play_next_one", title = title),
                        AddTrackMode::AddToQueue => t!("queue.toast.added_one", title = title),
                    },
                    (true, n) => match mode {
                        AddTrackMode::PlayNext => tp!("queue.toast.play_next_many", n),
                        AddTrackMode::AddToQueue => tp!("queue.toast.added_many", n),
                    },
                    (false, 1) => t!("together.toast.suggested_to", title = title, name = host),
                    (false, n) => tp!("together.toast.suggested_many_to", n, name = host),
                };
                self.together_toast(toast);
            }
            Routed::Blocked(block) => self.together_hint(block, &intent),
        }
        cx.notify();
    }

    /// A toast from a place without the window (the poll shows it).
    pub(super) fn together_toast(&mut self, text: String) {
        self.integrations.together.toasts.push(text);
        crate::poll::wake();
    }

    /// "You can't do that here", at most once a while per kind (the buttons show it
    /// as their disabled state already).
    fn together_hint(&mut self, block: Block, _intent: &Intent) {
        let together = &mut self.integrations.together;
        let now = Instant::now();
        if together
            .hints_shown
            .get(&block)
            .is_some_and(|at| now.duration_since(*at) < HINT_EVERY)
        {
            return;
        }
        together.hints_shown.insert(block, now);
        let text = self.together_block_text(block);
        self.integrations.together.pending_hint = Some((block, text));
        crate::poll::wake();
    }

    /// What a [`Block`] says, with the host's name.
    pub(crate) fn together_block_text(&self, block: Block) -> String {
        let together = &self.integrations.together;
        let host = together.host_name().unwrap_or_default();
        let dj = together.mode() == Some(SessionMode::Dj);
        match block {
            Block::NoControl if dj => t!("together.hint.no_control_dj", name = host),
            Block::NoControl => t!("together.hint.no_control", name = host),
            Block::NoQueue => t!("together.hint.no_queue", name = host),
            Block::NoSuggestions => t!("together.hint.no_suggestions", name = host),
            Block::Waiting => t!("together.hint.waiting", name = host),
            Block::NotShareable => t!("together.toast.only_youtube_music").into(),
            Block::QueueTabs => t!("queue.tabs.toast_guest").into(),
        }
    }

    /// Why this person can't use play / pause / skip / seek now (for the buttons'
    /// disabled state and tooltip), or `None`.
    pub(crate) fn transport_lock(&self) -> Option<String> {
        let together = &self.integrations.together;
        let Role::Guest(guest) = &together.role else {
            return None;
        };
        if guest.pending {
            return Some(self.together_block_text(Block::Waiting));
        }
        let p = together.permissions()?;
        (!p.control).then(|| self.together_block_text(Block::NoControl))
    }

    /// Why this person can't reorder / remove / clear queued songs now, or `None`.
    pub(crate) fn queue_lock(&self) -> Option<String> {
        let together = &self.integrations.together;
        let Role::Guest(guest) = &together.role else {
            return None;
        };
        if guest.pending {
            return Some(self.together_block_text(Block::Waiting));
        }
        let p = together.permissions()?;
        (!p.edit_queue).then(|| self.together_block_text(Block::NoQueue))
    }

    /// Why this person can't remove or move the song in slot `uid` now (a guest
    /// outside its rights: not its own song, or no song edits in this mode), or `None`.
    pub(crate) fn song_lock(&self, uid: u64) -> Option<String> {
        let together = &self.integrations.together;
        let Role::Guest(guest) = &together.role else {
            return None;
        };
        if guest.pending {
            return Some(self.together_block_text(Block::Waiting));
        }
        (!together.may_edit_song(uid).unwrap_or(true))
            .then(|| self.together_block_text(Block::NoQueue))
    }

    // ---- the host applies the guests' requests ------------------------------------

    /// Host: a guest's playback request, if the sequencer admits it.
    pub(super) fn together_apply_control(
        &mut self,
        participant: &Participant,
        action: ControlAction,
        intent: Option<IntentMeta>,
        cx: &mut Context<Self>,
    ) {
        let playing = self.playback.status == music_core::PlaybackStatus::Playing;
        let on = rules_playing(&self.queue);
        let Role::Hosting(host) = &mut self.integrations.together.role else {
            return;
        };
        let now = host.server.elapsed_ms();
        if !host.sequencer.admit(&action, intent.as_ref(), &on, now) {
            if self.trace.is_some() {
                eprintln!(
                    "music-gpui: together: dropped {action:?} from {}",
                    participant.name
                );
            }
            return;
        }
        self.integrations
            .together
            .log(format!("{}: {action:?}", participant.name));
        let note = match &action {
            ControlAction::Play if !playing => Some(NoteKind::Played),
            ControlAction::Pause if playing => Some(NoteKind::Paused),
            ControlAction::SkipNext => Some(NoteKind::Skipped),
            ControlAction::SkipPrevious => Some(NoteKind::WentBack),
            _ => None,
        };
        match action {
            ControlAction::Play if !playing => self.toggle_playback_local(cx),
            ControlAction::Pause if playing => self.toggle_playback_local(cx),
            ControlAction::Play | ControlAction::Pause => {}
            ControlAction::SeekTo { position_ms } => {
                self.seek_local(Duration::from_millis(position_ms), cx)
            }
            ControlAction::SkipNext => self.next_track_local(cx),
            ControlAction::SkipPrevious => self.previous_track_local(cx),
            ControlAction::SeekToIndex { index, position_ms } => {
                // The room's queue is the current track followed by the upcoming ones.
                let uid = self
                    .queue
                    .current_item()
                    .into_iter()
                    .chain(self.queue.upcoming())
                    .nth(index)
                    .map(|i| i.uid);
                if let Some(uid) = uid {
                    self.jump_to_slot(uid, position_ms, cx);
                }
            }
            ControlAction::SeekToTrack {
                track_id,
                position_ms,
            } => {
                let uid = self
                    .queue
                    .current_item()
                    .into_iter()
                    .chain(self.queue.upcoming())
                    .find(|i| i.track.video_id == track_id)
                    .map(|i| i.uid);
                if let Some(uid) = uid {
                    self.jump_to_slot(uid, position_ms, cx);
                }
            }
            ControlAction::SetRepeatMode { repeat_mode } => {
                self.set_repeat_local(repeat_of(repeat_mode), cx)
            }
            ControlAction::SetShuffleEnabled { shuffle_enabled } => {
                self.set_shuffle_local(shuffle_enabled, cx)
            }
        }
        if let Some(kind) = note {
            self.together_note(participant, kind, 1, None, cx);
        }
    }

    /// Plays queue slot `uid` from `position_ms` (the current one: a seek).
    fn jump_to_slot(&mut self, uid: u64, position_ms: u64, cx: &mut Context<Self>) {
        if self.queue.current_item().map(|i| i.uid) == Some(uid) {
            self.seek_local(Duration::from_millis(position_ms), cx);
            return;
        }
        self.queue_jump_local(uid, cx);
        if position_ms > 1_500
            && let Some(track) = self.queue.current().cloned()
        {
            self.integrations
                .devices
                .pending_seek_into(&track, Some(position_ms));
        }
    }

    /// Host: a guest's queue edit (Friends sessions; a jump also with playback control).
    pub(super) fn together_apply_queue_op(
        &mut self,
        participant: &Participant,
        op: QueueOp,
        _intent: Option<IntentMeta>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let on = rules_playing(&self.queue);
        let who = self.together_who(participant);
        let Role::Hosting(host) = &mut self.integrations.together.role else {
            return;
        };
        if !host.sequencer.admit_queue(&op, &on) {
            return;
        }
        // Removing or moving a song: anyone's with full queue rights, else only the
        // guest's own (the server let it through as far as it could tell).
        if let QueueOp::Remove { uid } | QueueOp::Move { uid, .. } = &op {
            let owned =
                !who.1.is_empty() && host.added_by.get(uid).is_some_and(|(_, key)| *key == who.1);
            if !rules::may_edit_song(&host.room, owned) {
                host.server.refuse(&participant.id, "edit the queue"); // i18n-ignore
                return;
            }
        }
        let title_of = |uid: u64| {
            self.queue
                .upcoming()
                .iter()
                .find(|i| i.uid == uid)
                .map(|i| i.track.title.clone())
        };
        let (note, title) = match &op {
            QueueOp::Jump { uid } => (
                NoteKind::Started,
                self.queue
                    .upcoming()
                    .iter()
                    .find(|i| i.uid == *uid)
                    .map(|i| i.track.title.clone()),
            ),
            QueueOp::Remove { uid } => (NoteKind::Removed, title_of(*uid)),
            QueueOp::Move { .. } => (NoteKind::Moved, None),
            QueueOp::Clear { .. } => (NoteKind::Cleared, None),
            QueueOp::PlayList {
                tracks,
                index,
                context_name,
                ..
            } => (
                NoteKind::Started,
                context_name
                    .clone()
                    .or_else(|| tracks.get(*index).map(|t| t.title.clone())),
            ),
            QueueOp::PlayNow { track } | QueueOp::Radio { track } => {
                (NoteKind::Started, Some(track.title.clone()))
            }
            QueueOp::RadioTarget { label, .. } => (NoteKind::Started, Some(label.clone())),
        };
        let before: HashSet<u64> = self.queue_uids();
        match op {
            QueueOp::Jump { uid } => self.jump_to_slot(uid, 0, cx),
            QueueOp::Remove { uid } => self.queue_remove_local(uid, cx),
            QueueOp::Move { uid, to_uid } => self.queue_move_to_local(uid, to_uid, cx),
            QueueOp::Clear { part } => match part {
                ClearPart::Queued => self.queue_clear_part_local(Source::User, cx),
                ClearPart::List => self.queue_clear_part_local(Source::Context, cx),
                ClearPart::All => self.queue_clear_local(cx),
            },
            QueueOp::PlayList {
                tracks,
                index,
                context_name,
                shuffle,
            } => {
                self.together_set_context_by(&who);
                let tracks: Vec<Track> = tracks.iter().map(TogetherTrack::to_track).collect();
                if shuffle && !self.queue.shuffle() {
                    self.set_shuffle_local(true, cx);
                }
                let context = context_name.map(QueueContext::list);
                self.play_list_in_local(Rc::new(tracks), index, context, cx);
            }
            QueueOp::PlayNow { track } => {
                self.together_set_context_by(&who);
                self.play_now_local(track.to_track(), cx);
            }
            QueueOp::Radio { track } => {
                self.together_set_context_by(&who);
                self.start_radio_local(track.to_track(), window, cx);
            }
            QueueOp::RadioTarget {
                video_id,
                playlist_id,
                params,
                label,
            } => {
                self.together_set_context_by(&who);
                let target = WatchTarget {
                    video_id,
                    playlist_id,
                    params,
                    ..WatchTarget::default()
                };
                self.start_radio_target_local(target, &label, window, cx);
            }
        }
        self.together_claim_new(&before, &who);
        self.together_note(participant, note, 1, title, cx);
    }

    fn together_set_context_by(&mut self, who: &super::together::Who) {
        if let Role::Hosting(host) = &mut self.integrations.together.role {
            host.context_by = Some(who.clone());
        }
    }
}

/// What plays here, as the sequencer compares requests with it.
fn rules_playing(queue: &crate::queue::Queue) -> music_core::together::Playing {
    music_core::together::Playing {
        track_id: queue.current().map(|t| t.video_id.clone()),
        uid: queue.current_item().map(|i| i.uid),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use music_core::together::RoomSettings;

    fn track(id: &str) -> Track {
        Track {
            video_id: id.into(),
            title: format!("Song {id}"),
            ..Track::default()
        }
    }

    fn room(mode: SessionMode, control: bool, playing: bool) -> RoomState {
        let base = RoomSettings {
            allow_guests_to_control_playback: control,
            ..RoomSettings::default()
        };
        RoomState {
            settings: mode.apply(base),
            is_playing: playing,
            queue: vec![
                TogetherTrack {
                    id: "aaaaaaaaaaa".into(),
                    uid: Some(10),
                    ..TogetherTrack::default()
                },
                TogetherTrack {
                    id: "bbbbbbbbbbb".into(),
                    uid: Some(11),
                    ..TogetherTrack::default()
                },
            ],
            ..RoomState::default()
        }
    }

    /// Every surface's intents (player bar, media keys, shortcuts, mini player, Now
    /// Playing, tray, queue panel, menus, scripts all call the same methods, which
    /// route these): what a guest's press does in each mode.
    #[test]
    fn control_routing_per_mode() {
        let g = Standing::Guest;
        let r = |intent: &Intent, room: &RoomState| route(intent, g, Some(room), false, None);
        let friends = room(SessionMode::Friends, false, true);
        let host_locked = room(SessionMode::Host, false, true);
        let host_open = room(SessionMode::Host, true, false);
        let dj = room(SessionMode::Dj, true, true);

        // Not in a session, or hosting: always here.
        for standing in [Standing::Alone, Standing::Host] {
            assert_eq!(
                route(&Intent::Next, standing, None, false, None),
                Routed::Local
            );
            assert_eq!(
                route(&Intent::Remove(1), standing, Some(&friends), false, None),
                Routed::Local
            );
        }
        // Play / pause is an intent from what the room does, not a toggle.
        assert_eq!(
            r(&Intent::Toggle, &friends),
            Routed::Control(ControlAction::Pause)
        );
        assert_eq!(
            r(&Intent::Toggle, &host_open),
            Routed::Control(ControlAction::Play)
        );
        // Transport: Friends and Host-with-control send; locked Host and DJ block.
        for (room, ok) in [
            (&friends, true),
            (&host_open, true),
            (&host_locked, false),
            (&dj, false),
        ] {
            let got = r(&Intent::Seek(Duration::from_secs(30)), room);
            if ok {
                assert_eq!(
                    got,
                    Routed::Control(ControlAction::SeekTo {
                        position_ms: 30_000
                    })
                );
            } else {
                assert_eq!(got, Routed::Blocked(Block::NoControl));
            }
        }
        assert_eq!(
            r(&Intent::Repeat(Repeat::All), &host_open),
            Routed::Control(ControlAction::SetRepeatMode { repeat_mode: 2 })
        );
        // Clearing: Friends, and Host with control.
        let clear = Intent::Clear(ClearPart::All);
        assert!(matches!(r(&clear, &friends), Routed::Queue(_)));
        assert!(matches!(r(&clear, &host_open), Routed::Queue(_)));
        assert_eq!(r(&clear, &host_locked), Routed::Blocked(Block::NoQueue));
        assert_eq!(r(&clear, &dj), Routed::Blocked(Block::NoQueue));
        // Playing a queue slot: a queue edit with Friends, by position with control.
        assert_eq!(
            r(&Intent::Jump(11), &friends),
            Routed::Queue(QueueOp::Jump { uid: 11 })
        );
        assert_eq!(
            r(&Intent::Jump(11), &host_open),
            Routed::Control(ControlAction::SeekToIndex {
                index: 1,
                position_ms: 0
            })
        );
        assert_eq!(
            r(&Intent::Jump(11), &host_locked),
            Routed::Blocked(Block::NoControl)
        );
        // "Play" on an album: everyone's with Friends, a suggestion elsewhere.
        let list = Intent::PlayList {
            tracks: vec![track("local:x"), track("ccccccccccc"), track("ddddddddddd")],
            index: 2,
            context: Some("Album".into()),
            shuffle: false,
        };
        let Routed::Queue(QueueOp::PlayList {
            tracks,
            index,
            context_name,
            ..
        }) = r(&list, &friends)
        else {
            panic!()
        };
        assert_eq!(
            (tracks.len(), index, context_name.as_deref()),
            (2, 1, Some("Album"))
        );
        let Routed::Suggest(tracks, AddTrackMode::PlayNext) = r(&list, &dj) else {
            panic!()
        };
        assert_eq!(tracks[0].id, "ddddddddddd");
        // Track menus: "Play next" / "Add to queue" / "Play now" / radio.
        let add = Intent::Enqueue {
            tracks: vec![track("eeeeeeeeeee")],
            mode: AddTrackMode::AddToQueue,
        };
        assert!(matches!(r(&add, &host_locked), Routed::Suggest(..)));
        assert!(matches!(
            r(&Intent::PlayNow(track("eeeeeeeeeee")), &friends),
            Routed::Queue(QueueOp::PlayNow { .. })
        ));
        assert!(matches!(
            r(&Intent::Radio(track("eeeeeeeeeee")), &host_locked),
            Routed::Suggest(_, AddTrackMode::PlayNext)
        ));
        // Local files can't travel; a host that takes no songs says so.
        let local = Intent::Enqueue {
            tracks: vec![track("local:1")],
            mode: AddTrackMode::PlayNext,
        };
        assert_eq!(r(&local, &friends), Routed::Blocked(Block::NotShareable));
        let mut closed = host_locked.clone();
        closed.settings.allow_guests_to_add_tracks = false;
        assert_eq!(r(&add, &closed), Routed::Blocked(Block::NoSuggestions));
        // Waiting to be let in: nothing goes out.
        assert_eq!(
            route(&Intent::Next, g, Some(&friends), true, None),
            Routed::Blocked(Block::Waiting)
        );
        // Queue tabs never swap the session's queue for a guest.
        assert_eq!(
            r(&Intent::QueueTab, &friends),
            Routed::Blocked(Block::QueueTabs)
        );
        // ArchiveTune host (no mode on the wire = Host), control allowed: plays by
        // position, never sends our queue edits.
        let mut archive = host_open.clone();
        archive.queue.iter_mut().for_each(|t| t.uid = None);
        assert_eq!(
            r(&Intent::Jump(11), &archive),
            Routed::Blocked(Block::NoQueue)
        );
        assert_eq!(
            r(&Intent::Remove(11), &archive),
            Routed::Blocked(Block::NoQueue)
        );
    }

    /// "Remove from queue" and reordering, per mode: anyone's song with Friends (and
    /// in Host mode with control), one's own in DJ mode and in Host mode while guests
    /// add songs, nothing in a Host session that takes no songs.
    #[test]
    fn removing_and_moving_songs_per_mode() {
        let g = Standing::Guest;
        let mine = Some("me-key");
        let mut rooms = [
            room(SessionMode::Friends, false, true),
            room(SessionMode::Host, false, true),
            room(SessionMode::Host, true, true),
            room(SessionMode::Dj, false, true),
        ];
        // Slot 11 is this guest's, slot 10 someone else's.
        for r in &mut rooms {
            r.queue[1].added_by_key = Some("me-key".into());
            r.queue[0].added_by_key = Some("ann-key".into());
        }
        let [friends, host_adds, host_control, dj] = &rooms;
        let mut host_closed = host_adds.clone();
        host_closed.settings.allow_guests_to_add_tracks = false;
        let remove =
            |uid: u64, room: &RoomState| route(&Intent::Remove(uid), g, Some(room), false, mine);
        let moved = |uid: u64, room: &RoomState| {
            route(
                &Intent::Move { uid, to_uid: 10 },
                g,
                Some(room),
                false,
                mine,
            )
        };
        let yes = |got: Routed| matches!(got, Routed::Queue(_));
        // (room, own song, someone else's)
        for (room, own, theirs) in [
            (friends, true, true),
            (host_control, true, true),
            (host_adds, true, false),
            (dj, true, false),
            (&host_closed, false, false),
        ] {
            assert_eq!(yes(remove(11, room)), own, "{:?}", room.settings);
            assert_eq!(yes(moved(11, room)), own, "{:?}", room.settings);
            assert_eq!(yes(remove(10, room)), theirs, "{:?}", room.settings);
            assert_eq!(yes(moved(10, room)), theirs, "{:?}", room.settings);
        }
        // Without knowing who it is, nothing counts as its own.
        assert!(!yes(route(&Intent::Remove(11), g, Some(dj), false, None)));
        // A slot the room doesn't know: nothing to send.
        assert!(!yes(remove(99, friends)));
    }

    #[test]
    fn notes_for_what_people_do() {
        assert_eq!(
            note_for(&Intent::Toggle, true),
            Some((NoteKind::Paused, 1, None))
        );
        assert_eq!(note_for(&Intent::Play, true), None);
        assert_eq!(note_for(&Intent::Seek(Duration::ZERO), true), None);
        let add = Intent::Enqueue {
            tracks: vec![track("aaaaaaaaaaa"), track("bbbbbbbbbbb"), track("local:1")],
            mode: AddTrackMode::AddToQueue,
        };
        assert_eq!(note_for(&add, true), Some((NoteKind::Added, 2, None)));
        assert_eq!(
            note_for(&Intent::PlayNow(track("aaaaaaaaaaa")), true),
            Some((NoteKind::Started, 1, Some("Song aaaaaaaaaaa".into())))
        );
    }
}
