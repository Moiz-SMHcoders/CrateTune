//! Radio, autoplay and "play this album / playlist / artist" — the queue
//! actions that need InnerTube first.
//!
//! * [`MusicApp::start_radio`] plays a track and fills the queue with its radio
//!   mix (`InnerTube::next(WatchTarget::radio(..))`); [`MusicApp::start_radio_target`]
//!   does the same for a playlist / album / artist radio target.
//! * While the radio is the current queue, a watcher (every 2 s) loads the next
//!   batch (`next_continuation`) when fewer than [`REFILL_AT`] tracks are left.
//! * With "Autoplay similar songs" on (Content & region settings), the queue's
//!   last track gets a radio batch appended once it starts playing.
//! * [`MusicApp::collection_action`] plays / shuffles / enqueues an album,
//!   playlist or artist from a card or menu without opening its page.

use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use gpui::{Context, Task, Window};
use music_core::innertube::{Item, WatchQueue, WatchTarget};
use music_core::{InnerTube, Track};

use crate::app::{MusicApp, notify};
use crate::browse::ContentFilter;
use crate::queue::{QueueContext, Repeat};
use crate::util::off_thread;

/// Load more radio tracks when at most this many are left after the current one.
pub const REFILL_AT: usize = 3;
const WATCH_INTERVAL: Duration = Duration::from_secs(2);

/// The queue's radio, while it is the current queue.
#[derive(Default)]
pub struct RadioState {
    pub(crate) target: Option<WatchTarget>,
    /// The queue context the radio's songs are shown under ("Next from <name> radio").
    context: Option<QueueContext>,
    continuation: Option<String>,
    /// Queue items that came from the radio (the radio is "current" while the
    /// current queue item is one of them).
    uids: HashSet<u64>,
    /// Video ids already queued (radio batches repeat tracks).
    seen: HashSet<String>,
    /// The request in flight.
    task: Option<Task<()>>,
    /// Queue item for which autoplay already ran (once per item).
    autoplay_done_for: Option<u64>,
    watch: Option<Task<()>>,
}

impl RadioState {
    pub fn is_loading(&self) -> bool {
        self.task.is_some()
    }
}

/// An album, playlist or artist acted on without opening its page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Collection {
    Album {
        browse_id: String,
        playlist_id: Option<String>,
    },
    Playlist {
        id: String,
    },
    Artist {
        id: String,
    },
}

impl Collection {
    pub fn from_item(item: &Item) -> Option<Self> {
        Some(match item {
            Item::Album(a) => Collection::Album {
                browse_id: a.browse_id.clone(),
                playlist_id: a.playlist_id.clone(),
            },
            Item::Playlist(p) => Collection::Playlist {
                id: p.playlist_id.clone(),
            },
            Item::Podcast(p) => Collection::Playlist {
                id: podcast_playlist_id(&p.browse_id),
            },
            Item::Artist(a) => Collection::Artist {
                id: a.browse_id.clone(),
            },
            _ => return None,
        })
    }
}

/// `MPSPPL…` podcast show → its `PL…` playlist.
pub fn podcast_playlist_id(browse_id: &str) -> String {
    browse_id
        .strip_prefix("MPSP")
        .unwrap_or(browse_id)
        .to_owned()
}

/// The radio mix seeded by a playlist (albums are playlists too).
pub fn playlist_radio(playlist_id: &str) -> WatchTarget {
    let id = playlist_id.strip_prefix("VL").unwrap_or(playlist_id);
    WatchTarget {
        playlist_id: Some(format!("RDAMPL{id}")),
        params: Some("wAEB".into()),
        ..WatchTarget::default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollectionAction {
    Play,
    Shuffle,
    PlayNext,
    AddToQueue,
    Radio,
}

impl MusicApp {
    /// Starts the background watcher that refills the radio and runs autoplay.
    pub(crate) fn start_radio_watch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let executor = cx.background_executor().clone();
        self.radio.watch = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                executor.timer(WATCH_INTERVAL).await;
                if this.update(cx, |this, cx| this.radio_tick(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// Forgets the radio (its refills belong to a queue that was put away).
    pub(crate) fn reset_radio(&mut self) {
        self.radio = RadioState {
            watch: self.radio.watch.take(),
            ..RadioState::default()
        };
    }

    /// Whether the current queue item came from the radio.
    pub fn radio_active(&self) -> bool {
        self.radio.target.is_some()
            && self
                .queue
                .current_item()
                .is_some_and(|i| self.radio.uids.contains(&i.uid))
    }

    /// Plays `track` and queues its radio mix after it.
    pub fn start_radio(&mut self, track: Track, window: &mut Window, cx: &mut Context<Self>) {
        if self.current_video_id() == Some(track.video_id.as_str()) {
            // Keep the song playing; only the context after it changes (your queued
            // songs stay, as when a new list starts).
            self.queue.clear_part(crate::queue::Source::Context);
        } else {
            self.play_list(Rc::new(vec![track.clone()]), 0, cx);
        }
        let uid = self.queue.current_item().map(|i| i.uid);
        let context = QueueContext::radio(track.title.clone());
        self.queue.set_context(Some(context.clone()));
        self.radio = RadioState {
            target: Some(WatchTarget::radio(&track.video_id)),
            context: Some(context),
            uids: uid.into_iter().collect(),
            seen: HashSet::from([track.video_id.clone()]),
            watch: self.radio.watch.take(),
            ..RadioState::default()
        };
        notify(
            window,
            cx,
            t!("radio.toast.starting_from", title = track.title),
        );
        self.fetch_radio(false, cx);
        cx.notify();
    }

    /// Replaces the queue with the radio mix of `target` (artist / playlist / album radio).
    pub fn start_radio_target(
        &mut self,
        target: WatchTarget,
        label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.radio = RadioState {
            target: Some(target),
            context: Some(QueueContext::radio(label)),
            watch: self.radio.watch.take(),
            ..RadioState::default()
        };
        notify(window, cx, t!("radio.toast.starting", name = label));
        self.fetch_radio(true, cx);
    }

    /// Loads the first batch (`replace`: it becomes the queue) or the next one.
    fn fetch_radio(&mut self, replace: bool, cx: &mut Context<Self>) {
        let Some(target) = self.radio.target.clone() else {
            return;
        };
        let continuation = self.radio.continuation.clone();
        let (client, filter) = (self.innertube.clone(), self.content_filter);
        let asked_at = std::time::Instant::now();
        self.radio.task = Some(cx.spawn(async move |this, cx| {
            let result = off_thread(move || match &continuation {
                Some(c) => client.next_continuation(&target, c),
                None => client.next(&target),
            })
            .await;
            this.update(cx, |this, cx| {
                this.radio.task = None;
                match result {
                    Ok(queue) => {
                        // Signed in, the response carries the seed's like (and maybe
                        // the rows'): no separate request for them.
                        this.note_queue_ratings(&queue, asked_at, cx);
                        this.on_radio_batch(queue, replace, filter, cx)
                    }
                    Err(e) => {
                        eprintln!("music-gpui: radio: {e}");
                        if replace {
                            this.radio.target = None;
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn on_radio_batch(
        &mut self,
        queue: WatchQueue,
        replace: bool,
        filter: ContentFilter,
        cx: &mut Context<Self>,
    ) {
        self.radio.continuation = queue.continuation.clone();
        let tracks: Vec<Track> = filter
            .tracks(queue.tracks)
            .into_iter()
            // Thumbs down: kept out of radio and autoplay.
            .filter(|t| !crate::store::is_disliked(&t.video_id))
            .filter(|t| self.radio.seen.insert(t.video_id.clone()))
            .collect();
        if tracks.is_empty() {
            if self.radio.continuation.is_none() {
                self.radio.target = None;
            }
            return;
        }
        let count = tracks.len();
        if crate::browse::trace_enabled() {
            eprintln!(
                "music-gpui: radio: +{count} tracks (replace={replace}, more={})",
                self.radio.continuation.is_some()
            );
        }
        if replace {
            let context = self.radio.context.clone();
            self.play_list_in(Rc::new(tracks), 0, context, cx);
            self.radio.uids = self.queue.items().iter().map(|i| i.uid).collect();
        } else {
            if let Some(track) = self.queue.append_context(tracks) {
                self.play(track, cx);
            }
            let items = self.queue.items();
            let start = items.len().saturating_sub(count);
            self.radio.uids.extend(items[start..].iter().map(|i| i.uid));
        }
    }

    /// Every [`WATCH_INTERVAL`]: refill the radio, or run autoplay on the last track.
    fn radio_tick(&mut self, cx: &mut Context<Self>) {
        if self.radio.is_loading() {
            return;
        }
        let left = self.queue.upcoming().len();
        if self.radio_active() {
            if left <= REFILL_AT && self.radio.continuation.is_some() {
                self.fetch_radio(false, cx);
            }
            return;
        }
        // Autoplay: the last track of an ordinary queue is playing.
        let Some(item) = self.queue.current_item() else {
            return;
        };
        if left > 0
            || self.queue.repeat() != Repeat::Off
            || !self.autoplay_similar
            || self.radio.autoplay_done_for == Some(item.uid)
            || item.track.is_local()
        {
            return;
        }
        let (uid, seed) = (item.uid, item.track.video_id.clone());
        let context = QueueContext::radio(item.track.title.clone());
        let seen: HashSet<String> = self
            .queue
            .items()
            .iter()
            .map(|i| i.track.video_id.clone())
            .collect();
        self.queue.set_context(Some(context.clone()));
        self.radio = RadioState {
            target: Some(WatchTarget::radio(&seed)),
            context: Some(context),
            uids: HashSet::from([uid]),
            seen,
            autoplay_done_for: Some(uid),
            watch: self.radio.watch.take(),
            ..RadioState::default()
        };
        if self.trace.is_some() {
            eprintln!("music-gpui: autoplay: appending radio for {seed}");
        }
        self.fetch_radio(false, cx);
    }

    /// Play / shuffle / enqueue / radio for an album, playlist or artist.
    pub fn collection_action(
        &mut self,
        collection: Collection,
        action: CollectionAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let client = self.innertube.clone();
        let filter = self.content_filter;
        cx.spawn_in(window, async move |this, cx| {
            let result = off_thread(move || load_collection(&client, &collection, action)).await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(Loaded::Radio(target, label)) => {
                    this.start_radio_target(target, &label, window, cx)
                }
                Ok(loaded @ (Loaded::Tracks(..) | Loaded::AlbumTracks(..))) => {
                    let (tracks, name) = match loaded {
                        Loaded::AlbumTracks(t, name) => (filter.album_tracks(t), name),
                        Loaded::Tracks(t, name) => (filter.tracks(t), name),
                        Loaded::Radio(..) => (Vec::new(), String::new()),
                    };
                    if tracks.is_empty() {
                        notify(window, cx, t!("radio.nothing_to_play"));
                        return;
                    }
                    let context = Some(QueueContext::list(name));
                    this.play_tracks_in(tracks, action, context, window, cx);
                }
                Err(e) => notify(window, cx, t!("radio.load_failed", error = e)),
            })
            .ok();
        })
        .detach();
    }

    /// Plays (or shuffles) the queue a watch target gives (e.g. an artist's
    /// "shuffle" button), without the radio's endless refills.
    pub fn play_watch_target(
        &mut self,
        target: WatchTarget,
        action: CollectionAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let client = self.innertube.clone();
        let filter = self.content_filter;
        cx.spawn_in(window, async move |this, cx| {
            let result = off_thread(move || client.next(&target)).await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(queue) if !queue.tracks.is_empty() => {
                    this.play_tracks(filter.tracks(queue.tracks), action, window, cx)
                }
                Ok(_) => notify(window, cx, t!("radio.nothing_to_play")),
                Err(e) => notify(window, cx, t!("radio.load_failed", error = e)),
            })
            .ok();
        })
        .detach();
    }

    /// Applies a [`CollectionAction`] (except `Radio`) to loaded tracks of the
    /// current page.
    pub fn play_tracks(
        &mut self,
        tracks: Vec<Track>,
        action: CollectionAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.play_tracks_in(tracks, action, None, window, cx);
    }

    /// [`Self::play_tracks`] with the queue context's name given (`None`: the page's).
    pub fn play_tracks_in(
        &mut self,
        tracks: Vec<Track>,
        action: CollectionAction,
        context: Option<QueueContext>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if tracks.is_empty() {
            return;
        }
        let play = |this: &mut Self, tracks: Vec<Track>, start: usize, cx: &mut Context<Self>| {
            match context {
                Some(context) => this.play_list_in(Rc::new(tracks), start, Some(context), cx),
                None => this.play_list(Rc::new(tracks), start, cx),
            }
        };
        match action {
            CollectionAction::Play | CollectionAction::Radio => play(self, tracks, 0, cx),
            CollectionAction::Shuffle => {
                let start = random_index(tracks.len());
                if !self.queue.shuffle() {
                    self.set_shuffle(true, cx);
                }
                play(self, tracks, start, cx);
            }
            CollectionAction::PlayNext => self.queue_play_next(tracks, window, cx),
            CollectionAction::AddToQueue => self.queue_add(tracks, window, cx),
        }
        cx.notify();
    }
}

enum Loaded {
    /// Tracks and the name of what they are (the queue's "Next from …").
    Tracks(Vec<Track>, String),
    /// An album's tracks (filtered with `ContentFilter::album_tracks`) and its title.
    AlbumTracks(Vec<Track>, String),
    Radio(WatchTarget, String),
}

/// Extra playlist pages (~100 rows each) followed for play / shuffle / enqueue.
const PLAYLIST_PAGES: usize = 10;

/// Blocking: fetches what `action` needs for `collection`.
fn load_collection(
    client: &InnerTube,
    collection: &Collection,
    action: CollectionAction,
) -> music_core::Result<Loaded> {
    let no_radio = || music_core::Error::Parse("no radio for this item".into()); // i18n-ignore
    match (collection, action) {
        (Collection::Album { browse_id, .. }, CollectionAction::Radio) => {
            let album = client.album(browse_id)?;
            let id = album.album.playlist_id.ok_or_else(no_radio)?;
            Ok(Loaded::Radio(playlist_radio(&id), album.album.title))
        }
        (Collection::Album { browse_id, .. }, _) => {
            let page = client.album_complete(browse_id)?;
            Ok(Loaded::AlbumTracks(page.tracks, page.album.title))
        }
        (Collection::Playlist { id }, CollectionAction::Radio) => Ok(Loaded::Radio(
            playlist_radio(id),
            t!("radio.playlist_label").into(),
        )),
        (Collection::Playlist { id }, _) => {
            let page = client.playlist_complete(id, PLAYLIST_PAGES)?;
            Ok(Loaded::Tracks(
                page.tracks.into_iter().map(|e| e.track).collect(),
                page.playlist.title,
            ))
        }
        (Collection::Artist { id }, action) => {
            let artist = client.artist(id)?;
            let name = artist.artist.name.clone();
            match action {
                CollectionAction::Radio => {
                    Ok(Loaded::Radio(artist.radio.ok_or_else(no_radio)?, name))
                }
                CollectionAction::Shuffle if artist.shuffle.is_some() => {
                    let target = artist.shuffle.unwrap_or_default();
                    Ok(Loaded::Tracks(client.next(&target)?.tracks, name))
                }
                _ => Ok(Loaded::Tracks(
                    artist
                        .shelves
                        .into_iter()
                        .flat_map(|s| s.items)
                        .filter_map(|i| match i {
                            Item::Song(t) => Some(t),
                            _ => None,
                        })
                        .collect(),
                    name,
                )),
            }
        }
    }
}

/// A start index for "shuffle" (time-seeded; good enough for picking a song).
fn random_index(len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos() as usize);
    nanos % len
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radio_targets() {
        let t = playlist_radio("VLOLAK5uy_abc");
        assert_eq!(t.playlist_id.as_deref(), Some("RDAMPLOLAK5uy_abc"));
        assert_eq!(podcast_playlist_id("MPSPPLxyz"), "PLxyz");
        assert_eq!(podcast_playlist_id("PLxyz"), "PLxyz");
        assert!(random_index(5) < 5);
        assert_eq!(random_index(0), 0);
    }
}
