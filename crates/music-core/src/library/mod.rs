// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/db/{MusicDatabase,DatabaseDao,LocalMusicIdentity}.kt
// — © Rukamori and ArchiveTune contributors, GPL-3.0

//! Local library: songs, artists, albums, playlists, play history, stats,
//! search history, lyrics cache, key/value settings and the saved queue, in
//! one SQLite database (`rusqlite`, SQLite bundled).
//!
//! [`Library`] is a cheap-to-clone handle (`Send + Sync`); every call is
//! blocking and runs under one connection mutex, so call it from a worker
//! thread when the UI must not stall. Timestamps are Unix milliseconds
//! ([`now_ms`]).
//!
//! ```no_run
//! # fn main() -> music_core::Result<()> {
//! use music_core::library::{Library, SongQuery, SongScope, SongSort};
//! let library = Library::open("/tmp/library.sqlite")?;
//! # let track = music_core::Track::default();
//! library.like(&track)?;
//! library.record_play(&track, 95_000, music_core::library::now_ms())?;
//! let liked = library.songs(&SongQuery {
//!     scope: SongScope::Liked,
//!     sort: SongSort::DateAdded,
//!     descending: true,
//!     ..Default::default()
//! })?;
//! # Ok(()) }
//! ```

mod backup;
mod history;
mod home;
mod playlists;
mod ratings;
mod schema;
mod songs;
mod stats;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{Error, Result, Track};

pub use backup::{
    BACKUP_DB_ENTRY, BACKUP_MANIFEST_ENTRY, BACKUP_SETTINGS_ENTRY, BackupManifest, RestoredBackup,
};
pub use history::{CachedLyrics, HistoryDay, HistoryEntry, SearchHistoryEntry, group_by_day};
pub use home::ForgottenRule;
pub use playlists::{Playlist, PlaylistEntry, PlaylistSort, RemotePlaylist};
pub use ratings::{MAX_RATING_ATTEMPTS, PendingRating, RatingPush};
pub use schema::SCHEMA_VERSION;
pub(crate) use songs::upsert_track_tx;
pub use songs::{
    AlbumMeta, AlbumQuery, AlbumSort, ArtistMeta, ArtistQuery, ArtistSort, LibraryAlbum,
    LibraryArtist, LibrarySong, LocalSearch, ReplayGain, SongExtras, SongQuery, SongScope,
    SongSort,
};
pub use stats::{
    AlbumStat, ArtistStat, DayListening, ListeningTotals, SongStat, TimeRange, TimeZone,
    YearInMusic,
};

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Storage(e.to_string())
    }
}

/// Current time in Unix milliseconds.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

/// Case/whitespace-insensitive identity used to merge artists and albums without ids
/// (port of ArchiveTune's `LocalMusicIdentity.normalize`, minus NFKC).
pub fn normalize_name(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Id for an artist that has no YouTube channel id: `LA:` + normalized name.
pub fn synthetic_artist_id(name: &str) -> String {
    format!("LA:{}", normalize_name(name))
}

/// Id for an album that has no `browseId`: `LB:` + normalized title + album artist.
pub fn synthetic_album_id(title: &str, artist: &str) -> String {
    format!(
        "LB:{}\u{1f}{}",
        normalize_name(title),
        normalize_name(artist)
    )
}

pub(crate) fn is_synthetic_id(id: &str) -> bool {
    id.starts_with("LA:") || id.starts_with("LB:")
}

/// What the UI needs to restore playback after a restart; see [`Library::save_queue`].
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedQueue {
    pub tracks: Vec<Track>,
    /// Index into `tracks` of the current track.
    pub index: usize,
    /// Playback position inside the current track.
    pub position_ms: u64,
    /// Play order when shuffled (indices into `tracks`), opaque to the core.
    pub shuffle_order: Option<Vec<usize>>,
    /// The UI's repeat mode, opaque to the core (e.g. `"off"`, `"all"`, `"one"`).
    pub repeat_mode: Option<String>,
    /// Indices into `tracks` of the songs you queued yourself ("Play next" / "Add to
    /// queue"), which play before the rest. Absent in queues saved by older versions
    /// (everything is then the context); older versions ignore it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub user_queued: Vec<usize>,
    /// Name of what the rest of the queue comes from ("Next from …"), opaque to the core.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_name: Option<String>,
    /// Whether that is a radio / autoplay mix.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub context_radio: bool,
}

impl SavedQueue {
    pub fn new(tracks: Vec<Track>, index: usize, position: Duration) -> Self {
        Self {
            tracks,
            index,
            position_ms: position.as_millis() as u64,
            ..Default::default()
        }
    }

    pub fn position(&self) -> Duration {
        Duration::from_millis(self.position_ms)
    }
}

const QUEUE_KEY: &str = "core.saved_queue";

/// Handle to the library database. Clones share one connection.
#[derive(Clone)]
pub struct Library {
    conn: Arc<Mutex<Connection>>,
    path: Option<PathBuf>,
}

impl std::fmt::Debug for Library {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Library")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl Library {
    /// Opens (creating if needed) the database at `path` and migrates it to [`SCHEMA_VERSION`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn, Some(path.to_owned()))
    }

    /// A private in-memory database (tests, or running without persistence).
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?, None)
    }

    fn init(conn: Connection, path: Option<PathBuf>) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let mut conn = conn;
        migrate(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            path,
        })
    }

    /// File the database lives in (`None` for in-memory).
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// `PRAGMA user_version` of the open database.
    pub fn schema_version(&self) -> Result<u32> {
        self.read(|c| c.pragma_query_value(None, "user_version", |r| r.get(0)))
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn read<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        Ok(f(&self.lock())?)
    }

    /// Runs `f` in a transaction, committed when it returns `Ok`.
    pub(crate) fn write<T>(
        &self,
        f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    // ---- key/value -------------------------------------------------------------------------

    /// App-level setting stored in the library (backed up with it).
    pub fn get_value(&self, key: &str) -> Result<Option<String>> {
        self.read(|c| {
            c.query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
                .optional()
        })
    }

    pub fn set_value(&self, key: &str, value: &str) -> Result<()> {
        self.read(|c| {
            c.execute(
                "INSERT INTO kv(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
        })?;
        Ok(())
    }

    pub fn remove_value(&self, key: &str) -> Result<()> {
        self.read(|c| c.execute("DELETE FROM kv WHERE key = ?1", [key]))?;
        Ok(())
    }

    /// [`Self::get_value`] decoded from JSON; `None` if missing or not decodable.
    pub fn get_json<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        Ok(self
            .get_value(key)?
            .and_then(|v| serde_json::from_str(&v).ok()))
    }

    pub fn set_json<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let json = serde_json::to_string(value).map_err(|e| Error::Storage(e.to_string()))?;
        self.set_value(key, &json)
    }

    // ---- queue -----------------------------------------------------------------------------

    /// Persists the play queue (replacing the previous one).
    pub fn save_queue(&self, queue: &SavedQueue) -> Result<()> {
        self.set_json(QUEUE_KEY, queue)
    }

    /// The queue last passed to [`Self::save_queue`]. The index is clamped into range.
    pub fn load_queue(&self) -> Result<Option<SavedQueue>> {
        Ok(self.get_json::<SavedQueue>(QUEUE_KEY)?.map(|mut q| {
            q.index = q.index.min(q.tracks.len().saturating_sub(1));
            q
        }))
    }

    pub fn clear_queue(&self) -> Result<()> {
        self.remove_value(QUEUE_KEY)
    }
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let current: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if current > SCHEMA_VERSION {
        return Err(Error::Storage(format!(
            "library schema v{current} is newer than this build (v{SCHEMA_VERSION})"
        )));
    }
    for (version, sql) in schema::MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version as u32 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

/// `%term%` for `LIKE … ESCAPE '\'`.
pub(crate) fn like_pattern(term: &str) -> String {
    let mut out = String::with_capacity(term.len() + 2);
    out.push('%');
    for c in term.trim().chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

#[cfg(test)]
pub(crate) mod test_util {
    use std::path::PathBuf;

    use crate::{EntityRef, Track};

    /// Fresh directory under the system temp dir (callers remove it).
    pub fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "music-core-test-{}-{}-{}",
            std::process::id(),
            super::now_ms(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub fn track(id: &str, title: &str, artist: &str, album: Option<(&str, &str)>) -> Track {
        Track {
            video_id: id.into(),
            title: title.into(),
            artists: artist.into(),
            album: album.map(|a| a.1.to_owned()),
            album_id: album.map(|a| a.0.to_owned()),
            duration_secs: Some(200),
            artist_refs: vec![EntityRef {
                name: artist.into(),
                id: Some(format!("UC{}", artist.replace(' ', ""))),
            }],
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_util::{tempdir, track};

    #[test]
    fn migrates_empty_database_and_reopens() {
        let dir = tempdir();
        let path = dir.join("lib.sqlite");
        {
            let lib = Library::open(&path).unwrap();
            assert_eq!(lib.schema_version().unwrap(), SCHEMA_VERSION);
            lib.set_value("a", "1").unwrap();
        }
        let lib = Library::open(&path).unwrap();
        assert_eq!(lib.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(lib.get_value("a").unwrap().as_deref(), Some("1"));
        // Windows can't delete a database file that is still open.
        drop(lib);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refuses_newer_schema() {
        let dir = tempdir();
        let path = dir.join("lib.sqlite");
        {
            let c = Connection::open(&path).unwrap();
            c.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
                .unwrap();
        }
        assert!(matches!(Library::open(&path), Err(Error::Storage(_))));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn library_is_send_sync_and_shared_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Library>();
        let lib = Library::open_in_memory().unwrap();
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let lib = lib.clone();
                std::thread::spawn(move || {
                    lib.upsert_track(&track(&format!("v{i}"), "t", "a", None))
                        .unwrap()
                })
            })
            .collect();
        handles.into_iter().for_each(|h| h.join().unwrap());
        assert_eq!(lib.song_count(SongScope::All).unwrap(), 4);
    }

    #[test]
    fn kv_and_json() {
        let lib = Library::open_in_memory().unwrap();
        assert_eq!(lib.get_value("x").unwrap(), None);
        lib.set_value("x", "1").unwrap();
        lib.set_value("x", "2").unwrap();
        assert_eq!(lib.get_value("x").unwrap().as_deref(), Some("2"));
        lib.set_json("j", &vec![1, 2, 3]).unwrap();
        assert_eq!(lib.get_json::<Vec<i32>>("j").unwrap(), Some(vec![1, 2, 3]));
        assert_eq!(lib.get_json::<String>("j").unwrap(), None);
        lib.remove_value("x").unwrap();
        assert_eq!(lib.get_value("x").unwrap(), None);
    }

    #[test]
    fn queue_round_trip() {
        let lib = Library::open_in_memory().unwrap();
        assert_eq!(lib.load_queue().unwrap(), None);
        let mut queue = SavedQueue::new(
            vec![track("a", "A", "X", None), track("b", "B", "Y", None)],
            1,
            Duration::from_millis(12_345),
        );
        queue.shuffle_order = Some(vec![1, 0]);
        queue.repeat_mode = Some("all".into());
        lib.save_queue(&queue).unwrap();
        let loaded = lib.load_queue().unwrap().unwrap();
        assert_eq!(loaded, queue);
        assert_eq!(loaded.position(), Duration::from_millis(12_345));

        // The two queue parts and the context round-trip too.
        queue.user_queued = vec![1];
        queue.context_name = Some("Album".into());
        queue.context_radio = true;
        lib.save_queue(&queue).unwrap();
        assert_eq!(lib.load_queue().unwrap().unwrap(), queue);

        lib.save_queue(&SavedQueue::new(vec![], 5, Duration::ZERO))
            .unwrap();
        assert_eq!(lib.load_queue().unwrap().unwrap().index, 0);
        lib.clear_queue().unwrap();
        assert_eq!(lib.load_queue().unwrap(), None);
    }

    /// A queue saved before `user_queued` / `context_*` existed loads with everything as
    /// context; a new one stays readable for older versions (extra fields only).
    #[test]
    fn saved_queue_is_backward_compatible() {
        let lib = Library::open_in_memory().unwrap();
        lib.set_value(
            QUEUE_KEY,
            r#"{"tracks":[{"video_id":"a","title":"A"}],"index":0,"position_ms":5,"shuffle_order":null,"repeat_mode":"all"}"#,
        )
        .unwrap();
        let old = lib.load_queue().unwrap().unwrap();
        assert!(old.user_queued.is_empty() && old.context_name.is_none() && !old.context_radio);
        assert_eq!(old.repeat_mode.as_deref(), Some("all"));
        // Nothing new is written for a queue without the new data.
        let json = serde_json::to_string(&old).unwrap();
        assert!(
            !json.contains("user_queued") && !json.contains("context_"),
            "{json}"
        );
        // The new fields are plain additions an older reader skips.
        let mut new = old.clone();
        new.user_queued = vec![0];
        new.context_name = Some("Mix".into());
        let json = serde_json::to_value(&new).unwrap();
        assert_eq!(json["user_queued"], serde_json::json!([0]));
        assert_eq!(json["tracks"][0]["video_id"], "a");
    }

    #[test]
    fn like_pattern_escapes() {
        assert_eq!(like_pattern(" a%b_c\\ "), "%a\\%b\\_c\\\\%");
    }
}
