// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/db/DatabaseDao.kt and
// db/entities/{SongEntity,ArtistEntity,AlbumEntity}.kt — © Rukamori and ArchiveTune contributors, GPL-3.0

//! Songs, artists and albums: upserts, likes, library membership, sorted listings, local search.

use std::path::PathBuf;

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};
use serde::{Deserialize, Serialize};

use super::{
    Library, Playlist, is_synthetic_id, like_pattern, now_ms, synthetic_album_id,
    synthetic_artist_id,
};
use crate::{EntityRef, Result, Track};

/// Which songs a listing covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SongScope {
    /// Added to the library, liked, or a local file (ArchiveTune's "Songs" tab).
    #[default]
    Library,
    Liked,
    /// Downloaded by the user (not the automatic play cache).
    Downloaded,
    /// Scanned from disk ([`crate::local_files`]).
    Local,
    /// Every song the database knows, including ones only seen in history or playlists.
    All,
}

impl SongScope {
    fn predicate(self) -> &'static str {
        match self {
            SongScope::Library => {
                "(s.in_library_at IS NOT NULL OR s.liked_at IS NOT NULL OR s.local_path IS NOT NULL)"
            }
            SongScope::Liked => "s.liked_at IS NOT NULL",
            SongScope::Downloaded => "s.downloaded_at IS NOT NULL",
            SongScope::Local => "s.local_path IS NOT NULL",
            SongScope::All => "1",
        }
    }

    fn added_column(self) -> &'static str {
        match self {
            SongScope::Liked => "s.liked_at",
            SongScope::Downloaded => "s.downloaded_at",
            _ => "COALESCE(s.in_library_at, s.liked_at, s.created_at)",
        }
    }
}

/// Songs that count as "in the collection" for artist/album pages and local search.
const COLLECTED: &str = "(s.in_library_at IS NOT NULL OR s.liked_at IS NOT NULL \
     OR s.local_path IS NOT NULL OR s.downloaded_at IS NOT NULL)";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SongSort {
    /// When the song entered the scope (liked/downloaded/added); see [`SongScope`].
    #[default]
    DateAdded,
    Title,
    Artist,
    PlayCount,
    /// Total listening time.
    PlayTime,
    LastPlayed,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SongQuery {
    pub scope: SongScope,
    pub sort: SongSort,
    pub descending: bool,
    /// Case-insensitive substring of title, artists or album.
    pub filter: Option<String>,
    pub limit: Option<u32>,
    pub offset: u32,
}

/// ReplayGain values from tags (dB / linear peak).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct ReplayGain {
    pub track_gain_db: Option<f32>,
    pub track_peak: Option<f32>,
    pub album_gain_db: Option<f32>,
    pub album_peak: Option<f32>,
}

/// Song metadata beyond [`Track`] (mostly from local files' tags).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SongExtras {
    pub album_artist: Option<String>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub year: Option<i32>,
    pub replay_gain: ReplayGain,
}

/// A song row with its library state.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LibrarySong {
    pub track: Track,
    pub extras: SongExtras,
    pub liked_at: Option<i64>,
    pub in_library_at: Option<i64>,
    /// First time the database saw this song.
    pub created_at: i64,
    pub total_play_ms: u64,
    pub play_count: u64,
    pub last_played_at: Option<i64>,
    /// Set while a user download exists (see [`crate::downloads`]).
    pub downloaded_at: Option<i64>,
    /// False for downloads that are only the ~1 MiB preview.
    pub download_complete: bool,
    /// Thumbs down (YouTube Music's dislike); never set together with `liked_at`.
    pub disliked_at: Option<i64>,
}

impl LibrarySong {
    pub fn liked(&self) -> bool {
        self.liked_at.is_some()
    }

    pub fn disliked(&self) -> bool {
        self.disliked_at.is_some()
    }

    pub fn in_library(&self) -> bool {
        self.in_library_at.is_some()
    }
}

pub(crate) const SONG_COLS: &str = "s.id, s.title, s.artists, s.artist_refs, s.album_name, \
    s.album_id, s.duration_secs, s.thumbnail_url, s.explicit, s.local_path, s.album_artist, \
    s.track_number, s.disc_number, s.year, s.rg_track_gain, s.rg_track_peak, s.rg_album_gain, \
    s.rg_album_peak, s.liked_at, s.in_library_at, s.created_at, s.total_play_ms, s.play_count, \
    s.last_played_at, s.downloaded_at, s.download_complete, s.disliked_at";
pub(crate) const SONG_COL_COUNT: usize = 27;

pub(crate) fn song_from_row(row: &Row<'_>, base: usize) -> rusqlite::Result<LibrarySong> {
    let c = |i: usize| base + i;
    let refs: String = row.get(c(3))?;
    let local_path: Option<String> = row.get(c(9))?;
    Ok(LibrarySong {
        track: Track {
            video_id: row.get(c(0))?,
            title: row.get(c(1))?,
            artists: row.get(c(2))?,
            artist_refs: serde_json::from_str(&refs).unwrap_or_default(),
            album: row.get(c(4))?,
            album_id: row.get(c(5))?,
            duration_secs: row.get(c(6))?,
            thumbnail_url: row.get(c(7))?,
            explicit: row.get(c(8))?,
            local_path: local_path.map(PathBuf::from),
            ..Track::default()
        },
        extras: SongExtras {
            album_artist: row.get(c(10))?,
            track_number: row.get(c(11))?,
            disc_number: row.get(c(12))?,
            year: row.get(c(13))?,
            replay_gain: ReplayGain {
                track_gain_db: row.get(c(14))?,
                track_peak: row.get(c(15))?,
                album_gain_db: row.get(c(16))?,
                album_peak: row.get(c(17))?,
            },
        },
        liked_at: row.get(c(18))?,
        in_library_at: row.get(c(19))?,
        created_at: row.get(c(20))?,
        total_play_ms: row.get::<_, i64>(c(21))?.max(0) as u64,
        play_count: row.get::<_, i64>(c(22))?.max(0) as u64,
        last_played_at: row.get(c(23))?,
        downloaded_at: row.get(c(24))?,
        download_complete: row.get(c(25))?,
        disliked_at: row.get(c(26))?,
    })
}

/// Artist refs for a track, falling back to one ref from the display string.
fn artist_refs(track: &Track) -> Vec<EntityRef> {
    if !track.artist_refs.is_empty() {
        return track.artist_refs.clone();
    }
    if track.artists.trim().is_empty() {
        return Vec::new();
    }
    vec![EntityRef {
        name: track.artists.trim().to_owned(),
        id: None,
    }]
}

/// Inserts or refreshes a song and its artist/album links. Existing optional metadata is
/// kept when `track` lacks it; library state (likes, plays, downloads) is never touched.
pub(crate) fn upsert_track_tx(
    c: &Connection,
    track: &Track,
    extras: Option<&SongExtras>,
    now: i64,
) -> Result<()> {
    let refs = artist_refs(track);
    let refs_json = serde_json::to_string(&track.artist_refs).unwrap_or_else(|_| "[]".into());
    c.execute(
        "INSERT INTO song (id, title, artists, artist_refs, album_name, album_id, duration_secs,
                           thumbnail_url, explicit, local_path, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)
         ON CONFLICT(id) DO UPDATE SET
            title = CASE WHEN excluded.title <> '' THEN excluded.title ELSE song.title END,
            artists = CASE WHEN excluded.artists <> '' THEN excluded.artists ELSE song.artists END,
            artist_refs = CASE WHEN excluded.artist_refs <> '[]' THEN excluded.artist_refs
                               ELSE song.artist_refs END,
            album_name = COALESCE(excluded.album_name, song.album_name),
            album_id = COALESCE(excluded.album_id, song.album_id),
            duration_secs = COALESCE(excluded.duration_secs, song.duration_secs),
            thumbnail_url = COALESCE(excluded.thumbnail_url, song.thumbnail_url),
            explicit = excluded.explicit,
            local_path = COALESCE(excluded.local_path, song.local_path),
            updated_at = excluded.updated_at",
        params![
            track.video_id,
            track.title,
            track.artists,
            refs_json,
            track.album,
            track.album_id,
            track.duration_secs,
            track.thumbnail_url,
            track.explicit,
            track
                .local_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            now,
        ],
    )?;
    if let Some(x) = extras {
        c.execute(
            "UPDATE song SET album_artist = ?2, track_number = ?3, disc_number = ?4, year = ?5,
                    rg_track_gain = ?6, rg_track_peak = ?7, rg_album_gain = ?8, rg_album_peak = ?9
             WHERE id = ?1",
            params![
                track.video_id,
                x.album_artist,
                x.track_number,
                x.disc_number,
                x.year,
                x.replay_gain.track_gain_db,
                x.replay_gain.track_peak,
                x.replay_gain.album_gain_db,
                x.replay_gain.album_peak,
            ],
        )?;
    }

    if !refs.is_empty() {
        c.execute(
            "DELETE FROM song_artist WHERE song_id = ?1",
            [&track.video_id],
        )?;
        for (position, artist) in refs.iter().enumerate() {
            let id = artist
                .id
                .clone()
                .unwrap_or_else(|| synthetic_artist_id(&artist.name));
            upsert_artist_tx(c, &id, &artist.name, None, now)?;
            c.execute(
                "INSERT OR REPLACE INTO song_artist (song_id, artist_id, position) VALUES (?1, ?2, ?3)",
                params![track.video_id, id, position as i64],
            )?;
        }
    }

    if track.album.is_some() || track.album_id.is_some() {
        let album_artist = extras
            .and_then(|x| x.album_artist.clone())
            .or_else(|| refs.first().map(|a| a.name.clone()))
            .unwrap_or_default();
        let title = track.album.clone().unwrap_or_default();
        let id = track
            .album_id
            .clone()
            .unwrap_or_else(|| synthetic_album_id(&title, &album_artist));
        let artists = extras
            .and_then(|x| x.album_artist.clone())
            .unwrap_or_else(|| track.artists.clone());
        c.execute(
            "INSERT INTO album (id, title, artists, year, thumbnail_url, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT(id) DO UPDATE SET
                title = CASE WHEN album.title = '' THEN excluded.title ELSE album.title END,
                artists = CASE WHEN album.artists = '' THEN excluded.artists ELSE album.artists END,
                year = COALESCE(album.year, excluded.year),
                thumbnail_url = COALESCE(album.thumbnail_url, excluded.thumbnail_url)",
            params![
                id,
                title,
                artists,
                extras.and_then(|x| x.year),
                track.thumbnail_url,
                now
            ],
        )?;
        c.execute(
            "DELETE FROM song_album WHERE song_id = ?1",
            [&track.video_id],
        )?;
        c.execute(
            "INSERT INTO song_album (song_id, album_id, track_index) VALUES (?1, ?2, ?3)",
            params![track.video_id, id, extras.and_then(|x| x.track_number)],
        )?;
    }
    Ok(())
}

fn upsert_artist_tx(
    c: &Connection,
    id: &str,
    name: &str,
    thumbnail_url: Option<&str>,
    now: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO artist (id, name, thumbnail_url, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?4)
         ON CONFLICT(id) DO UPDATE SET
            name = CASE WHEN excluded.name <> '' THEN excluded.name ELSE artist.name END,
            thumbnail_url = COALESCE(excluded.thumbnail_url, artist.thumbnail_url),
            updated_at = excluded.updated_at",
        params![id, name, thumbnail_url, now],
    )?;
    Ok(())
}

/// Artist metadata to store (e.g. from an artist page).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ArtistMeta {
    pub id: String,
    pub name: String,
    pub thumbnail_url: Option<String>,
}

/// An artist with library aggregates.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LibraryArtist {
    pub id: String,
    pub name: String,
    pub thumbnail_url: Option<String>,
    pub bookmarked_at: Option<i64>,
    pub created_at: i64,
    /// Songs by this artist in the collection (library, liked, local or downloaded).
    pub song_count: u32,
    /// Listening time over all of the artist's songs.
    pub total_play_ms: u64,
}

impl LibraryArtist {
    /// The YouTube channel id, unless the artist was created from a name only.
    pub fn channel_id(&self) -> Option<&str> {
        (!is_synthetic_id(&self.id)).then_some(self.id.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ArtistSort {
    #[default]
    Name,
    /// Bookmark time, else first seen.
    DateAdded,
    SongCount,
    PlayTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ArtistQuery {
    pub bookmarked_only: bool,
    pub filter: Option<String>,
    pub sort: ArtistSort,
    pub descending: bool,
    pub limit: Option<u32>,
    pub offset: u32,
}

/// Album metadata to store (e.g. from an album page).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AlbumMeta {
    pub id: String,
    pub title: String,
    /// Display string.
    pub artists: String,
    pub year: Option<i32>,
    pub thumbnail_url: Option<String>,
    /// Playlist id of the album (`OLAK5uy_…`), for playing it.
    pub playlist_id: Option<String>,
    pub track_count: Option<u32>,
    pub duration_secs: Option<u32>,
    pub explicit: bool,
}

/// An album with library aggregates.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LibraryAlbum {
    pub meta: AlbumMeta,
    pub bookmarked_at: Option<i64>,
    pub created_at: i64,
    /// Songs of the album in the collection.
    pub song_count: u32,
    pub total_play_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AlbumSort {
    #[default]
    Title,
    Artist,
    Year,
    DateAdded,
    SongCount,
    PlayTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AlbumQuery {
    pub bookmarked_only: bool,
    pub filter: Option<String>,
    pub sort: AlbumSort,
    pub descending: bool,
    pub limit: Option<u32>,
    pub offset: u32,
}

/// Results of [`Library::search_local`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LocalSearch {
    pub songs: Vec<LibrarySong>,
    pub artists: Vec<LibraryArtist>,
    pub albums: Vec<LibraryAlbum>,
    pub playlists: Vec<Playlist>,
}

const ARTIST_SELECT: &str = "SELECT a.id, a.name, a.thumbnail_url, a.bookmarked_at, a.created_at,
        (SELECT COUNT(*) FROM song_artist sa JOIN song s ON s.id = sa.song_id
          WHERE sa.artist_id = a.id AND (s.in_library_at IS NOT NULL OR s.liked_at IS NOT NULL
                OR s.local_path IS NOT NULL OR s.downloaded_at IS NOT NULL)) AS song_count,
        (SELECT COALESCE(SUM(s.total_play_ms), 0) FROM song_artist sa JOIN song s ON s.id = sa.song_id
          WHERE sa.artist_id = a.id) AS play_ms
     FROM artist a";

fn artist_from_row(row: &Row<'_>) -> rusqlite::Result<LibraryArtist> {
    Ok(LibraryArtist {
        id: row.get(0)?,
        name: row.get(1)?,
        thumbnail_url: row.get(2)?,
        bookmarked_at: row.get(3)?,
        created_at: row.get(4)?,
        song_count: row.get(5)?,
        total_play_ms: row.get::<_, i64>(6)?.max(0) as u64,
    })
}

const ALBUM_SELECT: &str = "SELECT al.id, al.title, al.artists, al.year, al.thumbnail_url,
        al.playlist_id, al.song_count, al.duration_secs, al.explicit, al.bookmarked_at, al.created_at,
        (SELECT COUNT(*) FROM song_album sa JOIN song s ON s.id = sa.song_id
          WHERE sa.album_id = al.id AND (s.in_library_at IS NOT NULL OR s.liked_at IS NOT NULL
                OR s.local_path IS NOT NULL OR s.downloaded_at IS NOT NULL)) AS lib_count,
        (SELECT COALESCE(SUM(s.total_play_ms), 0) FROM song_album sa JOIN song s ON s.id = sa.song_id
          WHERE sa.album_id = al.id) AS play_ms
     FROM album al";

fn album_from_row(row: &Row<'_>) -> rusqlite::Result<LibraryAlbum> {
    Ok(LibraryAlbum {
        meta: AlbumMeta {
            id: row.get(0)?,
            title: row.get(1)?,
            artists: row.get(2)?,
            year: row.get(3)?,
            thumbnail_url: row.get(4)?,
            playlist_id: row.get(5)?,
            track_count: row.get(6)?,
            duration_secs: row.get(7)?,
            explicit: row.get(8)?,
        },
        bookmarked_at: row.get(9)?,
        created_at: row.get(10)?,
        song_count: row.get(11)?,
        total_play_ms: row.get::<_, i64>(12)?.max(0) as u64,
    })
}

fn direction(descending: bool) -> &'static str {
    if descending { "DESC" } else { "ASC" }
}

fn limit_clause(limit: Option<u32>, offset: u32) -> String {
    format!(" LIMIT {} OFFSET {offset}", limit.map_or(-1, i64::from))
}

impl Library {
    /// Stores `track`'s metadata (no library state change).
    pub fn upsert_track(&self, track: &Track) -> Result<()> {
        self.write(|tx| upsert_track_tx(tx, track, None, now_ms()))
    }

    pub fn upsert_tracks(&self, tracks: &[Track]) -> Result<()> {
        let now = now_ms();
        self.write(|tx| {
            tracks
                .iter()
                .try_for_each(|t| upsert_track_tx(tx, t, None, now))
        })
    }

    /// Stores `track` plus tag metadata; `extras` overwrite what was stored.
    pub fn upsert_song(&self, track: &Track, extras: &SongExtras) -> Result<()> {
        self.write(|tx| upsert_track_tx(tx, track, Some(extras), now_ms()))
    }

    pub fn song(&self, video_id: &str) -> Result<Option<LibrarySong>> {
        self.read(|c| {
            c.query_row(
                &format!("SELECT {SONG_COLS} FROM song s WHERE s.id = ?1"),
                [video_id],
                |r| song_from_row(r, 0),
            )
            .optional()
        })
    }

    /// Removes a song and everything that references it (history, playlist entries).
    pub fn delete_song(&self, video_id: &str) -> Result<()> {
        self.read(|c| c.execute("DELETE FROM song WHERE id = ?1", [video_id]))?;
        Ok(())
    }

    /// Likes `track` (stores it first). Liking a YouTube song also adds it to the library,
    /// as in ArchiveTune.
    pub fn like(&self, track: &Track) -> Result<()> {
        self.set_liked_at(track, Some(now_ms()))
    }

    pub fn unlike(&self, video_id: &str) -> Result<()> {
        self.read(|c| c.execute("UPDATE song SET liked_at = NULL WHERE id = ?1", [video_id]))?;
        Ok(())
    }

    /// Like with an explicit time (e.g. when syncing YouTube likes), or unlike with `None`.
    /// An existing like time is kept.
    pub fn set_liked_at(&self, track: &Track, at: Option<i64>) -> Result<()> {
        self.write(|tx| {
            upsert_track_tx(tx, track, None, now_ms())?;
            match at {
                Some(at) => tx.execute(
                    "UPDATE song SET liked_at = COALESCE(liked_at, ?2), disliked_at = NULL,
                        in_library_at = CASE WHEN local_path IS NULL
                                             THEN COALESCE(in_library_at, ?2)
                                             ELSE in_library_at END
                     WHERE id = ?1",
                    params![track.video_id, at],
                )?,
                None => tx.execute(
                    "UPDATE song SET liked_at = NULL WHERE id = ?1",
                    [&track.video_id],
                )?,
            };
            Ok(())
        })
    }

    pub fn add_to_library(&self, track: &Track) -> Result<()> {
        self.set_in_library_at(track, Some(now_ms()))
    }

    /// Removes from the library; this also unlikes it (ArchiveTune's `toggleLibrary`).
    pub fn remove_from_library(&self, video_id: &str) -> Result<()> {
        self.read(|c| {
            c.execute(
                "UPDATE song SET in_library_at = NULL, liked_at = NULL WHERE id = ?1",
                [video_id],
            )
        })?;
        Ok(())
    }

    /// Adds with an explicit time (kept if already in the library), or removes with `None`.
    pub fn set_in_library_at(&self, track: &Track, at: Option<i64>) -> Result<()> {
        match at {
            Some(at) => self.write(|tx| {
                upsert_track_tx(tx, track, None, now_ms())?;
                tx.execute(
                    "UPDATE song SET in_library_at = COALESCE(in_library_at, ?2) WHERE id = ?1",
                    params![track.video_id, at],
                )?;
                Ok(())
            }),
            None => self.remove_from_library(&track.video_id),
        }
    }

    /// Sorted, filtered song listing.
    pub fn songs(&self, q: &SongQuery) -> Result<Vec<LibrarySong>> {
        let mut args: Vec<Value> = Vec::new();
        let mut sql = format!(
            "SELECT {SONG_COLS} FROM song s WHERE {}",
            q.scope.predicate()
        );
        if let Some(filter) = q.filter.as_deref().filter(|f| !f.trim().is_empty()) {
            sql.push_str(
                " AND (s.title LIKE ?1 ESCAPE '\\' OR s.artists LIKE ?1 ESCAPE '\\' \
                 OR s.album_name LIKE ?1 ESCAPE '\\')",
            );
            args.push(Value::Text(like_pattern(filter)));
        }
        let dir = direction(q.descending);
        let order = match q.sort {
            SongSort::DateAdded => format!("{} {dir}", q.scope.added_column()),
            SongSort::Title => format!("s.title COLLATE NOCASE {dir}"),
            SongSort::Artist => {
                format!("s.artists COLLATE NOCASE {dir}, s.title COLLATE NOCASE {dir}")
            }
            SongSort::PlayCount => format!("s.play_count {dir}, s.total_play_ms {dir}"),
            SongSort::PlayTime => format!("s.total_play_ms {dir}"),
            SongSort::LastPlayed => format!("s.last_played_at {dir}"),
        };
        sql.push_str(&format!(" ORDER BY {order}, s.id {dir}"));
        sql.push_str(&limit_clause(q.limit, q.offset));
        self.read(|c| {
            let mut stmt = c.prepare(&sql)?;
            stmt.query_map(params_from_iter(args), |r| song_from_row(r, 0))?
                .collect()
        })
    }

    pub fn song_count(&self, scope: SongScope) -> Result<u64> {
        self.read(|c| {
            c.query_row(
                &format!("SELECT COUNT(*) FROM song s WHERE {}", scope.predicate()),
                [],
                |r| r.get::<_, i64>(0),
            )
        })
        .map(|n| n as u64)
    }

    /// Marks a user download (`Some(complete)`, storing the track) or its removal (`None`).
    /// Called by [`crate::downloads::DownloadManager`].
    pub fn set_downloaded(&self, track: &Track, complete: Option<bool>) -> Result<()> {
        self.write(|tx| {
            match complete {
                Some(complete) => {
                    upsert_track_tx(tx, track, None, now_ms())?;
                    tx.execute(
                        "UPDATE song SET downloaded_at = COALESCE(downloaded_at, ?2),
                                download_complete = ?3 WHERE id = ?1",
                        params![track.video_id, now_ms(), complete],
                    )?;
                }
                None => {
                    tx.execute(
                        "UPDATE song SET downloaded_at = NULL, download_complete = 0 WHERE id = ?1",
                        [&track.video_id],
                    )?;
                }
            }
            Ok(())
        })
    }

    // ---- artists ---------------------------------------------------------------------------

    pub fn upsert_artist(&self, artist: &ArtistMeta) -> Result<()> {
        self.write(|tx| {
            upsert_artist_tx(
                tx,
                &artist.id,
                &artist.name,
                artist.thumbnail_url.as_deref(),
                now_ms(),
            )
        })
    }

    /// Bookmarks ("subscribes to") an artist locally. No-op for unknown ids.
    pub fn set_artist_bookmarked(&self, id: &str, bookmarked: bool) -> Result<()> {
        self.read(|c| {
            c.execute(
                "UPDATE artist SET bookmarked_at = CASE WHEN ?2 THEN COALESCE(bookmarked_at, ?3)
                                                        ELSE NULL END WHERE id = ?1",
                params![id, bookmarked, now_ms()],
            )
        })?;
        Ok(())
    }

    pub fn artist(&self, id: &str) -> Result<Option<LibraryArtist>> {
        self.read(|c| {
            c.query_row(
                &format!("{ARTIST_SELECT} WHERE a.id = ?1"),
                [id],
                artist_from_row,
            )
            .optional()
        })
    }

    /// Bookmarked artists plus artists of songs in the collection.
    pub fn artists(&self, q: &ArtistQuery) -> Result<Vec<LibraryArtist>> {
        let mut args: Vec<Value> = Vec::new();
        let mut sql = format!("SELECT * FROM ({ARTIST_SELECT}) WHERE ");
        sql.push_str(if q.bookmarked_only {
            "bookmarked_at IS NOT NULL"
        } else {
            "(bookmarked_at IS NOT NULL OR song_count > 0)"
        });
        if let Some(filter) = q.filter.as_deref().filter(|f| !f.trim().is_empty()) {
            sql.push_str(" AND name LIKE ?1 ESCAPE '\\'");
            args.push(Value::Text(like_pattern(filter)));
        }
        let dir = direction(q.descending);
        let order = match q.sort {
            ArtistSort::Name => format!("name COLLATE NOCASE {dir}"),
            ArtistSort::DateAdded => format!("COALESCE(bookmarked_at, created_at) {dir}"),
            ArtistSort::SongCount => format!("song_count {dir}, name COLLATE NOCASE"),
            ArtistSort::PlayTime => format!("play_ms {dir}, name COLLATE NOCASE"),
        };
        sql.push_str(&format!(" ORDER BY {order}, id"));
        sql.push_str(&limit_clause(q.limit, q.offset));
        self.read(|c| {
            let mut stmt = c.prepare(&sql)?;
            stmt.query_map(params_from_iter(args), artist_from_row)?
                .collect()
        })
    }

    /// The artist's songs in the collection, most played first.
    pub fn artist_songs(&self, artist_id: &str) -> Result<Vec<LibrarySong>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {SONG_COLS} FROM song_artist sa JOIN song s ON s.id = sa.song_id
                 WHERE sa.artist_id = ?1 AND {COLLECTED}
                 ORDER BY s.total_play_ms DESC, s.title COLLATE NOCASE"
            ))?;
            stmt.query_map([artist_id], |r| song_from_row(r, 0))?
                .collect()
        })
    }

    // ---- albums ----------------------------------------------------------------------------

    /// Stores album metadata and, if given, its track list (in album order).
    pub fn upsert_album(&self, album: &AlbumMeta, tracks: &[Track]) -> Result<()> {
        let now = now_ms();
        self.write(|tx| {
            tx.execute(
                "INSERT INTO album (id, title, artists, year, thumbnail_url, playlist_id, song_count,
                                    duration_secs, explicit, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
                 ON CONFLICT(id) DO UPDATE SET
                    title = excluded.title, artists = excluded.artists,
                    year = COALESCE(excluded.year, album.year),
                    thumbnail_url = COALESCE(excluded.thumbnail_url, album.thumbnail_url),
                    playlist_id = COALESCE(excluded.playlist_id, album.playlist_id),
                    song_count = COALESCE(excluded.song_count, album.song_count),
                    duration_secs = COALESCE(excluded.duration_secs, album.duration_secs),
                    explicit = excluded.explicit, updated_at = excluded.updated_at",
                params![
                    album.id,
                    album.title,
                    album.artists,
                    album.year,
                    album.thumbnail_url,
                    album.playlist_id,
                    album.track_count,
                    album.duration_secs,
                    album.explicit,
                    now
                ],
            )?;
            for (index, track) in tracks.iter().enumerate() {
                upsert_track_tx(tx, track, None, now)?;
                tx.execute(
                    "DELETE FROM song_album WHERE song_id = ?1",
                    [&track.video_id],
                )?;
                tx.execute(
                    "INSERT INTO song_album (song_id, album_id, track_index) VALUES (?1, ?2, ?3)",
                    params![track.video_id, album.id, index as u32 + 1],
                )?;
            }
            Ok(())
        })
    }

    pub fn set_album_bookmarked(&self, id: &str, bookmarked: bool) -> Result<()> {
        self.read(|c| {
            c.execute(
                "UPDATE album SET bookmarked_at = CASE WHEN ?2 THEN COALESCE(bookmarked_at, ?3)
                                                       ELSE NULL END WHERE id = ?1",
                params![id, bookmarked, now_ms()],
            )
        })?;
        Ok(())
    }

    pub fn album(&self, id: &str) -> Result<Option<LibraryAlbum>> {
        self.read(|c| {
            c.query_row(
                &format!("{ALBUM_SELECT} WHERE al.id = ?1"),
                [id],
                album_from_row,
            )
            .optional()
        })
    }

    /// Bookmarked albums plus albums with songs in the collection.
    pub fn albums(&self, q: &AlbumQuery) -> Result<Vec<LibraryAlbum>> {
        let mut args: Vec<Value> = Vec::new();
        let mut sql = format!("SELECT * FROM ({ALBUM_SELECT}) WHERE ");
        sql.push_str(if q.bookmarked_only {
            "bookmarked_at IS NOT NULL"
        } else {
            "(bookmarked_at IS NOT NULL OR lib_count > 0)"
        });
        if let Some(filter) = q.filter.as_deref().filter(|f| !f.trim().is_empty()) {
            sql.push_str(" AND (title LIKE ?1 ESCAPE '\\' OR artists LIKE ?1 ESCAPE '\\')");
            args.push(Value::Text(like_pattern(filter)));
        }
        let dir = direction(q.descending);
        let order = match q.sort {
            AlbumSort::Title => format!("title COLLATE NOCASE {dir}"),
            AlbumSort::Artist => {
                format!("artists COLLATE NOCASE {dir}, title COLLATE NOCASE {dir}")
            }
            AlbumSort::Year => format!("year {dir}, title COLLATE NOCASE"),
            AlbumSort::DateAdded => format!("COALESCE(bookmarked_at, created_at) {dir}"),
            AlbumSort::SongCount => format!("lib_count {dir}, title COLLATE NOCASE"),
            AlbumSort::PlayTime => format!("play_ms {dir}, title COLLATE NOCASE"),
        };
        sql.push_str(&format!(" ORDER BY {order}, id"));
        sql.push_str(&limit_clause(q.limit, q.offset));
        self.read(|c| {
            let mut stmt = c.prepare(&sql)?;
            stmt.query_map(params_from_iter(args), album_from_row)?
                .collect()
        })
    }

    /// Every known song of the album, in disc/track order.
    pub fn album_songs(&self, album_id: &str) -> Result<Vec<LibrarySong>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {SONG_COLS} FROM song_album sa JOIN song s ON s.id = sa.song_id
                 WHERE sa.album_id = ?1
                 ORDER BY COALESCE(s.disc_number, 1), sa.track_index IS NULL, sa.track_index,
                          s.title COLLATE NOCASE"
            ))?;
            stmt.query_map([album_id], |r| song_from_row(r, 0))?
                .collect()
        })
    }

    // ---- search ----------------------------------------------------------------------------

    /// Case-insensitive substring search over the collection (songs in the library, liked,
    /// local, downloaded or in a playlist), artists, albums and playlists. Prefix matches
    /// rank first, then the most played.
    pub fn search_local(&self, query: &str, limit_per_kind: u32) -> Result<LocalSearch> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(LocalSearch::default());
        }
        let pattern = like_pattern(query);
        let prefix = {
            let mut p = like_pattern(query);
            p.remove(0);
            p
        };
        let limit = i64::from(limit_per_kind);
        let songs = self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {SONG_COLS} FROM song s
                 WHERE ({COLLECTED} OR EXISTS (SELECT 1 FROM playlist_entry pe WHERE pe.song_id = s.id))
                   AND (s.title LIKE ?1 ESCAPE '\\' OR s.artists LIKE ?1 ESCAPE '\\'
                        OR s.album_name LIKE ?1 ESCAPE '\\')
                 ORDER BY (s.title LIKE ?2 ESCAPE '\\') DESC, s.play_count DESC,
                          s.title COLLATE NOCASE
                 LIMIT ?3"
            ))?;
            stmt.query_map(params![pattern, prefix, limit], |r| song_from_row(r, 0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })?;
        let artists = self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT * FROM ({ARTIST_SELECT})
                 WHERE (bookmarked_at IS NOT NULL OR song_count > 0) AND name LIKE ?1 ESCAPE '\\'
                 ORDER BY (name LIKE ?2 ESCAPE '\\') DESC, play_ms DESC, name COLLATE NOCASE
                 LIMIT ?3"
            ))?;
            stmt.query_map(params![pattern, prefix, limit], artist_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()
        })?;
        let albums = self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT * FROM ({ALBUM_SELECT})
                 WHERE (bookmarked_at IS NOT NULL OR lib_count > 0)
                   AND (title LIKE ?1 ESCAPE '\\' OR artists LIKE ?1 ESCAPE '\\')
                 ORDER BY (title LIKE ?2 ESCAPE '\\') DESC, play_ms DESC, title COLLATE NOCASE
                 LIMIT ?3"
            ))?;
            stmt.query_map(params![pattern, prefix, limit], album_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()
        })?;
        let playlists = self.search_playlists(&pattern, &prefix, limit)?;
        Ok(LocalSearch {
            songs,
            artists,
            albums,
            playlists,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::test_util::track;

    fn ids(songs: &[LibrarySong]) -> Vec<&str> {
        songs.iter().map(|s| s.track.video_id.as_str()).collect()
    }

    #[test]
    fn upsert_round_trips_track_and_keeps_known_metadata() {
        let lib = Library::open_in_memory().unwrap();
        let mut t = track("v1", "Song", "Artist", Some(("MPREb_1", "Album")));
        t.explicit = true;
        t.thumbnail_url = Some("https://x/=w120-h120".into());
        lib.upsert_track(&t).unwrap();
        assert_eq!(lib.song("v1").unwrap().unwrap().track, t);

        // A sparser copy (e.g. from history) doesn't wipe the album or artwork.
        let sparse = Track {
            video_id: "v1".into(),
            title: "Song (Remastered)".into(),
            ..Default::default()
        };
        lib.upsert_track(&sparse).unwrap();
        let s = lib.song("v1").unwrap().unwrap();
        assert_eq!(s.track.title, "Song (Remastered)");
        assert_eq!(s.track.album.as_deref(), Some("Album"));
        assert_eq!(s.track.artists, "Artist");
        assert_eq!(s.track.artist_refs, t.artist_refs);
        assert!(lib.song("nope").unwrap().is_none());
    }

    #[test]
    fn like_unlike_and_library_membership() {
        let lib = Library::open_in_memory().unwrap();
        let t = track("v1", "Song", "Artist", None);
        lib.like(&t).unwrap();
        let s = lib.song("v1").unwrap().unwrap();
        assert!(s.liked() && s.in_library());
        lib.unlike("v1").unwrap();
        let s = lib.song("v1").unwrap().unwrap();
        assert!(!s.liked() && s.in_library());
        lib.like(&t).unwrap();
        lib.remove_from_library("v1").unwrap();
        let s = lib.song("v1").unwrap().unwrap();
        assert!(!s.liked() && !s.in_library());

        // Local songs are liked without being "added".
        let mut local = track("local:1", "L", "A", None);
        local.local_path = Some("/music/l.mp3".into());
        lib.like(&local).unwrap();
        let s = lib.song("local:1").unwrap().unwrap();
        assert!(s.liked() && !s.in_library());
        assert_eq!(lib.song_count(SongScope::Library).unwrap(), 1);
        assert_eq!(lib.song_count(SongScope::Local).unwrap(), 1);
        assert_eq!(lib.song_count(SongScope::All).unwrap(), 2);

        // Explicit like time is kept on re-like.
        lib.set_liked_at(&t, Some(5)).unwrap();
        lib.set_liked_at(&t, Some(9)).unwrap();
        assert_eq!(lib.song("v1").unwrap().unwrap().liked_at, Some(5));
        lib.set_liked_at(&t, None).unwrap();
        assert_eq!(lib.song("v1").unwrap().unwrap().liked_at, None);
    }

    #[test]
    fn sorting_and_filtering() {
        let lib = Library::open_in_memory().unwrap();
        let a = track("a", "banana", "Zed", Some(("MPREb_x", "Fruit")));
        let b = track("b", "Apple", "Mia", None);
        let c = track("c", "cherry", "Abe", None);
        lib.set_in_library_at(&a, Some(300)).unwrap();
        lib.set_in_library_at(&b, Some(100)).unwrap();
        lib.set_in_library_at(&c, Some(200)).unwrap();
        lib.record_play(&a, 1_000, 10).unwrap();
        lib.record_play(&a, 1_000, 20).unwrap();
        lib.record_play(&b, 5_000, 30).unwrap();
        lib.upsert_track(&track("d", "not in library", "X", None))
            .unwrap();

        let q = |sort, descending| SongQuery {
            sort,
            descending,
            ..Default::default()
        };
        let list = |q: SongQuery| ids(&lib.songs(&q).unwrap()).join("");
        assert_eq!(list(q(SongSort::Title, false)), "bac");
        assert_eq!(list(q(SongSort::Title, true)), "cab");
        assert_eq!(list(q(SongSort::Artist, false)), "cba");
        assert_eq!(list(q(SongSort::DateAdded, true)), "acb");
        assert_eq!(list(q(SongSort::DateAdded, false)), "bca");
        assert_eq!(list(q(SongSort::PlayCount, true)), "abc");
        assert_eq!(list(q(SongSort::PlayTime, true)), "bac");
        assert_eq!(list(q(SongSort::LastPlayed, true)), "bac");

        let filtered = SongQuery {
            filter: Some("FRU".into()),
            ..Default::default()
        };
        assert_eq!(list(filtered), "a");
        let filtered = SongQuery {
            filter: Some("APP".into()),
            sort: SongSort::Title,
            ..Default::default()
        };
        assert_eq!(list(filtered), "b");
        let paged = SongQuery {
            sort: SongSort::Title,
            limit: Some(1),
            offset: 1,
            ..Default::default()
        };
        assert_eq!(list(paged), "a");
        let all = SongQuery {
            scope: SongScope::All,
            sort: SongSort::Title,
            ..Default::default()
        };
        assert_eq!(list(all), "bacd");
    }

    #[test]
    fn artists_and_albums() {
        let lib = Library::open_in_memory().unwrap();
        let mut t1 = track("v1", "One", "Daft Punk", Some(("MPREb_d", "Discovery")));
        t1.artist_refs.push(EntityRef {
            name: "Romanthony".into(),
            id: None,
        });
        t1.artists = "Daft Punk, Romanthony".into();
        let t2 = track("v2", "Two", "Daft Punk", Some(("MPREb_d", "Discovery")));
        lib.add_to_library(&t1).unwrap();
        lib.add_to_library(&t2).unwrap();
        lib.record_play(&t2, 7_000, 1).unwrap();

        let artists = lib.artists(&ArtistQuery::default()).unwrap();
        assert_eq!(
            artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["Daft Punk", "Romanthony"]
        );
        assert_eq!(artists[0].song_count, 2);
        assert_eq!(artists[0].total_play_ms, 7_000);
        assert_eq!(artists[0].channel_id(), Some("UCDaftPunk"));
        assert_eq!(artists[1].channel_id(), None);
        let by_count = lib
            .artists(&ArtistQuery {
                sort: ArtistSort::SongCount,
                descending: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(by_count[0].name, "Daft Punk");
        assert_eq!(ids(&lib.artist_songs("UCDaftPunk").unwrap()), ["v2", "v1"]);

        lib.set_artist_bookmarked("UCDaftPunk", true).unwrap();
        let bookmarked = lib
            .artists(&ArtistQuery {
                bookmarked_only: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(bookmarked.len(), 1);
        assert!(bookmarked[0].bookmarked_at.is_some());

        let albums = lib.albums(&AlbumQuery::default()).unwrap();
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].meta.title, "Discovery");
        assert_eq!(albums[0].song_count, 2);

        // Album page metadata + track order.
        lib.upsert_album(
            &AlbumMeta {
                id: "MPREb_d".into(),
                title: "Discovery".into(),
                artists: "Daft Punk".into(),
                year: Some(2001),
                track_count: Some(14),
                ..Default::default()
            },
            &[t2.clone(), t1.clone()],
        )
        .unwrap();
        assert_eq!(ids(&lib.album_songs("MPREb_d").unwrap()), ["v2", "v1"]);
        let album = lib.album("MPREb_d").unwrap().unwrap();
        assert_eq!(album.meta.year, Some(2001));
        assert_eq!(album.meta.track_count, Some(14));
        lib.set_album_bookmarked("MPREb_d", true).unwrap();
        assert!(
            lib.album("MPREb_d")
                .unwrap()
                .unwrap()
                .bookmarked_at
                .is_some()
        );

        lib.upsert_artist(&ArtistMeta {
            id: "UCDaftPunk".into(),
            name: "Daft Punk".into(),
            thumbnail_url: Some("https://img".into()),
        })
        .unwrap();
        assert_eq!(
            lib.artist("UCDaftPunk")
                .unwrap()
                .unwrap()
                .thumbnail_url
                .as_deref(),
            Some("https://img")
        );
    }

    #[test]
    fn local_search() {
        let lib = Library::open_in_memory().unwrap();
        lib.add_to_library(&track(
            "a",
            "Around the World",
            "Daft Punk",
            Some(("MPREb_h", "Homework")),
        ))
        .unwrap();
        lib.add_to_library(&track("b", "World of Punk", "Other", None))
            .unwrap();
        lib.upsert_track(&track("c", "World history-only", "X", None))
            .unwrap();
        let pl = lib.create_playlist("Punk picks").unwrap();
        lib.add_to_playlist(&pl.id, &[track("d", "Playlist world", "Y", None)])
            .unwrap();

        let r = lib.search_local("world", 10).unwrap();
        assert_eq!(ids(&r.songs), ["b", "a", "d"]);
        let r = lib.search_local("punk", 10).unwrap();
        assert_eq!(r.artists.len(), 1);
        assert_eq!(r.playlists.len(), 1);
        let r = lib.search_local("home", 10).unwrap();
        assert_eq!(r.albums.len(), 1);
        assert_eq!(lib.search_local("  ", 10).unwrap(), LocalSearch::default());
    }

    #[test]
    fn delete_song_cascades() {
        let lib = Library::open_in_memory().unwrap();
        let t = track("v", "T", "A", None);
        lib.record_play(&t, 1000, 1).unwrap();
        let pl = lib.create_playlist("p").unwrap();
        lib.add_to_playlist(&pl.id, std::slice::from_ref(&t))
            .unwrap();
        lib.delete_song("v").unwrap();
        assert!(lib.history(Default::default(), 10, 0).unwrap().is_empty());
        assert!(lib.playlist_entries(&pl.id).unwrap().is_empty());
    }
}
