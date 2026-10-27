//! YouTube Music account: sign-in with the browser cookie (pasted, or captured by
//! the embedded login window on macOS, see `login_webview.rs`), channel / Google
//! account selection, sign-out, and syncing the account's library (liked songs,
//! saved songs, albums, subscriptions, playlists) into the local database.
//!
//! The cookie is a secret (keychain, `Secret::YouTubeCookie`); the account's
//! name, avatar, `auth_user` and selected channel live in `settings.library.account`.
//!
//! **Not verified against a real account** in this round: every request here is
//! music-core's cookie layer, whose request bodies are unit-tested but which was
//! never run signed-in. Errors are shown in the Account section.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{Context, Window};
use music_core::innertube::{AccountChannel, Auth, InnerTube, Item};
use music_core::library::{
    AlbumMeta, AlbumQuery, ArtistMeta, ArtistQuery, Library, PlaylistSort, RemotePlaylist,
    SongQuery, SongScope, now_ms,
};
use music_core::{Result, Track};

use crate::app::{MusicApp, notify};
use crate::settings::library_config::AccountConfig;
use crate::util::off_thread;

/// Continuation pages followed per list (≈ 100 items each).
const MAX_PAGES: usize = 60;

#[derive(Debug, Clone, Default, PartialEq)]
pub enum Busy {
    #[default]
    Idle,
    Working(String),
    Failed(String),
    Done(String),
}

pub struct Account {
    pub config: Option<AccountConfig>,
    cookie: Option<String>,
    pub channels: Vec<AccountChannel>,
    /// Sign-in / account refresh.
    pub status: Busy,
    /// Library sync.
    pub sync: Busy,
}

impl Account {
    pub fn new(config: Option<AccountConfig>) -> Self {
        Self {
            config,
            cookie: None,
            channels: Vec::new(),
            status: Busy::Idle,
            sync: Busy::Idle,
        }
    }

    /// Returns whether the cookie changed.
    pub fn set_cookie(&mut self, cookie: Option<String>) -> bool {
        if self.cookie == cookie {
            return false;
        }
        self.cookie = cookie;
        if self.cookie.is_none() {
            self.channels.clear();
        }
        true
    }

    pub fn is_signed_in(&self) -> bool {
        self.config.is_some() && self.auth().is_some()
    }

    pub fn auth(&self) -> Option<Auth> {
        let cookie = self.cookie.as_ref()?;
        let config = self.config.as_ref();
        let auth = Auth {
            cookie: cookie.clone(),
            visitor_data: None,
            data_sync_id: config.and_then(|c| c.data_sync_id.clone()),
            auth_user: config.map_or(0, |c| c.auth_user),
        };
        auth.is_signed_in().then_some(auth)
    }

    pub fn is_syncing(&self) -> bool {
        matches!(self.sync, Busy::Working(_))
    }
}

/// Cleans up what users paste: a `cookie:` prefix, quotes, line breaks.
pub fn normalize_cookie(raw: &str) -> String {
    let mut text = raw.trim();
    for prefix in ["cookie:", "Cookie:", "COOKIE:"] {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest.trim();
        }
    }
    let text = text.trim_matches(|c| c == '"' || c == '\'');
    text.split(['\n', '\r'])
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Account name, avatar, channels, data sync id for `auth`. Blocking.
fn fetch_account(auth: Auth) -> Result<(AccountConfig, Vec<AccountChannel>)> {
    let it = InnerTube::default().with_auth(auth.clone());
    let info = it.account_info()?;
    let channels = it.account_channels().unwrap_or_default();
    let config = AccountConfig {
        name: info.name,
        email: info.email,
        handle: info.handle,
        thumbnail_url: info.thumbnail_url,
        auth_user: auth.auth_user,
        data_sync_id: auth.data_sync_id,
        last_sync_at: None,
    };
    Ok((config, channels))
}

impl MusicApp {
    /// Signs in with a `Cookie` header from a signed-in music.youtube.com tab.
    pub fn sign_in_with_cookie(
        &mut self,
        cookie: String,
        auth_user: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cookie = normalize_cookie(&cookie);
        let auth = Auth {
            auth_user,
            ..Auth::new(cookie.clone())
        };
        if !auth.is_signed_in() {
            self.store.account.status = Busy::Failed(
                "That cookie has no SAPISID / __Secure-3PAPISID entry; copy the whole \
                 cookie header of a signed-in music.youtube.com request."
                    .into(),
            );
            cx.notify();
            return;
        }
        self.store.account.status = Busy::Working("Checking the account…".into());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = off_thread(move || fetch_account(auth)).await;
            this.update_in(cx, |this, window, cx| match result {
                Ok((config, channels)) => {
                    let name = config.name.clone();
                    this.store.account.status = Busy::Idle;
                    this.store.account.channels = channels;
                    this.store.account.config = Some(config.clone());
                    // Saves the cookie in the keychain; `SettingsEvent::Library` follows.
                    this.settings.update(cx, |panel, cx| {
                        panel.update_library(|l| l.account = Some(config), cx);
                        panel.set_youtube_cookie(Some(cookie.clone()), cx);
                    });
                    this.store.account.set_cookie(Some(cookie));
                    notify(window, cx, format!("Signed in as {name}"));
                    this.sync_youtube_library(window, cx);
                }
                Err(e) => {
                    this.store.account.status = Busy::Failed(format!("Sign-in failed: {e}"));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Forgets the cookie and the account (the synced library stays).
    pub fn sign_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.store.account.config = None;
        self.store.account.set_cookie(None);
        self.store.account.status = Busy::Idle;
        self.store.account.sync = Busy::Idle;
        self.settings.update(cx, |panel, cx| {
            panel.update_library(|l| l.account = None, cx);
            panel.set_youtube_cookie(None, cx);
        });
        notify(window, cx, "Signed out of YouTube Music");
        cx.notify();
    }

    /// The cookie arrived from the keychain at startup: refresh name, avatar, channels.
    pub(crate) fn account_after_cookie(&mut self, cx: &mut Context<Self>) {
        let Some(auth) = self.store.account.auth() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = off_thread(move || fetch_account(auth)).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok((mut config, channels)) => {
                        this.store.account.channels = channels;
                        if let Some(old) = &this.store.account.config {
                            config.last_sync_at = old.last_sync_at;
                        }
                        if this.store.account.config.as_ref() != Some(&config) {
                            this.store.account.config = Some(config.clone());
                            this.settings.update(cx, |panel, cx| {
                                panel.update_library(|l| l.account = Some(config), cx)
                            });
                        }
                        this.store.account.status = Busy::Idle;
                    }
                    Err(e) => {
                        this.store.account.status =
                            Busy::Failed(format!("Couldn't reach the account: {e}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Acts as another channel (brand account) and/or Google account of the cookie.
    pub fn switch_account(
        &mut self,
        data_sync_id: Option<String>,
        auth_user: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mut config) = self.store.account.config.clone() else {
            return;
        };
        config.data_sync_id = data_sync_id;
        config.auth_user = auth_user;
        config.last_sync_at = None;
        self.store.account.config = Some(config.clone());
        self.settings.update(cx, |panel, cx| {
            panel.update_library(|l| l.account = Some(config), cx)
        });
        self.store.account.status = Busy::Working("Switching…".into());
        let Some(auth) = self.store.account.auth() else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = off_thread(move || fetch_account(auth)).await;
            this.update_in(cx, |this, window, cx| match result {
                Ok((config, channels)) => {
                    let name = config.name.clone();
                    this.store.account.channels = channels;
                    this.store.account.config = Some(config.clone());
                    this.store.account.status = Busy::Idle;
                    this.settings.update(cx, |panel, cx| {
                        panel.update_library(|l| l.account = Some(config), cx)
                    });
                    notify(window, cx, format!("Now using {name}"));
                    this.sync_youtube_library(window, cx);
                }
                Err(e) => {
                    this.store.account.status = Busy::Failed(format!("Switching failed: {e}"));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Copies the account's library into the local database.
    pub fn sync_youtube_library(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.store.account.is_syncing() {
            return;
        }
        let (Some(auth), Some(library)) = (self.store.account.auth(), self.store.library.clone())
        else {
            notify(window, cx, "Sign in to YouTube Music first.");
            return;
        };
        let progress = Arc::new(Mutex::new(String::from("Starting…")));
        self.store.account.sync = Busy::Working("Starting…".into());
        let shared = progress.clone();
        let executor = cx.background_executor().clone();
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(300)).await;
                let step = progress.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let running = this
                    .update(cx, |this, cx| {
                        if this.store.account.is_syncing() {
                            this.store.account.sync = Busy::Working(step);
                            cx.notify();
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let report = off_thread(move || {
                let it = InnerTube::default().with_auth(auth);
                sync_library(&library, &it, &|step| {
                    *shared.lock().unwrap_or_else(|e| e.into_inner()) = step.to_owned();
                })
            })
            .await;
            drop(ticker);
            this.update_in(cx, |this, window, cx| {
                let summary = report.summary();
                this.store.account.sync = if report.errors.is_empty() {
                    Busy::Done(summary.clone())
                } else {
                    Busy::Failed(format!("{summary}. Errors: {}", report.errors.join("; ")))
                };
                if report.errors.len() < 5 {
                    let now = now_ms();
                    if let Some(config) = &mut this.store.account.config {
                        config.last_sync_at = Some(now);
                    }
                    this.settings.update(cx, |panel, cx| {
                        panel.update_library(
                            |l| {
                                if let Some(a) = &mut l.account {
                                    a.last_sync_at = Some(now)
                                }
                            },
                            cx,
                        )
                    });
                }
                // Liked flags for the hearts.
                this.reload_liked_flags();
                notify(window, cx, format!("YouTube Music sync: {summary}"));
                this.library_changed(cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn reload_liked_flags(&mut self) {
        if let Some(lib) = &self.store.library
            && let Ok(songs) = lib.songs(&SongQuery {
                scope: SongScope::Liked,
                ..Default::default()
            })
        {
            crate::store::replace_liked(songs.into_iter().map(|s| s.track.video_id).collect());
        }
    }

    /// Subscribes to / unsubscribes from an artist (YouTube when signed in, and the
    /// local bookmark either way).
    pub fn set_subscribed(
        &mut self,
        artist: ArtistMeta,
        subscribed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let remote = self.innertube();
        self.with_library(
            "Subscribing",
            move |lib| {
                lib.upsert_artist(&artist)?;
                lib.set_artist_bookmarked(&artist.id, subscribed)?;
                if remote.is_signed_in() && artist.id.starts_with("UC") {
                    let ids = [artist.id.as_str()];
                    if subscribed {
                        remote.subscribe(&ids)?;
                    } else {
                        remote.unsubscribe(&ids)?;
                    }
                }
                Ok(artist.name)
            },
            move |_, name, window, cx| {
                let message = if subscribed {
                    format!("Subscribed to {name}")
                } else {
                    format!("Unsubscribed from {name}")
                };
                notify(window, cx, message);
            },
            window,
            cx,
        );
    }
}

#[derive(Default)]
struct SyncReport {
    liked: usize,
    songs: usize,
    albums: usize,
    artists: usize,
    playlists: usize,
    errors: Vec<String>,
}

impl SyncReport {
    fn summary(&self) -> String {
        format!(
            "{} liked songs, {} library songs, {} albums, {} artists, {} playlists",
            self.liked, self.songs, self.albums, self.artists, self.playlists
        )
    }
}

/// All items of a library tab, following continuations.
fn all_items(
    it: &InnerTube,
    first: impl FnOnce() -> Result<music_core::innertube::LibraryPage>,
) -> Result<Vec<Item>> {
    let mut page = first()?;
    let mut items = std::mem::take(&mut page.items);
    let mut pages = 1;
    while let Some(token) = page.continuation.take()
        && pages < MAX_PAGES
    {
        page = it.library_continuation(&token)?;
        items.append(&mut page.items);
        pages += 1;
    }
    Ok(items)
}

/// A playlist's rows: track + `setVideoId`.
type Rows = Vec<(Track, Option<String>)>;

/// Playlist rows with their `setVideoId`s, following continuations.
fn playlist_rows(it: &InnerTube, playlist_id: &str) -> Result<(bool, Option<String>, Rows)> {
    let mut page = it.playlist(playlist_id)?;
    let mut rows: Rows = page
        .tracks
        .drain(..)
        .map(|e| (e.track, e.set_video_id))
        .collect();
    let mut continuation = page.continuation.take();
    let mut pages = 1;
    while let Some(token) = continuation
        && pages < MAX_PAGES
    {
        let more = it.playlist_continuation(&token)?;
        rows.extend(more.tracks.into_iter().map(|e| (e.track, e.set_video_id)));
        continuation = more.continuation;
        pages += 1;
    }
    Ok((page.editable, page.playlist.thumbnail_url, rows))
}

/// Port of ArchiveTune's `SyncUtils`: YouTube is the source of truth for liked songs,
/// saved albums, subscriptions and playlists; local-only items (local files, local
/// playlists, artists without a channel) are left alone.
fn sync_library(lib: &Library, it: &InnerTube, progress: &dyn Fn(&str)) -> SyncReport {
    let mut report = SyncReport::default();
    let now = now_ms();
    let fail = |what: &str, e: music_core::Error, report: &mut SyncReport| {
        report.errors.push(format!("{what}: {e}"));
    };

    progress("Liked songs…");
    match it.liked_songs().and_then(|mut page| {
        let mut tracks: Vec<Track> = page.tracks.drain(..).map(|e| e.track).collect();
        let mut continuation = page.continuation.take();
        let mut pages = 1;
        while let Some(token) = continuation
            && pages < MAX_PAGES
        {
            let more = it.playlist_continuation(&token)?;
            tracks.extend(more.tracks.into_iter().map(|e| e.track));
            continuation = more.continuation;
            pages += 1;
        }
        Ok(tracks)
    }) {
        Ok(tracks) => {
            let remote: HashSet<&str> = tracks.iter().map(|t| t.video_id.as_str()).collect();
            let result = (|| {
                for (i, track) in tracks.iter().enumerate() {
                    lib.set_liked_at(track, Some(now - i as i64))?;
                }
                let local = lib.songs(&SongQuery {
                    scope: SongScope::Liked,
                    ..Default::default()
                })?;
                for song in local {
                    if !song.track.is_local() && !remote.contains(song.track.video_id.as_str()) {
                        lib.unlike(&song.track.video_id)?;
                    }
                }
                Ok::<_, music_core::Error>(())
            })();
            match result {
                Ok(()) => report.liked = tracks.len(),
                Err(e) => fail("liked songs", e, &mut report),
            }
        }
        Err(e) => fail("liked songs", e, &mut report),
    }

    progress("Library songs…");
    match all_items(it, || it.library_songs()) {
        Ok(items) => {
            let tracks: Vec<&Track> = items
                .iter()
                .filter_map(|i| match i {
                    Item::Song(t) | Item::Video(t) => Some(t),
                    _ => None,
                })
                .collect();
            let result: Result<()> = tracks
                .iter()
                .enumerate()
                .try_for_each(|(i, t)| lib.set_in_library_at(t, Some(now - i as i64)));
            match result {
                Ok(()) => report.songs = tracks.len(),
                Err(e) => fail("library songs", e, &mut report),
            }
        }
        Err(e) => fail("library songs", e, &mut report),
    }

    progress("Albums…");
    match all_items(it, || it.library_albums()) {
        Ok(items) => {
            let result = (|| {
                let mut remote = HashSet::new();
                for item in &items {
                    let Item::Album(a) = item else { continue };
                    let meta = AlbumMeta {
                        id: a.browse_id.clone(),
                        title: a.title.clone(),
                        artists: a
                            .artists
                            .iter()
                            .map(|r| r.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", "),
                        year: a.year.as_deref().and_then(|y| y.parse().ok()),
                        thumbnail_url: a.thumbnail_url.clone(),
                        playlist_id: a.playlist_id.clone(),
                        explicit: a.explicit,
                        ..Default::default()
                    };
                    lib.upsert_album(&meta, &[])?;
                    lib.set_album_bookmarked(&meta.id, true)?;
                    remote.insert(meta.id);
                }
                for album in lib.albums(&AlbumQuery {
                    bookmarked_only: true,
                    ..Default::default()
                })? {
                    let id = &album.meta.id;
                    if !id.starts_with("LB:") && !remote.contains(id) {
                        lib.set_album_bookmarked(id, false)?;
                    }
                }
                Ok::<_, music_core::Error>(remote.len())
            })();
            match result {
                Ok(n) => report.albums = n,
                Err(e) => fail("albums", e, &mut report),
            }
        }
        Err(e) => fail("albums", e, &mut report),
    }

    progress("Artists…");
    match all_items(it, || it.library_subscriptions()) {
        Ok(items) => {
            let result = (|| {
                let mut remote = HashSet::new();
                for item in &items {
                    let Item::Artist(a) = item else { continue };
                    lib.upsert_artist(&ArtistMeta {
                        id: a.browse_id.clone(),
                        name: a.name.clone(),
                        thumbnail_url: a.thumbnail_url.clone(),
                    })?;
                    lib.set_artist_bookmarked(&a.browse_id, true)?;
                    remote.insert(a.browse_id.clone());
                }
                for artist in lib.artists(&ArtistQuery {
                    bookmarked_only: true,
                    ..Default::default()
                })? {
                    if artist.channel_id().is_some() && !remote.contains(&artist.id) {
                        lib.set_artist_bookmarked(&artist.id, false)?;
                    }
                }
                Ok::<_, music_core::Error>(remote.len())
            })();
            match result {
                Ok(n) => report.artists = n,
                Err(e) => fail("artists", e, &mut report),
            }
        }
        Err(e) => fail("artists", e, &mut report),
    }

    progress("Playlists…");
    match all_items(it, || it.library_playlists()) {
        Ok(items) => {
            let mut remote = HashSet::new();
            let playlists: Vec<_> = items
                .into_iter()
                .filter_map(|i| match i {
                    // "Liked music" and "Episodes for later" are covered elsewhere.
                    Item::Playlist(p) if p.playlist_id != "LM" && p.playlist_id != "SE" => Some(p),
                    _ => None,
                })
                .collect();
            let total = playlists.len();
            for (n, p) in playlists.into_iter().enumerate() {
                progress(&format!("Playlist {}/{total}: {}", n + 1, p.title));
                remote.insert(p.playlist_id.clone());
                let result =
                    playlist_rows(it, &p.playlist_id).and_then(|(editable, thumb, rows)| {
                        let playlist = lib.upsert_remote_playlist(&RemotePlaylist {
                            browse_id: p.playlist_id.clone(),
                            name: p.title.clone(),
                            thumbnail_url: p.thumbnail_url.clone().or(thumb),
                            editable,
                            song_count: Some(rows.len() as u32),
                        })?;
                        lib.replace_playlist_entries(&playlist.id, &rows)?;
                        Ok(())
                    });
                match result {
                    Ok(()) => report.playlists += 1,
                    Err(e) => fail(&format!("playlist “{}”", p.title), e, &mut report),
                }
            }
            // Mirrors of playlists that left the account.
            if let Ok(local) = lib.playlists(PlaylistSort::Name, false) {
                for playlist in local {
                    if let Some(browse_id) = &playlist.browse_id
                        && !remote.contains(browse_id)
                        && let Err(e) = lib.delete_playlist(&playlist.id)
                    {
                        fail("removing a playlist", e, &mut report);
                    }
                }
            }
        }
        Err(e) => fail("playlists", e, &mut report),
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_paste_is_normalized() {
        assert_eq!(
            normalize_cookie("cookie: SID=a; SAPISID=b\n"),
            "SID=a; SAPISID=b"
        );
        assert_eq!(normalize_cookie("'A=1;\n B=2'"), "A=1; B=2");
        assert!(Auth::new(normalize_cookie("Cookie: SAPISID=x")).is_signed_in());
    }

    #[test]
    fn account_needs_cookie_and_config() {
        let mut a = Account::new(None);
        assert!(!a.is_signed_in());
        assert!(a.set_cookie(Some("SAPISID=x; SID=y".into())));
        assert!(!a.set_cookie(Some("SAPISID=x; SID=y".into())));
        assert!(!a.is_signed_in(), "no account config yet");
        a.config = Some(AccountConfig {
            auth_user: 2,
            data_sync_id: Some("d||u".into()),
            ..Default::default()
        });
        let auth = a.auth().unwrap();
        assert_eq!(auth.auth_user, 2);
        assert_eq!(auth.delegated_session_id(), Some("d"));
        assert!(a.is_signed_in());
        a.set_cookie(Some("SID=only".into()));
        assert!(!a.is_signed_in());
    }

    /// Syncing against an anonymous client fails cleanly for every step.
    #[test]
    fn sync_without_cookie_reports_errors() {
        let lib = Library::open_in_memory().unwrap();
        let report = sync_library(&lib, &InnerTube::default(), &|_| {});
        assert_eq!(report.errors.len(), 5, "{:?}", report.errors);
        assert_eq!(report.liked, 0);
    }
}
