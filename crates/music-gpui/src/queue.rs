//! Play queue: what plays next, shuffle/repeat, and what has been played.
//!
//! Pure bookkeeping (no GPUI, no player), so the rules are unit-tested:
//!
//! * clicking a track in a list plays it with the whole list as the queue
//!   ([`Queue::replace`]); "play next" inserts right after the current track,
//!   "add to queue" appends; both start playback when the queue was empty;
//! * "next" (button, media key) moves one forward; at the end it stops, or
//!   wraps with repeat-all. When a track *ends*, repeat-one restarts it
//!   ([`Queue::track_ended`]);
//! * "previous" restarts the current track once it has played for more than
//!   [`RESTART_THRESHOLD`], otherwise goes one back (restarting the first
//!   track, or wrapping to the last one with repeat-all);
//! * shuffle keeps the current track first and shuffles the rest; turning it
//!   off restores the original order (including tracks added meanwhile);
//! * items can be removed (not the current one), moved, or cleared (everything
//!   but the current track);
//! * every track that was left is recorded in [`Queue::history`].
//!
//! **Your queue and the context** (as Spotify shows it; idea from Zuno, Apache-2.0,
//! `src/player/Queue.ts`): every item carries a [`Source`]. "Play next" and "Add to
//! queue" add [`Source::User`] items, everything else (the list you clicked, an
//! album, a radio, autoplay) is [`Source::Context`]. After the current track your
//! items always come first ("Next in queue"), then the context ("Next from …",
//! [`QueueContext`]): "add to queue" appends to the end of *your* part, starting a
//! new list keeps your pending items, each part can be cleared on its own, and
//! shuffle only shuffles the context. A user item that has played turns into a
//! context one (it is history now), so going back never splits the two parts.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use music_core::Track;
use serde::{Deserialize, Serialize};

/// "Previous" restarts the current track instead when it is past this position.
pub const RESTART_THRESHOLD: Duration = Duration::from_secs(3);
/// Played tracks kept in [`Queue::history`].
pub const HISTORY_CAP: usize = 200;

/// What "previous" should do.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)] // short-lived return value
pub enum Previous {
    /// Seek the current track back to 0.
    Restart,
    Play(Track),
}

/// What to do when the current track finished on its own.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)] // short-lived return value
pub enum Ended {
    /// Repeat-one: play it again.
    Restart,
    Play(Track),
    /// End of the queue.
    Stop,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

impl Repeat {
    /// The bar button cycles off → all → one → off.
    pub fn next(self) -> Self {
        match self {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "off" => Some(Repeat::Off),
            "all" => Some(Repeat::All),
            "one" => Some(Repeat::One),
            _ => None,
        }
    }
}

/// Who put an item in the queue.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Source {
    /// Part of what was started: the list, album, playlist, radio or autoplay.
    #[default]
    Context,
    /// "Play next" / "Add to queue": plays before the context.
    User,
}

/// What the context part of the queue is, for its "Next from …" heading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueContext {
    /// Album / playlist / page title, or the radio's seed.
    pub name: String,
    /// A radio or autoplay mix ("Next from <name> radio").
    pub radio: bool,
}

impl QueueContext {
    pub fn list(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            radio: false,
        }
    }

    pub fn radio(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            radio: true,
        }
    }
}

/// One queue slot. `uid` tells duplicates apart (the same song can be queued twice).
#[derive(Debug, Clone, PartialEq)]
pub struct QueueItem {
    pub uid: u64,
    pub track: Track,
    pub source: Source,
}

/// A queue as saved: tracks in their original order, the current one, the play order
/// while shuffled, which items you added and the context's name.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    pub tracks: Vec<Track>,
    pub index: usize,
    pub order: Option<Vec<usize>>,
    /// Indices into `tracks` of [`Source::User`] items.
    pub user: Vec<usize>,
    pub context: Option<QueueContext>,
}

pub struct Queue {
    /// Play order.
    items: Vec<QueueItem>,
    /// Order before shuffling (same items); only meaningful while `shuffle`.
    unshuffled: Vec<QueueItem>,
    index: Option<usize>,
    shuffle: bool,
    repeat: Repeat,
    /// Most recent last.
    history: VecDeque<Track>,
    next_uid: u64,
    rng: u64,
    /// What the context items are ("Next from …").
    context: Option<QueueContext>,
}

impl Default for Queue {
    fn default() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9E37_79B9_7F4A_7C15, |d| d.as_nanos() as u64);
        Self::with_seed(seed)
    }
}

impl Queue {
    /// A queue whose shuffles are reproducible (tests).
    pub fn with_seed(seed: u64) -> Self {
        Self {
            items: Vec::new(),
            unshuffled: Vec::new(),
            index: None,
            shuffle: false,
            repeat: Repeat::Off,
            history: VecDeque::new(),
            next_uid: 1,
            rng: seed | 1,
            context: None,
        }
    }

    // ---- reading ---------------------------------------------------------------

    pub fn current(&self) -> Option<&Track> {
        self.current_item().map(|i| &i.track)
    }

    pub fn current_item(&self) -> Option<&QueueItem> {
        self.items.get(self.index?)
    }

    /// Whether `video_id` is the queue's current track (e.g. the one that just ended).
    pub fn is_current(&self, video_id: &str) -> bool {
        self.current().is_some_and(|t| t.video_id == video_id)
    }

    /// Everything in play order.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn items(&self) -> &[QueueItem] {
        &self.items
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn index(&self) -> Option<usize> {
        self.index
    }

    /// Items after the current one, in play order.
    pub fn upcoming(&self) -> &[QueueItem] {
        match self.index {
            Some(i) => &self.items[i + 1..],
            None => &self.items,
        }
    }

    /// The leading run of [`Source::User`] items in [`Self::upcoming`]: "Next in queue".
    pub fn user_upcoming(&self) -> &[QueueItem] {
        let upcoming = self.upcoming();
        &upcoming[..self.user_len()]
    }

    /// The rest of [`Self::upcoming`]: "Next from …".
    pub fn context_upcoming(&self) -> &[QueueItem] {
        let upcoming = self.upcoming();
        &upcoming[self.user_len()..]
    }

    fn user_len(&self) -> usize {
        self.upcoming()
            .iter()
            .take_while(|i| i.source == Source::User)
            .count()
    }

    /// What the context items are, if known.
    pub fn context(&self) -> Option<&QueueContext> {
        self.context.as_ref()
    }

    pub fn set_context(&mut self, context: Option<QueueContext>) {
        self.context = context;
    }

    /// Where upcoming items start in `items`.
    fn upcoming_start(&self) -> usize {
        self.index.map_or(0, |i| i + 1)
    }

    /// Played tracks, most recent first.
    pub fn history(&self) -> impl Iterator<Item = &Track> {
        self.history.iter().rev()
    }

    pub fn shuffle(&self) -> bool {
        self.shuffle
    }

    pub fn repeat(&self) -> Repeat {
        self.repeat
    }

    fn next_index(&self) -> Option<usize> {
        let index = self.index?;
        if index + 1 < self.items.len() {
            Some(index + 1)
        } else if self.repeat == Repeat::All {
            Some(0)
        } else {
            None
        }
    }

    /// The track auto-advance / "next" would play.
    pub fn peek_next(&self) -> Option<&Track> {
        self.items.get(self.next_index()?).map(|i| &i.track)
    }

    pub fn has_next(&self) -> bool {
        self.next_index().is_some()
    }

    // ---- moving through the queue ---------------------------------------------

    fn record(&mut self, track: Track) {
        self.history.push_back(track);
        if self.history.len() > HISTORY_CAP {
            self.history.pop_front();
        }
    }

    /// Makes `index` current, recording the track being left in the history. Moving
    /// `forward`, a user item that was current has been played: it becomes part of the
    /// context (history), so it can never sit between context items later.
    fn go_to(&mut self, index: usize, forward: bool) -> Track {
        if let Some(old) = self.index {
            self.record(self.items[old].track.clone());
            if forward {
                self.items[old].source = Source::Context;
            }
        }
        self.index = Some(index);
        self.items[index].track.clone()
    }

    /// Starts playing `tracks[index]` with `tracks` as the queue.
    pub fn replace(&mut self, tracks: Vec<Track>, index: usize) -> Option<Track> {
        if index >= tracks.len() {
            return None;
        }
        if let Some(old) = self.current().cloned() {
            self.record(old);
        }
        self.context = None;
        let items: Vec<QueueItem> = tracks
            .into_iter()
            .map(|t| self.item(t, Source::Context))
            .collect();
        if self.shuffle {
            self.unshuffled = items.clone();
            let mut rest = items;
            let first = rest.remove(index);
            self.shuffle_slice(&mut rest);
            self.items = std::iter::once(first).chain(rest).collect();
            self.index = Some(0);
        } else {
            self.items = items;
            self.index = Some(index);
        }
        self.current().cloned()
    }

    /// Starts playing `tracks[index]` with `tracks` as the context (a click in a list,
    /// an album's Play): like [`Self::replace`], but your pending "Next in queue" items
    /// stay and play right after it.
    pub fn play_context(
        &mut self,
        tracks: Vec<Track>,
        index: usize,
        context: Option<QueueContext>,
    ) -> Option<Track> {
        if index >= tracks.len() {
            return None;
        }
        let pending: Vec<QueueItem> = self.user_upcoming().to_vec();
        let started = self.replace(tracks, index)?;
        self.context = context;
        if !pending.is_empty() {
            let at = self.upcoming_start();
            if self.shuffle {
                let current = self.items[at - 1].uid;
                let pos = self
                    .unshuffled
                    .iter()
                    .position(|i| i.uid == current)
                    .map_or(self.unshuffled.len(), |p| p + 1);
                self.unshuffled.splice(pos..pos, pending.iter().cloned());
            }
            self.items.splice(at..at, pending);
        }
        Some(started)
    }

    /// Moves to the next track (user "next"); `None` (and no change) at the end
    /// of the queue unless repeat-all wraps around.
    pub fn advance(&mut self) -> Option<Track> {
        let next = self.next_index()?;
        Some(self.go_to(next, true))
    }

    /// The current track finished playing.
    pub fn track_ended(&mut self) -> Ended {
        if self.index.is_none() {
            return Ended::Stop;
        }
        if self.repeat == Repeat::One {
            return Ended::Restart;
        }
        match self.advance() {
            Some(track) => Ended::Play(track),
            None => Ended::Stop,
        }
    }

    /// "Previous" at `position` into the current track.
    pub fn previous(&mut self, position: Duration) -> Previous {
        let Some(index) = self.index else {
            return Previous::Restart;
        };
        if position > RESTART_THRESHOLD {
            return Previous::Restart;
        }
        if index > 0 {
            Previous::Play(self.go_to(index - 1, false))
        } else if self.repeat == Repeat::All && self.items.len() > 1 {
            Previous::Play(self.go_to(self.items.len() - 1, false))
        } else {
            Previous::Restart
        }
    }

    /// Plays the queue item `uid` (a click in the queue panel). Jumping ahead into
    /// the context skips none of your queued items: they move to play right after it.
    pub fn jump(&mut self, uid: u64) -> Option<Track> {
        let mut index = self.position(uid)?;
        if Some(index) == self.index {
            return self.current().cloned();
        }
        let start = self.upcoming_start();
        let forward = index >= start;
        if forward && self.items[index].source == Source::User {
            // Jumping to one of your items skips the ones before it: they count as played.
            for item in &mut self.items[start..index] {
                item.source = Source::Context;
            }
        } else if forward {
            let skipped: Vec<QueueItem> = self.items[start..index]
                .iter()
                .filter(|i| i.source == Source::User)
                .cloned()
                .collect();
            if !skipped.is_empty() {
                self.items
                    .retain(|i| i.uid == uid || !skipped.iter().any(|s| s.uid == i.uid));
                index = self.position(uid)?;
                self.items.splice(index + 1..index + 1, skipped);
            }
        }
        Some(self.go_to(index, forward))
    }

    // ---- editing ---------------------------------------------------------------

    fn item(&mut self, track: Track, source: Source) -> QueueItem {
        let uid = self.next_uid;
        self.next_uid += 1;
        QueueItem { uid, track, source }
    }

    fn items_from(&mut self, tracks: Vec<Track>, source: Source) -> Vec<QueueItem> {
        tracks.into_iter().map(|t| self.item(t, source)).collect()
    }

    /// Restores "your items first" after the current track (stable: each part keeps
    /// its order).
    fn normalize(&mut self) {
        let start = self.upcoming_start().min(self.items.len());
        let tail = &self.items[start..];
        let users = tail.iter().filter(|i| i.source == Source::User).count();
        if tail[..users].iter().all(|i| i.source == Source::User) {
            return;
        }
        let tail: Vec<QueueItem> = self.items.drain(start..).collect();
        let (user, context): (Vec<_>, Vec<_>) =
            tail.into_iter().partition(|i| i.source == Source::User);
        self.items.extend(user);
        self.items.extend(context);
    }

    fn position(&self, uid: u64) -> Option<usize> {
        self.items.iter().position(|i| i.uid == uid)
    }

    /// Inserts `tracks` right after the current one. Returns the track to start
    /// when the queue had nothing current.
    pub fn play_next(&mut self, tracks: Vec<Track>) -> Option<Track> {
        let new = self.items_from(tracks, Source::User);
        if new.is_empty() {
            return None;
        }
        let Some(index) = self.index else {
            return Some(self.start_with(new));
        };
        if self.shuffle {
            let uid = self.items[index].uid;
            let at = self
                .unshuffled
                .iter()
                .position(|i| i.uid == uid)
                .map_or(self.unshuffled.len(), |p| p + 1);
            self.unshuffled.splice(at..at, new.iter().cloned());
        }
        self.items.splice(index + 1..index + 1, new);
        None
    }

    /// Adds `tracks` to the end of your queue ("Next in queue", before the context).
    /// Returns the track to start when the queue had nothing current.
    pub fn add(&mut self, tracks: Vec<Track>) -> Option<Track> {
        let new = self.items_from(tracks, Source::User);
        if new.is_empty() {
            return None;
        }
        if self.index.is_none() {
            return Some(self.start_with(new));
        }
        if self.shuffle {
            // Put back in place by `normalize` when shuffle is turned off.
            self.unshuffled.extend(new.iter().cloned());
        }
        let at = self.upcoming_start() + self.user_len();
        self.items.splice(at..at, new);
        None
    }

    /// Appends `tracks` to the context (radio refills, autoplay). Returns the track to
    /// start when the queue had nothing current.
    pub fn append_context(&mut self, tracks: Vec<Track>) -> Option<Track> {
        let new = self.items_from(tracks, Source::Context);
        if new.is_empty() {
            return None;
        }
        if self.index.is_none() {
            return Some(self.start_with(new));
        }
        if self.shuffle {
            self.unshuffled.extend(new.iter().cloned());
        }
        self.items.extend(new);
        None
    }

    /// Plays `track` now, keeping the rest of the queue after it.
    pub fn play_now(&mut self, track: Track) -> Track {
        if let Some(started) = self.play_next(vec![track.clone()]) {
            return started;
        }
        self.advance().unwrap_or(track)
    }

    /// Nothing current: the new items are appended and the first one starts.
    fn start_with(&mut self, items: Vec<QueueItem>) -> Track {
        let start = self.items.len();
        if self.shuffle {
            self.unshuffled.extend(items.iter().cloned());
        }
        self.items.extend(items);
        self.go_to(start, true)
    }

    /// Removes an item (not the current one). Returns whether something was removed.
    pub fn remove(&mut self, uid: u64) -> bool {
        let Some(pos) = self.position(uid) else {
            return false;
        };
        if Some(pos) == self.index {
            return false;
        }
        self.items.remove(pos);
        self.unshuffled.retain(|i| i.uid != uid);
        if let Some(index) = self.index
            && pos < index
        {
            self.index = Some(index - 1);
        }
        true
    }

    /// Moves the item at play-order position `from` to position `to`; the
    /// current track stays current.
    pub fn move_item(&mut self, from: usize, to: usize) -> bool {
        let len = self.items.len();
        if from >= len || to >= len || from == to {
            return false;
        }
        let current = self.current_item().map(|i| i.uid);
        let item = self.items.remove(from);
        self.items.insert(to, item);
        self.index = current.and_then(|uid| self.position(uid));
        // Dropped among your items it is yours, among the context's it is context.
        let start = self.upcoming_start();
        if to >= start {
            let before = (to > start).then(|| self.items[to - 1].source);
            let after = self.items.get(to + 1).map(|i| i.source);
            if let Some(source) = before.or(after) {
                self.items[to].source = source;
            }
        }
        true
    }

    /// Moves `uid` to where `target` is (drag and drop in the queue panel).
    pub fn move_to(&mut self, uid: u64, target: u64) -> bool {
        match (self.position(uid), self.position(target)) {
            (Some(from), Some(to)) => self.move_item(from, to),
            _ => false,
        }
    }

    /// Moves `uid` one slot up (`-1`) or down (`+1`).
    pub fn nudge(&mut self, uid: u64, delta: isize) -> bool {
        let Some(from) = self.position(uid) else {
            return false;
        };
        let Some(to) = from.checked_add_signed(delta) else {
            return false;
        };
        self.move_item(from, to)
    }

    /// Drops the upcoming items of one part ("Next in queue" or "Next from …"),
    /// keeping the other. Returns whether something was removed.
    pub fn clear_part(&mut self, source: Source) -> bool {
        let start = self.upcoming_start();
        let gone: Vec<u64> = self.items[start.min(self.items.len())..]
            .iter()
            .filter(|i| i.source == source)
            .map(|i| i.uid)
            .collect();
        if gone.is_empty() {
            return false;
        }
        self.items.retain(|i| !gone.contains(&i.uid));
        self.unshuffled.retain(|i| !gone.contains(&i.uid));
        if source == Source::Context {
            self.context = None;
        }
        true
    }

    /// Drops everything except the current track.
    pub fn clear(&mut self) {
        match self.current_item().cloned() {
            Some(current) => {
                self.items = vec![current.clone()];
                self.unshuffled = vec![current];
                self.index = Some(0);
            }
            None => {
                self.items.clear();
                self.unshuffled.clear();
                self.index = None;
            }
        }
        self.context = None;
    }

    pub fn set_repeat(&mut self, repeat: Repeat) {
        self.repeat = repeat;
    }

    /// Shuffle on: the current track moves first and the rest is shuffled.
    /// Off: the original order comes back with the current track still current.
    pub fn set_shuffle(&mut self, on: bool) {
        if on == self.shuffle {
            return;
        }
        self.shuffle = on;
        if on {
            self.unshuffled = self.items.clone();
            let mut rest = self.items.clone();
            let first = self.index.map(|i| rest.remove(i));
            // Your queued items stay first, in your order; only the context shuffles.
            let (user, mut context): (Vec<_>, Vec<_>) =
                rest.into_iter().partition(|i| i.source == Source::User);
            self.shuffle_slice(&mut context);
            self.items = first.into_iter().chain(user).chain(context).collect();
            if self.index.is_some() {
                self.index = Some(0);
            }
        } else {
            let current = self.current_item().map(|i| i.uid);
            // Sources changed while shuffled (played, moved): the play order has them.
            let sources: HashMap<u64, Source> =
                self.items.iter().map(|i| (i.uid, i.source)).collect();
            self.items = std::mem::take(&mut self.unshuffled);
            for item in &mut self.items {
                if let Some(source) = sources.get(&item.uid) {
                    item.source = *source;
                }
            }
            self.index = current.and_then(|uid| self.position(uid));
            self.normalize();
        }
    }

    /// xorshift64* — good enough for shuffling, no extra dependency.
    fn random(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn shuffle_slice(&mut self, items: &mut [QueueItem]) {
        for i in (1..items.len()).rev() {
            let j = (self.random() % (i as u64 + 1)) as usize;
            items.swap(i, j);
        }
    }
}

/// Saving and restoring across launches (`store.rs` keeps it in the library database).
impl Queue {
    /// Tracks in their original order, the current track's index in it, the play
    /// order as indices into it while shuffled, your items and the context.
    pub fn snapshot(&self) -> Snapshot {
        let base = if self.shuffle {
            &self.unshuffled
        } else {
            &self.items
        };
        let position = |uid: u64| base.iter().position(|i| i.uid == uid);
        let index = self
            .current_item()
            .and_then(|i| position(i.uid))
            .unwrap_or(0);
        let order = self
            .shuffle
            .then(|| self.items.iter().filter_map(|i| position(i.uid)).collect());
        // The play order has the up-to-date sources (the copy kept for unshuffling
        // may not).
        let user = self
            .items
            .iter()
            .filter(|i| i.source == Source::User)
            .filter_map(|i| position(i.uid))
            .collect();
        Snapshot {
            tracks: base.iter().map(|i| i.track.clone()).collect(),
            index,
            order,
            user,
            context: self.context.clone(),
        }
    }

    /// Replaces the queue with a [`Self::snapshot`] without playing anything. A valid
    /// `order` turns shuffle on with that play order; otherwise shuffle is off. Saved
    /// queues from before the two parts existed (no `user`) are all context.
    pub fn restore(&mut self, snapshot: Snapshot) {
        let Snapshot {
            tracks,
            index,
            order,
            user,
            context,
        } = snapshot;
        let mut base: Vec<QueueItem> = tracks
            .into_iter()
            .map(|t| self.item(t, Source::Context))
            .collect();
        for i in user {
            if let Some(item) = base.get_mut(i) {
                item.source = Source::User;
            }
        }
        self.context = context;
        if base.is_empty() {
            self.items.clear();
            self.unshuffled.clear();
            self.index = None;
            return;
        }
        let index = index.min(base.len() - 1);
        let valid = order.filter(|o| {
            let mut sorted = o.clone();
            sorted.sort_unstable();
            sorted.len() == base.len() && sorted.iter().enumerate().all(|(i, v)| i == *v)
        });
        match valid {
            Some(order) => {
                let current = base[index].uid;
                self.items = order.iter().map(|&i| base[i].clone()).collect();
                self.unshuffled = base;
                self.shuffle = true;
                self.index = self.position(current);
            }
            None => {
                self.items = base;
                self.unshuffled.clear();
                self.shuffle = false;
                self.index = Some(index);
            }
        }
        self.normalize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracks(n: usize) -> Vec<Track> {
        (0..n).map(|i| track(&format!("v{i}"))).collect()
    }

    fn track(id: &str) -> Track {
        Track {
            video_id: id.into(),
            title: format!("Song {id}"),
            artists: "Artist".into(),
            duration_secs: Some(200),
            ..Default::default()
        }
    }

    fn id(track: Option<Track>) -> Option<String> {
        track.map(|t| t.video_id)
    }

    fn order(q: &Queue) -> Vec<String> {
        q.items().iter().map(|i| i.track.video_id.clone()).collect()
    }

    fn order_of(tracks: &[Track]) -> Vec<String> {
        tracks.iter().map(|t| t.video_id.clone()).collect()
    }

    fn current(q: &Queue) -> &str {
        q.current().map_or("", |t| t.video_id.as_str())
    }

    fn uid(q: &Queue, id: &str) -> u64 {
        q.items()
            .iter()
            .find(|i| i.track.video_id == id)
            .unwrap()
            .uid
    }

    // ---- the original rules (unchanged semantics) ----

    #[test]
    fn empty_queue_does_nothing() {
        let mut q = Queue::default();
        assert!(q.current().is_none());
        assert!(!q.has_next());
        assert_eq!(q.advance(), None);
        assert_eq!(q.previous(Duration::ZERO), Previous::Restart);
        assert_eq!(q.track_ended(), Ended::Stop);
        assert!(!q.is_current("v0"));
    }

    #[test]
    fn replace_plays_the_clicked_result_in_context() {
        let mut q = Queue::default();
        assert_eq!(id(q.replace(tracks(5), 2)), Some("v2".into()));
        assert!(q.is_current("v2"));
        assert_eq!(q.peek_next().map(|t| t.video_id.as_str()), Some("v3"));
        // Out of range: nothing changes.
        assert_eq!(q.replace(tracks(2), 7), None);
        assert!(q.is_current("v2"));
    }

    #[test]
    fn next_walks_forward_and_stops_at_the_end() {
        let mut q = Queue::default();
        q.replace(tracks(3), 1);
        assert_eq!(id(q.advance()), Some("v2".into()));
        assert!(!q.has_next());
        assert_eq!(q.advance(), None);
        assert_eq!(q.track_ended(), Ended::Stop);
        assert!(q.is_current("v2"));
    }

    #[test]
    fn previous_restarts_after_three_seconds() {
        let mut q = Queue::default();
        q.replace(tracks(3), 2);
        assert_eq!(q.previous(Duration::from_millis(3_001)), Previous::Restart);
        assert!(q.is_current("v2"));
        // Exactly at the threshold still goes back.
        assert_eq!(q.previous(RESTART_THRESHOLD), Previous::Play(track("v1")));
        assert_eq!(
            q.previous(Duration::from_secs(1)),
            Previous::Play(track("v0"))
        );
        // The first track can only restart.
        assert_eq!(q.previous(Duration::ZERO), Previous::Restart);
        assert!(q.is_current("v0"));
    }

    #[test]
    fn auto_advance_plays_the_next_track() {
        let mut q = Queue::default();
        q.replace(tracks(2), 0);
        assert_eq!(q.track_ended(), Ended::Play(track("v1")));
        assert_eq!(q.track_ended(), Ended::Stop);
    }

    #[test]
    fn a_new_search_does_not_disturb_the_queue_until_a_click() {
        let mut q = Queue::default();
        q.replace(tracks(4), 0);
        // The results list is replaced elsewhere; the queue keeps its own copy.
        assert_eq!(id(q.advance()), Some("v1".into()));
    }

    // ---- repeat ----

    #[test]
    fn repeat_all_wraps_both_ways() {
        let mut q = Queue::default();
        q.set_repeat(Repeat::All);
        q.replace(tracks(3), 2);
        assert!(q.has_next());
        assert_eq!(q.peek_next().map(|t| t.video_id.as_str()), Some("v0"));
        assert_eq!(id(q.advance()), Some("v0".into()));
        assert_eq!(q.previous(Duration::ZERO), Previous::Play(track("v2")));
        // A single track with repeat-all "advances" to itself...
        q.replace(tracks(1), 0);
        assert_eq!(q.track_ended(), Ended::Play(track("v0")));
        // ...but "previous" just restarts it.
        assert_eq!(q.previous(Duration::ZERO), Previous::Restart);
    }

    #[test]
    fn repeat_one_restarts_on_end_but_next_still_skips() {
        let mut q = Queue::default();
        q.set_repeat(Repeat::One);
        q.replace(tracks(3), 0);
        assert_eq!(q.track_ended(), Ended::Restart);
        assert!(q.is_current("v0"));
        assert_eq!(id(q.advance()), Some("v1".into()));
        assert_eq!(Repeat::Off.next(), Repeat::All);
        assert_eq!(Repeat::All.next(), Repeat::One);
        assert_eq!(Repeat::One.next(), Repeat::Off);
        assert_eq!(Repeat::parse("one"), Some(Repeat::One));
    }

    // ---- editing ----

    #[test]
    fn play_next_and_add_to_queue() {
        let mut q = Queue::default();
        // On an empty queue both start playback.
        assert_eq!(id(q.add(vec![track("a")])), Some("a".into()));
        assert_eq!(q.play_next(vec![track("b"), track("c")]), None);
        assert_eq!(q.add(vec![track("z")]), None);
        assert_eq!(order(&q), ["a", "b", "c", "z"]);
        assert_eq!(q.play_next(vec![track("n")]), None);
        assert_eq!(order(&q), ["a", "n", "b", "c", "z"]);
        assert_eq!(current(&q), "a");
        assert_eq!(id(q.advance()), Some("n".into()));
        assert_eq!(q.play_next(vec![]), None);

        let mut q = Queue::default();
        assert_eq!(id(q.play_next(vec![track("x")])), Some("x".into()));
        assert_eq!(q.play_now(track("y")), track("y"));
        assert_eq!(order(&q), ["x", "y"]);
        assert_eq!(current(&q), "y");
    }

    #[test]
    fn remove_move_and_clear() {
        let mut q = Queue::default();
        q.replace(tracks(5), 2);
        // The current track can't be removed.
        assert!(!q.remove(uid(&q, "v2")));
        assert!(q.remove(uid(&q, "v0")));
        assert_eq!(order(&q), ["v1", "v2", "v3", "v4"]);
        assert_eq!(current(&q), "v2");
        assert!(!q.remove(999));

        // Moving items around keeps the current track current.
        assert!(q.move_item(3, 0));
        assert_eq!(order(&q), ["v4", "v1", "v2", "v3"]);
        assert_eq!(q.index(), Some(2));
        assert!(q.nudge(uid(&q, "v3"), -1));
        assert_eq!(order(&q), ["v4", "v1", "v3", "v2"]);
        assert_eq!(current(&q), "v2");
        assert!(!q.nudge(uid(&q, "v4"), -1));
        assert!(!q.move_item(0, 9));
        assert!(q.upcoming().is_empty(), "v2 is last now");
        assert!(q.move_to(uid(&q, "v2"), uid(&q, "v4")));
        assert_eq!(order(&q), ["v2", "v4", "v1", "v3"]);
        assert_eq!(q.upcoming().len(), 3);

        q.clear();
        assert_eq!(order(&q), ["v2"]);
        assert_eq!(current(&q), "v2");
        assert!(!q.has_next());
    }

    // ---- shuffle ----

    #[test]
    fn shuffle_keeps_current_first_and_restores_order() {
        let mut q = Queue::with_seed(42);
        q.replace(tracks(10), 4);
        q.set_shuffle(true);
        assert!(q.shuffle());
        assert_eq!(current(&q), "v4");
        assert_eq!(q.index(), Some(0));
        let shuffled = order(&q);
        assert_ne!(shuffled, order_of(&tracks(10)));
        let mut sorted = shuffled.clone();
        sorted.sort();
        assert_eq!(sorted, order_of(&tracks(10)), "same tracks, other order");

        // Additions while shuffled land in the right place once unshuffled.
        q.play_next(vec![track("next")]);
        q.add(vec![track("end")]);
        assert_eq!(order(&q)[1], "next");
        assert_eq!(
            order(&q)[2],
            "end",
            "your queue plays before the shuffled rest"
        );
        q.advance();
        q.set_shuffle(false);
        // "end" is still in your queue, so it stays right after the current track.
        assert_eq!(
            order(&q),
            [
                "v0", "v1", "v2", "v3", "v4", "next", "end", "v5", "v6", "v7", "v8", "v9"
            ]
        );
        assert_eq!(current(&q), "next");
    }

    #[test]
    fn replace_while_shuffled_starts_with_the_clicked_track() {
        let mut q = Queue::with_seed(7);
        q.set_shuffle(true);
        assert_eq!(id(q.replace(tracks(6), 3)), Some("v3".into()));
        assert_eq!(q.index(), Some(0));
        q.set_shuffle(false);
        assert_eq!(order(&q), order_of(&tracks(6)));
        assert_eq!(current(&q), "v3");
        assert_eq!(q.index(), Some(3));
    }

    // ---- history ----

    #[test]
    fn history_records_tracks_that_were_left() {
        let mut q = Queue::default();
        q.replace(tracks(3), 0);
        q.advance();
        q.advance();
        q.previous(Duration::ZERO);
        let history: Vec<_> = q.history().map(|t| t.video_id.as_str()).collect();
        assert_eq!(history, ["v2", "v1", "v0"]);
        // Replacing the queue records the old current track too.
        q.replace(tracks(1), 0);
        assert_eq!(q.history().next().map(|t| t.video_id.as_str()), Some("v1"));
        // A restart is not a new play.
        q.previous(Duration::from_secs(10));
        assert_eq!(q.history().count(), 4);
    }

    #[test]
    fn jump_plays_a_queue_item() {
        let mut q = Queue::default();
        q.replace(tracks(4), 0);
        let uid = q.items()[3].uid;
        assert_eq!(id(q.jump(uid)), Some("v3".into()));
        assert_eq!(q.jump(12345), None);
        assert!(q.upcoming().is_empty());
    }

    #[test]
    fn snapshot_and_restore() {
        let mut q = Queue::with_seed(7);
        q.replace(tracks(5), 2);
        let snap = q.snapshot();
        assert_eq!(
            (snap.tracks.len(), snap.index, snap.order.clone()),
            (5, 2, None)
        );
        let mut r = Queue::with_seed(1);
        r.restore(snap);
        assert_eq!(r.current().unwrap().video_id, "v2");
        assert!(!r.shuffle());

        q.set_shuffle(true);
        let play_order: Vec<String> = q.items().iter().map(|i| i.track.video_id.clone()).collect();
        let snap = q.snapshot();
        assert_eq!(snap.tracks[snap.index].video_id, "v2");
        let mut r = Queue::with_seed(1);
        r.restore(snap.clone());
        assert!(r.shuffle());
        let restored: Vec<String> = r.items().iter().map(|i| i.track.video_id.clone()).collect();
        assert_eq!(restored, play_order);
        assert_eq!(r.current().unwrap().video_id, "v2");
        // Turning shuffle off brings the original order back.
        r.set_shuffle(false);
        assert_eq!(r.items()[0].track.video_id, "v0");
        assert_eq!(r.current().unwrap().video_id, "v2");
        // A broken order is ignored.
        let mut r = Queue::with_seed(1);
        r.restore(Snapshot {
            index: 1,
            order: Some(vec![0, 0, 1, 2, 3]),
            ..snap
        });
        assert!(!r.shuffle());
        assert_eq!(r.current().unwrap().video_id, "v1");
    }

    // ---- your queue vs the context ----

    fn parts(q: &Queue) -> (Vec<String>, Vec<String>) {
        let ids = |items: &[QueueItem]| -> Vec<String> {
            items.iter().map(|i| i.track.video_id.clone()).collect()
        };
        (ids(q.user_upcoming()), ids(q.context_upcoming()))
    }

    #[test]
    fn your_items_play_before_the_context() {
        let mut q = Queue::default();
        q.play_context(tracks(4), 0, Some(QueueContext::list("Album")));
        assert_eq!(q.context().unwrap().name, "Album");
        q.add(vec![track("a")]);
        q.play_next(vec![track("n")]);
        q.add(vec![track("b")]);
        // "Play next" goes first, "Add to queue" to the end of your part, both before
        // the album's remaining songs.
        assert_eq!(
            parts(&q),
            (
                vec!["n".into(), "a".into(), "b".into()],
                vec!["v1".into(), "v2".into(), "v3".into()]
            )
        );
        assert_eq!(order(&q), ["v0", "n", "a", "b", "v1", "v2", "v3"]);
        // Played in that order; a played item of yours leaves your part.
        assert_eq!(id(q.advance()), Some("n".into()));
        assert_eq!(id(q.advance()), Some("a".into()));
        assert_eq!(parts(&q).0, ["b"]);
        assert_eq!(id(q.advance()), Some("b".into()));
        assert_eq!(id(q.advance()), Some("v1".into()));
        assert!(q.user_upcoming().is_empty());
        // Going back never splits the parts.
        q.previous(Duration::ZERO);
        q.previous(Duration::ZERO);
        assert_eq!(current(&q), "a");
        assert!(q.user_upcoming().is_empty(), "played items are history now");
        assert_eq!(parts(&q).1, ["b", "v1", "v2", "v3"]);
    }

    #[test]
    fn radio_and_autoplay_append_to_the_context() {
        let mut q = Queue::default();
        q.play_context(tracks(2), 0, None);
        q.add(vec![track("mine")]);
        q.append_context(vec![track("r1"), track("r2")]);
        assert_eq!(
            parts(&q),
            (
                vec!["mine".into()],
                vec!["v1".into(), "r1".into(), "r2".into()]
            )
        );
        // On an empty queue it starts playing.
        let mut q = Queue::default();
        assert_eq!(id(q.append_context(vec![track("r")])), Some("r".into()));
        assert!(q.upcoming().is_empty());
    }

    #[test]
    fn a_new_list_keeps_your_pending_items() {
        let mut q = Queue::default();
        q.play_context(tracks(3), 0, Some(QueueContext::list("One")));
        q.add(vec![track("a"), track("b")]);
        q.play_context(
            vec![track("x"), track("y"), track("z")],
            1,
            Some(QueueContext::radio("Two")),
        );
        assert_eq!(current(&q), "y");
        assert_eq!(parts(&q), (vec!["a".into(), "b".into()], vec!["z".into()]));
        assert!(q.context().unwrap().radio);
        // `replace` (Together mirrors the host's queue) drops them.
        q.replace(tracks(2), 0);
        assert_eq!(parts(&q), (vec![], vec!["v1".into()]));
        assert_eq!(q.context(), None);
        // While shuffled too.
        let mut q = Queue::with_seed(3);
        q.set_shuffle(true);
        q.play_context(tracks(5), 0, None);
        q.add(vec![track("a")]);
        q.play_context(tracks(6), 2, None);
        assert_eq!(parts(&q).0, ["a"]);
        q.set_shuffle(false);
        assert_eq!(order(&q), ["v0", "v1", "v2", "a", "v3", "v4", "v5"]);
    }

    #[test]
    fn clearing_one_part_keeps_the_other() {
        let mut q = Queue::default();
        q.play_context(tracks(4), 0, Some(QueueContext::list("L")));
        q.add(vec![track("a"), track("b")]);
        assert!(q.clear_part(Source::User));
        assert_eq!(
            parts(&q),
            (vec![], vec!["v1".into(), "v2".into(), "v3".into()])
        );
        assert!(!q.clear_part(Source::User));
        q.add(vec![track("c")]);
        assert!(q.clear_part(Source::Context));
        assert_eq!(parts(&q), (vec!["c".into()], vec![]));
        assert_eq!(q.context(), None);
        assert_eq!(current(&q), "v0");
    }

    #[test]
    fn moving_an_item_puts_it_in_the_part_it_lands_in() {
        let mut q = Queue::default();
        q.play_context(tracks(4), 0, None);
        q.add(vec![track("a"), track("b")]);
        // [v0] a b | v1 v2 v3: drag v2 onto a → it joins your queue.
        assert!(q.move_to(uid(&q, "v2"), uid(&q, "a")));
        assert_eq!(
            parts(&q),
            (
                vec!["v2".into(), "a".into(), "b".into()],
                vec!["v1".into(), "v3".into()]
            )
        );
        // Drag b down among the context → it leaves your queue.
        assert!(q.move_to(uid(&q, "b"), uid(&q, "v3")));
        assert_eq!(
            parts(&q),
            (
                vec!["v2".into(), "a".into()],
                vec!["v1".into(), "v3".into(), "b".into()]
            )
        );
        // Nudging the last of yours down one makes it context.
        assert!(q.nudge(uid(&q, "a"), 1));
        assert_eq!(parts(&q).0, ["v2"]);
    }

    #[test]
    fn jumping_ahead_keeps_your_queue() {
        let mut q = Queue::default();
        q.play_context(tracks(5), 0, None);
        q.add(vec![track("a"), track("b")]);
        // A context track further down: your items follow it.
        assert_eq!(id(q.jump(uid(&q, "v3"))), Some("v3".into()));
        assert_eq!(parts(&q), (vec!["a".into(), "b".into()], vec!["v4".into()]));
        // One of your items: the ones before it are skipped.
        assert_eq!(id(q.jump(uid(&q, "b"))), Some("b".into()));
        assert_eq!(parts(&q), (vec![], vec!["v4".into()]));
    }

    #[test]
    fn shuffle_keeps_your_queue_first_and_in_order() {
        let mut q = Queue::with_seed(9);
        q.play_context(tracks(10), 0, None);
        q.add(vec![track("a"), track("b"), track("c")]);
        q.set_shuffle(true);
        assert_eq!(parts(&q).0, ["a", "b", "c"]);
        assert_eq!(q.context_upcoming().len(), 9);
        q.advance();
        q.set_shuffle(false);
        assert_eq!(current(&q), "a");
        assert_eq!(parts(&q).0, ["b", "c"]);
        assert_eq!(parts(&q).1, order_of(&tracks(10))[1..]);
    }

    #[test]
    fn snapshot_keeps_the_parts_and_old_snapshots_still_load() {
        let mut q = Queue::with_seed(5);
        q.play_context(tracks(4), 1, Some(QueueContext::radio("Seed")));
        q.add(vec![track("a")]);
        q.set_shuffle(true);
        let snap = q.snapshot();
        assert_eq!(snap.user.len(), 1);
        assert_eq!(snap.tracks[snap.user[0]].video_id, "a");
        let mut r = Queue::with_seed(1);
        r.restore(snap.clone());
        assert_eq!(parts(&r), parts(&q));
        assert_eq!(r.context(), Some(&QueueContext::radio("Seed")));
        // A queue saved before there were two parts: everything is context.
        let mut r = Queue::with_seed(1);
        r.restore(Snapshot {
            user: vec![],
            context: None,
            order: None,
            ..snap.clone()
        });
        assert!(r.user_upcoming().is_empty());
        assert_eq!(r.upcoming().len(), 3);
        // Out-of-range indices are ignored; misplaced ones are put first.
        let mut r = Queue::with_seed(1);
        r.restore(Snapshot {
            tracks: tracks(4),
            index: 0,
            order: None,
            user: vec![3, 99],
            context: None,
        });
        assert_eq!(
            parts(&r),
            (vec!["v3".into()], vec!["v1".into(), "v2".into()])
        );
    }
}
