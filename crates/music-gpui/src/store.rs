//! The app's local data services: the SQLite [`Library`] and the
//! [`DownloadManager`], opened at startup next to `settings.json`, plus what
//! the UI derives from them (liked ids, download flags) and the glue the rest
//! of the app calls: likes, playlists, downloads, play recording, the saved
//! queue, search history and the local-file scan.
//!
//! Every database call is blocking but short; the ones triggered by the UI run
//! on worker threads ([`crate::util::off_thread`]) and bump
//! [`Store::revision`] when they changed something, which pages compare in
//! their render to know when to reload.
//!
//! Track rows build their context menu without access to the app, so the bits
//! they need (liked / downloaded) are mirrored in a UI-thread-local
//! ([`is_liked`], [`download_flag`]).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context, Entity, Window};
use music_core::downloads::{DownloadKind, DownloadManager, DownloadStatus};
use music_core::innertube::{Auth, InnerTube};
use music_core::library::{Library, PlaylistSort, SavedQueue, SongQuery, SongScope, now_ms};
use music_core::local_files::{ScanConfig, scan_with_progress};
use music_core::{IosClientResolver, PlaybackState, PlaybackStatus, Track};

use crate::account::Account;
use crate::app::{MusicApp, notify};
use crate::queue::Repeat;
use crate::settings::library_config::LibraryConfig;
use crate::util::off_thread;

/// A play counts for the history after this much listening (ArchiveTune's default).
pub const HISTORY_THRESHOLD: Duration = Duration::from_secs(30);
/// The queue position is saved at most this often while playing.
const QUEUE_SAVE_INTERVAL: Duration = Duration::from_secs(10);
/// Download progress / flags are refreshed at most this often.
const DOWNLOAD_POLL: Duration = Duration::from_millis(750);

// ---- UI-thread mirror for track menus and rows --------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadFlag {
    Queued,
    Downloading,
    /// Stored; `complete == false` means only the preview could be fetched.
    Done {
        complete: bool,
    },
    Failed,
}

#[derive(Default)]
struct Flags {
    liked: HashSet<String>,
    downloads: HashMap<String, DownloadFlag>,
    library_open: bool,
}

thread_local! {
    static FLAGS: RefCell<Flags> = RefCell::default();
}

/// Whether the song is liked in the local library.
pub fn is_liked(video_id: &str) -> bool {
    FLAGS.with(|f| f.borrow().liked.contains(video_id))
}

/// The user download state of a song (`None`: not downloaded).
pub fn download_flag(video_id: &str) -> Option<DownloadFlag> {
    FLAGS.with(|f| f.borrow().downloads.get(video_id).copied())
}

/// Whether the library database is available (menus disable entries without it).
pub fn library_open() -> bool {
    FLAGS.with(|f| f.borrow().library_open)
}

fn set_liked_flag(video_id: &str, liked: bool) {
    FLAGS.with(|f| {
        let mut f = f.borrow_mut();
        if liked {
            f.liked.insert(video_id.to_owned());
        } else {
            f.liked.remove(video_id);
        }
    });
}

// ---- state --------------------------------------------------------------------------------

/// A local-folder scan.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum ScanState {
    #[default]
    Idle,
    Running {
        done: usize,
        total: usize,
    },
    Done(String),
    Failed(String),
}

/// The track being listened to, for the play history.
struct Session {
    track: Track,
    started_at: i64,
    played: Duration,
    last_tick: Instant,
    /// Already recorded (ended); a replay starts a new session.
    recorded: bool,
}

pub struct Store {
    pub library: Option<Library>,
    pub downloads: Option<Arc<DownloadManager>>,
    /// Why the library couldn't be opened.
    pub error: Option<String>,
    /// Bumped whenever library data changed; pages reload when it moves.
    pub revision: u64,
    /// Copy of `settings.library` (privacy switches, cache limit, folders, account).
    pub prefs: LibraryConfig,
    pub dir: Option<PathBuf>,
    pub scan: Rc<RefCell<ScanState>>,
    pub account: Account,
    session: Option<Session>,
    queue_sig: Option<u64>,
    queue_saved_at: Option<Instant>,
    /// Restored from the last run and not started yet: seek here once it plays.
    pub(crate) resume_at: Option<(String, Duration)>,
    downloads_polled: Option<Instant>,
    downloads_sig: u64,
}

impl Store {
    /// Opens `library.sqlite` and the download store in the settings directory
    /// (`MUSIC_CONFIG_DIR` respected). Failures leave the app usable without them.
    pub fn open(prefs: LibraryConfig) -> Self {
        let dir = crate::settings::file::config_dir();
        let mut error = None;
        let library =
            dir.as_ref()
                .and_then(|dir| match Library::open(dir.join("library.sqlite")) {
                    Ok(lib) => Some(lib),
                    Err(e) => {
                        eprintln!("music-gpui: library unavailable: {e}");
                        error = Some(format!("Library unavailable: {e}"));
                        None
                    }
                });
        if dir.is_none() {
            error = Some("No data directory; the library is not available.".into());
        }
        let downloads = dir.as_ref().and_then(|dir| {
            match DownloadManager::new(
                dir.join("media"),
                Arc::new(IosClientResolver),
                library.clone(),
            ) {
                Ok(manager) => {
                    manager.set_cache_limit(prefs.cache_limit_bytes());
                    Some(Arc::new(manager))
                }
                Err(e) => {
                    eprintln!("music-gpui: downloads unavailable: {e}");
                    None
                }
            }
        });
        let liked: HashSet<String> = library
            .as_ref()
            .and_then(|lib| {
                lib.songs(&SongQuery {
                    scope: SongScope::Liked,
                    ..Default::default()
                })
                .ok()
            })
            .map(|songs| songs.into_iter().map(|s| s.track.video_id).collect())
            .unwrap_or_default();
        FLAGS.with(|f| {
            let mut f = f.borrow_mut();
            f.liked = liked;
            f.library_open = library.is_some();
        });
        let mut store = Self {
            library,
            downloads,
            error,
            revision: 0,
            account: Account::new(prefs.account.clone()),
            prefs,
            dir,
            scan: Rc::default(),
            session: None,
            queue_sig: None,
            queue_saved_at: None,
            resume_at: None,
            downloads_polled: None,
            downloads_sig: 0,
        };
        store.refresh_download_flags(true);
        store
    }

    /// Where extracted cover art of local files goes.
    pub fn artwork_dir(&self) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join("local-artwork"))
    }

    /// Rebuilds the download flags if the job list changed (or `force`).
    fn refresh_download_flags(&mut self, force: bool) -> bool {
        let Some(downloads) = &self.downloads else {
            return false;
        };
        let states = downloads.states();
        let sig = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            for s in &states {
                s.track.video_id.hash(&mut h);
                format!("{:?}", s.status).hash(&mut h);
                s.size.hash(&mut h);
            }
            h.finish()
        };
        if !force && sig == self.downloads_sig {
            return false;
        }
        self.downloads_sig = sig;
        let mut flags: HashMap<String, DownloadFlag> = downloads
            .list(Some(DownloadKind::Download))
            .into_iter()
            .map(|r| {
                (
                    r.track.video_id,
                    DownloadFlag::Done {
                        complete: r.complete,
                    },
                )
            })
            .collect();
        for s in states.iter().filter(|s| s.kind == DownloadKind::Download) {
            let flag = match &s.status {
                DownloadStatus::Queued => Some(DownloadFlag::Queued),
                DownloadStatus::Downloading => Some(DownloadFlag::Downloading),
                DownloadStatus::Failed(_) => Some(DownloadFlag::Failed),
                DownloadStatus::Completed | DownloadStatus::Cancelled => None,
            };
            if let Some(flag) = flag
                && !matches!(
                    flags.get(&s.track.video_id),
                    Some(DownloadFlag::Done { .. })
                )
            {
                flags.insert(s.track.video_id.clone(), flag);
            }
        }
        FLAGS.with(|f| f.borrow_mut().downloads = flags);
        true
    }
}

/// Order-independent fingerprint of the queue for "did it change?".
fn queue_signature(app: &MusicApp) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for item in app.queue.items() {
        item.uid.hash(&mut h);
    }
    app.queue.index().hash(&mut h);
    app.queue.shuffle().hash(&mut h);
    format!("{:?}", app.queue.repeat()).hash(&mut h);
    h.finish()
}

fn repeat_key(repeat: Repeat) -> &'static str {
    match repeat {
        Repeat::Off => "off",
        Repeat::All => "all",
        Repeat::One => "one",
    }
}

impl MusicApp {
    pub fn library(&self) -> Option<&Library> {
        self.store.library.as_ref()
    }

    /// Something in the library changed: pages reload on their next render.
    pub fn library_changed(&mut self, cx: &mut Context<Self>) {
        self.store.revision += 1;
        cx.notify();
    }

    /// Runs `work` on a worker thread with the library; on success bumps the revision,
    /// on failure shows `what` + the error.
    pub fn with_library<T: Send + 'static>(
        &mut self,
        what: &'static str,
        work: impl FnOnce(&Library) -> music_core::Result<T> + Send + 'static,
        done: impl FnOnce(&mut MusicApp, T, &mut Window, &mut Context<MusicApp>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(library) = self.store.library.clone() else {
            notify(window, cx, "The library database is not available.");
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = off_thread(move || work(&library)).await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(value) => {
                    this.library_changed(cx);
                    done(this, value, window, cx);
                }
                Err(e) => notify(window, cx, format!("{what} failed: {e}")),
            })
            .ok();
        })
        .detach();
    }

    // ---- settings ------------------------------------------------------------------------

    /// `SettingsEvent::Library`: privacy / storage / account preferences or the cookie changed.
    pub(crate) fn apply_library_settings(&mut self, cx: &mut Context<Self>) {
        let (prefs, cookie) = {
            let panel = self.settings.read(cx);
            (
                panel.settings().library.clone(),
                panel.youtube_cookie().map(str::to_owned),
            )
        };
        if let Some(downloads) = &self.store.downloads
            && prefs.cache_limit_mb != self.store.prefs.cache_limit_mb
        {
            downloads.set_cache_limit(prefs.cache_limit_bytes());
        }
        let secrets_loaded = self.settings.read(cx).secrets_loaded();
        self.store.account.config = prefs.account.clone();
        self.store.prefs = prefs;
        if secrets_loaded {
            let changed = self.store.account.set_cookie(cookie);
            if changed && self.store.account.is_signed_in() {
                self.account_after_cookie(cx);
            }
        }
        cx.notify();
    }

    /// The signed-in account for YouTube requests (`None` = anonymous).
    pub fn youtube_auth(&self) -> Option<Auth> {
        self.store.account.auth()
    }

    /// An InnerTube client acting as the signed-in account, if any.
    pub fn innertube(&self) -> InnerTube {
        match self.youtube_auth() {
            Some(auth) => InnerTube::default().with_auth(auth),
            None => InnerTube::default(),
        }
    }

    // ---- likes ---------------------------------------------------------------------------

    /// Likes or unlikes `track` in the library and, when signed in, on YouTube Music.
    pub fn toggle_like(&mut self, track: Track, window: &mut Window, cx: &mut Context<Self>) {
        let like = !is_liked(&track.video_id);
        self.set_liked(vec![track], like, window, cx);
    }

    pub fn set_liked(
        &mut self,
        tracks: Vec<Track>,
        like: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.store.library.is_none() {
            notify(window, cx, "The library database is not available.");
            return;
        }
        for t in &tracks {
            set_liked_flag(&t.video_id, like);
        }
        let remote = self.innertube();
        let signed_in = remote.is_signed_in();
        let message = match (tracks.as_slice(), like) {
            ([one], true) => format!("Liked “{}”", one.title),
            ([one], false) => format!("Removed “{}” from liked songs", one.title),
            (many, true) => format!("Liked {} songs", many.len()),
            (many, false) => format!("Removed {} songs from liked songs", many.len()),
        };
        self.with_library(
            "Saving the like",
            move |lib| {
                let mut remote_errors = Vec::new();
                for t in &tracks {
                    if like {
                        lib.like(t)?;
                    } else {
                        lib.unlike(&t.video_id)?;
                    }
                    if signed_in && !t.is_local() {
                        let rating = if like {
                            music_core::innertube::Rating::Like
                        } else {
                            music_core::innertube::Rating::Indifferent
                        };
                        if let Err(e) = remote.rate_song(&t.video_id, rating) {
                            remote_errors.push(e.to_string());
                        }
                    }
                }
                Ok(remote_errors)
            },
            move |_, remote_errors, window, cx| {
                match remote_errors.first() {
                    Some(e) => notify(
                        window,
                        cx,
                        format!("{message} (only locally; YouTube Music: {e})"),
                    ),
                    None => notify(window, cx, message),
                };
            },
            window,
            cx,
        );
        cx.notify();
    }

    /// Removes songs from the library (which also unlikes them, as in ArchiveTune).
    pub fn remove_from_library(
        &mut self,
        tracks: Vec<Track>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for t in &tracks {
            set_liked_flag(&t.video_id, false);
        }
        let count = tracks.len();
        self.with_library(
            "Removing from the library",
            move |lib| {
                for t in &tracks {
                    lib.remove_from_library(&t.video_id)?;
                }
                Ok(())
            },
            move |_, _, window, cx| {
                let what = if count == 1 {
                    "1 song".to_owned()
                } else {
                    format!("{count} songs")
                };
                notify(window, cx, format!("Removed {what} from the library"));
            },
            window,
            cx,
        );
    }

    // ---- playlists -----------------------------------------------------------------------

    /// "Add to playlist…" from a menu: opens the playlist picker.
    pub fn add_to_playlist(&mut self, track: Track, window: &mut Window, cx: &mut Context<Self>) {
        self.add_tracks_to_playlist(vec![track], window, cx);
    }

    pub fn add_tracks_to_playlist(
        &mut self,
        tracks: Vec<Track>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(library) = &self.store.library else {
            notify(window, cx, "The library database is not available.");
            return;
        };
        let playlists = library
            .playlists(PlaylistSort::Updated, true)
            .unwrap_or_default();
        crate::components::playlist_dialog::open(
            tracks,
            playlists,
            self.store.account.is_signed_in(),
            window,
            cx,
        );
    }

    /// Appends `tracks` to playlist `id` (and to its YouTube playlist when it is an
    /// editable mirror and the account is signed in).
    pub fn append_to_playlist(
        &mut self,
        id: String,
        tracks: Vec<Track>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let remote = self.innertube();
        let count = tracks.len();
        self.with_library(
            "Adding to the playlist",
            move |lib| {
                let playlist = lib
                    .playlist(&id)?
                    .ok_or_else(|| music_core::Error::Storage("playlist not found".into()))?;
                let entries = lib.add_to_playlist(&id, &tracks)?;
                let mut remote_error = None;
                if let Some(browse_id) = playlist.browse_id.as_deref().filter(|_| playlist.editable)
                    && remote.is_signed_in()
                {
                    let ids: Vec<&str> = tracks
                        .iter()
                        .filter(|t| !t.is_local())
                        .map(|t| t.video_id.as_str())
                        .collect();
                    match remote.add_playlist_items(browse_id, &ids) {
                        Ok(set_ids) => {
                            for (entry, set_id) in entries.iter().zip(set_ids) {
                                lib.set_playlist_entry_set_video_id(*entry, Some(&set_id))?;
                            }
                        }
                        Err(e) => remote_error = Some(e.to_string()),
                    }
                }
                Ok((playlist.name, remote_error))
            },
            move |_, (name, remote_error), window, cx| {
                let what = if count == 1 {
                    "1 song".to_owned()
                } else {
                    format!("{count} songs")
                };
                let mut message = format!("Added {what} to “{name}”");
                if let Some(e) = remote_error {
                    message.push_str(&format!(" (YouTube Music: {e})"));
                }
                notify(window, cx, message);
            },
            window,
            cx,
        );
    }

    /// Creates a local playlist (optionally also on YouTube Music) and adds `tracks`.
    pub fn create_playlist(
        &mut self,
        name: String,
        tracks: Vec<Track>,
        on_youtube: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = name.trim().to_owned();
        if name.is_empty() {
            notify(window, cx, "Give the playlist a name.");
            return;
        }
        let remote = self.innertube();
        let on_youtube = on_youtube && remote.is_signed_in();
        self.with_library(
            "Creating the playlist",
            move |lib| {
                let playlist = if on_youtube {
                    let ids: Vec<&str> = tracks
                        .iter()
                        .filter(|t| !t.is_local())
                        .map(|t| t.video_id.as_str())
                        .collect();
                    let browse_id = remote.create_playlist(
                        &name,
                        None,
                        music_core::innertube::PlaylistPrivacy::Private,
                        &ids,
                    )?;
                    let playlist =
                        lib.upsert_remote_playlist(&music_core::library::RemotePlaylist {
                            browse_id,
                            name: name.clone(),
                            thumbnail_url: None,
                            editable: true,
                            song_count: Some(ids.len() as u32),
                        })?;
                    lib.add_to_playlist(&playlist.id, &tracks)?;
                    playlist
                } else {
                    let playlist = lib.create_playlist(&name)?;
                    lib.add_to_playlist(&playlist.id, &tracks)?;
                    playlist
                };
                Ok((playlist, tracks.len()))
            },
            move |_, (playlist, count), window, cx| {
                let message = if count == 0 {
                    format!("Created “{}”", playlist.name)
                } else {
                    format!("Created “{}” with {count} songs", playlist.name)
                };
                notify(window, cx, message);
            },
            window,
            cx,
        );
    }

    // ---- downloads -----------------------------------------------------------------------

    pub fn download_tracks(
        &mut self,
        tracks: Vec<Track>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(downloads) = self.store.downloads.clone() else {
            notify(window, cx, "Downloads are not available.");
            return;
        };
        let tracks: Vec<Track> = tracks.into_iter().filter(|t| !t.is_local()).collect();
        let message = match tracks.as_slice() {
            [] => return,
            [one] => format!("Downloading “{}”", one.title),
            many => format!("Downloading {} songs", many.len()),
        };
        for track in tracks {
            downloads.download(track);
        }
        self.store.refresh_download_flags(true);
        notify(window, cx, message);
        cx.notify();
    }

    /// Deletes the stored file (download or cached copy) of `video_id`.
    pub fn delete_download(
        &mut self,
        video_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(downloads) = self.store.downloads.clone() else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = off_thread(move || downloads.delete(&video_id)).await;
            this.update_in(cx, |this, window, cx| {
                if let Err(e) = result {
                    notify(window, cx, format!("Deleting the download failed: {e}"));
                }
                this.store.refresh_download_flags(true);
                this.library_changed(cx);
            })
            .ok();
        })
        .detach();
    }

    /// The stored audio file to play instead of streaming: user downloads always, cached
    /// copies only when they hold the whole track.
    pub fn local_copy(&self, track: &Track) -> Option<Track> {
        if track.is_local() {
            return None;
        }
        let downloads = self.store.downloads.as_ref()?;
        let record = downloads.record(&track.video_id)?;
        if record.kind == DownloadKind::Cache && !record.complete {
            return None;
        }
        let path = downloads.local_file(&track.video_id)?;
        Some(Track {
            local_path: Some(path),
            ..track.clone()
        })
    }

    /// Playback hook: what to hand the engine for `track` (a downloaded copy if there is
    /// one). Also queues the play cache when enabled.
    pub fn playable(&self, track: Track) -> Track {
        if let Some(local) = self.local_copy(&track) {
            return local;
        }
        if self.store.prefs.cache_played
            && !track.is_local()
            && let Some(downloads) = &self.store.downloads
        {
            downloads.cache(track.clone());
        }
        track
    }

    // ---- search history ------------------------------------------------------------------

    /// Remembers a submitted search (unless search history is paused).
    pub fn record_search(&mut self, query: &str, cx: &mut Context<Self>) {
        let query = query.trim().to_owned();
        if query.is_empty() || self.store.prefs.pause_search_history {
            return;
        }
        let Some(library) = self.store.library.clone() else {
            return;
        };
        cx.background_spawn(async move {
            if let Err(e) = library.add_search_query(&query) {
                eprintln!("music-gpui: saving the search failed: {e}");
            }
        })
        .detach();
    }

    /// Past searches starting with `prefix`, newest first (empty while paused). For the
    /// search field's suggestions (browse / search page).
    #[allow(dead_code)]
    pub fn search_history(&self, prefix: &str, limit: u32) -> Vec<String> {
        if self.store.prefs.pause_search_history {
            return Vec::new();
        }
        self.store
            .library
            .as_ref()
            .and_then(|lib| lib.search_history(prefix, limit).ok())
            .map(|entries| entries.into_iter().map(|e| e.query).collect())
            .unwrap_or_default()
    }

    // ---- polling: play history, queue, downloads -------------------------------------------

    /// Called from the 250 ms poll with the fresh engine state.
    pub(crate) fn poll_store(&mut self, state: &PlaybackState, cx: &mut Context<Self>) {
        self.track_listening(state, cx);
        self.resume_restored_position(state, cx);
        self.maybe_save_queue(state.status == PlaybackStatus::Playing, cx);
        let due = self
            .store
            .downloads_polled
            .is_none_or(|t| t.elapsed() >= DOWNLOAD_POLL);
        if due {
            self.store.downloads_polled = Some(Instant::now());
            if self.store.refresh_download_flags(false) {
                self.library_changed(cx);
            }
        }
    }

    fn track_listening(&mut self, state: &PlaybackState, cx: &mut Context<Self>) {
        let now = Instant::now();
        let current = state.track.as_ref();
        let same = matches!(
            (&self.store.session, current),
            (Some(s), Some(t)) if s.track.video_id == t.video_id
        );
        if !same {
            if let Some(session) = self.store.session.take() {
                self.finish_session(session, cx);
            }
            if let Some(track) = current {
                self.store.session = Some(Session {
                    track: track.clone(),
                    started_at: now_ms(),
                    played: Duration::ZERO,
                    last_tick: now,
                    recorded: false,
                });
            }
            return;
        }
        let Some(session) = &mut self.store.session else {
            return;
        };
        let delta = now.saturating_duration_since(session.last_tick);
        session.last_tick = now;
        match state.status {
            PlaybackStatus::Playing if session.recorded => {
                // Replayed after it ended (repeat one, "play again").
                let track = session.track.clone();
                self.store.session = Some(Session {
                    track,
                    started_at: now_ms(),
                    played: Duration::ZERO,
                    last_tick: now,
                    recorded: false,
                });
            }
            // Wall time while playing; a long gap (sleep, stalled poll) counts as one tick.
            PlaybackStatus::Playing => session.played += delta.min(Duration::from_secs(1)),
            PlaybackStatus::Ended if !session.recorded => {
                let finished = Session {
                    track: session.track.clone(),
                    started_at: session.started_at,
                    played: session.played,
                    last_tick: now,
                    recorded: false,
                };
                session.recorded = true;
                self.finish_session(finished, cx);
            }
            _ => {}
        }
    }

    fn finish_session(&mut self, session: Session, cx: &mut Context<Self>) {
        if session.recorded
            || session.played < HISTORY_THRESHOLD
            || self.store.prefs.pause_listen_history
        {
            return;
        }
        let Some(library) = self.store.library.clone() else {
            return;
        };
        if self.trace.is_some() {
            eprintln!(
                "music-gpui: history: {} played {:.1}s",
                session.track.video_id,
                session.played.as_secs_f64()
            );
        }
        // The stored track is the streamed one, not a temporary local copy.
        let mut track = session.track;
        if !track.video_id.starts_with("local:") {
            track.local_path = None;
        }
        let played = session.played.as_millis() as u64;
        let at = session.started_at;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { library.record_play(&track, played, at) })
                .await;
            this.update(cx, |this, cx| match result {
                Ok(_) => this.library_changed(cx),
                Err(e) => eprintln!("music-gpui: recording the play failed: {e}"),
            })
            .ok();
        })
        .detach();
    }

    /// Records the current listening session (quit).
    pub(crate) fn flush_listening(&mut self) {
        let Some(session) = self.store.session.take() else {
            return;
        };
        if session.recorded
            || session.played < HISTORY_THRESHOLD
            || self.store.prefs.pause_listen_history
        {
            return;
        }
        if let Some(library) = &self.store.library {
            let mut track = session.track;
            if !track.video_id.starts_with("local:") {
                track.local_path = None;
            }
            let _ = library.record_play(
                &track,
                session.played.as_millis() as u64,
                session.started_at,
            );
        }
    }

    fn saved_queue(&self) -> SavedQueue {
        let (tracks, index, order) = self.queue.snapshot();
        let position = if self.playback.track.is_some() {
            self.clock.position(Instant::now())
        } else {
            self.store
                .resume_at
                .as_ref()
                .map(|(_, p)| *p)
                .unwrap_or_default()
        };
        let mut saved = SavedQueue::new(
            tracks
                .into_iter()
                .map(|mut t| {
                    if !t.video_id.starts_with("local:") {
                        t.local_path = None;
                    }
                    t
                })
                .collect(),
            index,
            position,
        );
        saved.shuffle_order = order;
        saved.repeat_mode = Some(repeat_key(self.queue.repeat()).into());
        saved
    }

    fn maybe_save_queue(&mut self, playing: bool, cx: &mut Context<Self>) {
        let Some(library) = self.store.library.clone() else {
            return;
        };
        let sig = queue_signature(self);
        let changed = self.store.queue_sig != Some(sig);
        let stale = playing
            && self
                .store
                .queue_saved_at
                .is_none_or(|t| t.elapsed() >= QUEUE_SAVE_INTERVAL);
        if !changed && !stale {
            return;
        }
        // The very first poll only learns the restored queue's signature.
        if self.store.queue_sig.is_none() && !playing {
            self.store.queue_sig = Some(sig);
            return;
        }
        self.store.queue_sig = Some(sig);
        self.store.queue_saved_at = Some(Instant::now());
        let saved = self.saved_queue();
        cx.background_spawn(async move {
            if let Err(e) = library.save_queue(&saved) {
                eprintln!("music-gpui: saving the queue failed: {e}");
            }
        })
        .detach();
    }

    /// Saves the queue now (quit).
    pub(crate) fn save_queue_now(&mut self) {
        if let Some(library) = &self.store.library
            && let Err(e) = library.save_queue(&self.saved_queue())
        {
            eprintln!("music-gpui: saving the queue failed: {e}");
        }
    }

    /// Puts last session's queue back (paused, nothing loads until play is pressed).
    pub(crate) fn restore_queue(&mut self) {
        let Some(saved) = self
            .store
            .library
            .as_ref()
            .and_then(|lib| lib.load_queue().ok().flatten())
        else {
            return;
        };
        if saved.tracks.is_empty() {
            return;
        }
        let position = saved.position();
        let current = saved.tracks.get(saved.index).map(|t| t.video_id.clone());
        self.queue
            .restore(saved.tracks, saved.index, saved.shuffle_order);
        if let Some(repeat) = saved.repeat_mode.as_deref().and_then(Repeat::parse) {
            self.queue.set_repeat(repeat);
        }
        if let Some(id) = current {
            self.store.resume_at = Some((id, position));
        }
    }

    /// Play pressed with a restored queue and nothing loaded: start it where it was.
    /// Returns whether it did.
    pub(crate) fn start_restored(&mut self, cx: &mut Context<Self>) -> bool {
        if self.playback.track.is_some() || self.store.resume_at.is_none() {
            return false;
        }
        match self.queue.current().cloned() {
            Some(track) => {
                self.play(track, cx);
                true
            }
            None => false,
        }
    }

    fn resume_restored_position(&mut self, state: &PlaybackState, cx: &mut Context<Self>) {
        let Some((id, position)) = &self.store.resume_at else {
            return;
        };
        let Some(track) = &state.track else { return };
        if &track.video_id != id {
            if self.playback.track.is_some() {
                // Something else was played first.
                self.store.resume_at = None;
            }
            return;
        }
        if state.status != PlaybackStatus::Playing {
            return;
        }
        let position = *position;
        self.store.resume_at = None;
        let fits = state
            .duration
            .is_none_or(|d| position + Duration::from_secs(5) < d);
        if position > Duration::from_secs(3) && fits {
            self.seek_to(position, cx);
        }
    }

    // ---- local files ---------------------------------------------------------------------

    /// Scans the configured music folders into the library.
    pub fn rescan_local_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(library) = self.store.library.clone() else {
            notify(window, cx, "The library database is not available.");
            return;
        };
        if matches!(*self.store.scan.borrow(), ScanState::Running { .. }) {
            return;
        }
        let prefs = &self.store.prefs;
        if prefs.local_folders.is_empty() {
            notify(window, cx, "Add a music folder first.");
            return;
        }
        let Some(artwork_dir) = self.store.artwork_dir() else {
            return;
        };
        let mut config = ScanConfig::new(prefs.local_folders.clone(), artwork_dir);
        config.exclude = prefs.excluded_folders.clone();
        config.min_duration_secs = prefs.local_min_secs;
        *self.store.scan.borrow_mut() = ScanState::Running { done: 0, total: 0 };
        let progress = Arc::new(std::sync::Mutex::new((0usize, 0usize)));
        let shared = progress.clone();
        let scan_state = self.store.scan.clone();
        let executor = cx.background_executor().clone();
        // Progress ticker while the scan runs.
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(200)).await;
                let (done, total) = *progress.lock().unwrap_or_else(|e| e.into_inner());
                let keep = this
                    .update(cx, |_, cx| {
                        let mut state = scan_state.borrow_mut();
                        if matches!(*state, ScanState::Running { .. }) {
                            *state = ScanState::Running { done, total };
                            cx.notify();
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = off_thread(move || {
                scan_with_progress(&library, &config, &|done, total| {
                    *shared.lock().unwrap_or_else(|e| e.into_inner()) = (done, total);
                })
            })
            .await;
            drop(ticker);
            this.update_in(cx, |this, window, cx| {
                let state = match result {
                    Ok(s) => {
                        let mut line = format!(
                            "{} files: {} new, {} updated, {} unchanged, {} removed",
                            s.found, s.added, s.updated, s.unchanged, s.removed
                        );
                        if s.skipped_short > 0 {
                            line.push_str(&format!(", {} too short", s.skipped_short));
                        }
                        if !s.failed.is_empty() {
                            line.push_str(&format!(", {} unreadable", s.failed.len()));
                        }
                        notify(window, cx, format!("Scan finished: {line}"));
                        ScanState::Done(line)
                    }
                    Err(e) => {
                        notify(window, cx, format!("Scan failed: {e}"));
                        ScanState::Failed(e.to_string())
                    }
                };
                *this.store.scan.borrow_mut() = state;
                this.library_changed(cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Settings entity (for pages that change library preferences).
    pub fn settings_entity(&self) -> Entity<crate::settings::SettingsPanel> {
        self.settings.clone()
    }
}

/// Replaces the liked-id mirror (after a sync or a backup restore).
pub fn replace_liked(ids: HashSet<String>) {
    FLAGS.with(|f| f.borrow_mut().liked = ids);
}
