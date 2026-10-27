// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/db/entities/{PlaylistEntity,PlaylistSongMap}.kt
// and db/DatabaseDao.kt — © Rukamori and ArchiveTune contributors, GPL-3.0

//! Local playlists and local mirrors of YouTube playlists, with ordered entries.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use super::songs::{SONG_COL_COUNT, SONG_COLS, song_from_row, upsert_track_tx};
use super::{Library, LibrarySong, now_ms};
use crate::{Error, Result, Track};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Playlist {
    /// Local id (`LP…`), stable across renames and syncs.
    pub id: String,
    pub name: String,
    /// YouTube playlist id (`PL…`/`VL…`) for mirrors, `None` for purely local playlists.
    pub browse_id: Option<String>,
    /// Whether the user can change the entries (false for mirrors of others' playlists).
    pub editable: bool,
    pub thumbnail_url: Option<String>,
    /// Song count YouTube reported, for mirrors not fully synced yet.
    pub remote_song_count: Option<u32>,
    /// Entries stored locally.
    pub song_count: u32,
    pub bookmarked_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Mirror of a YouTube playlist to create or refresh; see [`Library::upsert_remote_playlist`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RemotePlaylist {
    pub browse_id: String,
    pub name: String,
    pub thumbnail_url: Option<String>,
    pub editable: bool,
    pub song_count: Option<u32>,
}

/// One entry of a playlist. The same song may appear several times.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistEntry {
    /// Row id, stable while the entry exists (use it to remove/move this exact entry).
    pub entry_id: i64,
    pub position: u32,
    /// YouTube's per-entry id (`setVideoId`), needed to edit the remote playlist.
    pub set_video_id: Option<String>,
    pub song: LibrarySong,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PlaylistSort {
    #[default]
    Name,
    Created,
    Updated,
    SongCount,
}

const PLAYLIST_SELECT: &str = "SELECT p.id, p.name, p.browse_id, p.editable, p.thumbnail_url,
        p.remote_song_count, (SELECT COUNT(*) FROM playlist_entry pe WHERE pe.playlist_id = p.id),
        p.bookmarked_at, p.created_at, p.updated_at
     FROM playlist p";

fn playlist_from_row(r: &Row<'_>) -> rusqlite::Result<Playlist> {
    Ok(Playlist {
        id: r.get(0)?,
        name: r.get(1)?,
        browse_id: r.get(2)?,
        editable: r.get(3)?,
        thumbnail_url: r.get(4)?,
        remote_song_count: r.get(5)?,
        song_count: r.get(6)?,
        bookmarked_at: r.get(7)?,
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
    })
}

fn new_playlist_id(name: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let seed = format!(
        "{name}|{nanos}|{}|{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    format!("LP{:x}", md5::compute(seed))[..14].to_owned()
}

fn get_playlist(c: &Connection, id: &str) -> rusqlite::Result<Option<Playlist>> {
    c.query_row(
        &format!("{PLAYLIST_SELECT} WHERE p.id = ?1"),
        [id],
        playlist_from_row,
    )
    .optional()
}

fn require_playlist(c: &Connection, id: &str) -> Result<Playlist> {
    get_playlist(c, id)?.ok_or_else(|| Error::Storage(format!("no playlist {id}")))
}

fn touch(c: &Connection, id: &str) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE playlist SET updated_at = ?2 WHERE id = ?1",
        params![id, now_ms()],
    )?;
    Ok(())
}

fn entry_count(c: &Connection, id: &str) -> rusqlite::Result<i64> {
    c.query_row(
        "SELECT COUNT(*) FROM playlist_entry WHERE playlist_id = ?1",
        [id],
        |r| r.get(0),
    )
}

/// Rewrites positions as 0..n in the current order.
fn renumber(c: &Connection, id: &str) -> rusqlite::Result<()> {
    let ids: Vec<i64> = {
        let mut stmt = c.prepare(
            "SELECT id FROM playlist_entry WHERE playlist_id = ?1 ORDER BY position, id",
        )?;
        stmt.query_map([id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    let mut stmt = c.prepare("UPDATE playlist_entry SET position = ?2 WHERE id = ?1")?;
    for (pos, entry) in ids.iter().enumerate() {
        stmt.execute(params![entry, pos as i64])?;
    }
    Ok(())
}

impl Library {
    /// Creates an empty local playlist.
    pub fn create_playlist(&self, name: &str) -> Result<Playlist> {
        let id = new_playlist_id(name);
        let now = now_ms();
        self.write(|tx| {
            tx.execute(
                "INSERT INTO playlist (id, name, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
                params![id, name, now],
            )?;
            require_playlist(tx, &id)
        })
    }

    /// Creates or refreshes the local mirror of a YouTube playlist (matched by `browse_id`).
    /// Entries are left alone; replace them with [`Self::replace_playlist_entries`].
    pub fn upsert_remote_playlist(&self, remote: &RemotePlaylist) -> Result<Playlist> {
        let now = now_ms();
        self.write(|tx| {
            let existing: Option<String> = tx
                .query_row(
                    "SELECT id FROM playlist WHERE browse_id = ?1",
                    [&remote.browse_id],
                    |r| r.get(0),
                )
                .optional()?;
            let id = match existing {
                Some(id) => {
                    tx.execute(
                        "UPDATE playlist SET name = ?2, thumbnail_url = COALESCE(?3, thumbnail_url),
                                editable = ?4, remote_song_count = ?5, updated_at = ?6
                         WHERE id = ?1",
                        params![
                            id,
                            remote.name,
                            remote.thumbnail_url,
                            remote.editable,
                            remote.song_count,
                            now
                        ],
                    )?;
                    id
                }
                None => {
                    let id = new_playlist_id(&remote.browse_id);
                    tx.execute(
                        "INSERT INTO playlist (id, name, browse_id, editable, thumbnail_url,
                                               remote_song_count, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                        params![
                            id,
                            remote.name,
                            remote.browse_id,
                            remote.editable,
                            remote.thumbnail_url,
                            remote.song_count,
                            now
                        ],
                    )?;
                    id
                }
            };
            require_playlist(tx, &id)
        })
    }

    pub fn playlist(&self, id: &str) -> Result<Option<Playlist>> {
        self.read(|c| get_playlist(c, id))
    }

    pub fn playlist_by_browse_id(&self, browse_id: &str) -> Result<Option<Playlist>> {
        self.read(|c| {
            c.query_row(
                &format!("{PLAYLIST_SELECT} WHERE p.browse_id = ?1"),
                [browse_id],
                playlist_from_row,
            )
            .optional()
        })
    }

    pub fn playlists(&self, sort: PlaylistSort, descending: bool) -> Result<Vec<Playlist>> {
        let dir = if descending { "DESC" } else { "ASC" };
        let order = match sort {
            PlaylistSort::Name => format!("p.name COLLATE NOCASE {dir}"),
            PlaylistSort::Created => format!("p.created_at {dir}"),
            PlaylistSort::Updated => format!("p.updated_at {dir}"),
            PlaylistSort::SongCount => format!("7 {dir}"),
        };
        self.read(|c| {
            let mut stmt = c.prepare(&format!("{PLAYLIST_SELECT} ORDER BY {order}, p.id"))?;
            stmt.query_map([], playlist_from_row)?.collect()
        })
    }

    pub(crate) fn search_playlists(
        &self,
        pattern: &str,
        prefix: &str,
        limit: i64,
    ) -> Result<Vec<Playlist>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "{PLAYLIST_SELECT} WHERE p.name LIKE ?1 ESCAPE '\\'
                 ORDER BY (p.name LIKE ?2 ESCAPE '\\') DESC, p.name COLLATE NOCASE LIMIT ?3"
            ))?;
            stmt.query_map(params![pattern, prefix, limit], playlist_from_row)?
                .collect()
        })
    }

    pub fn rename_playlist(&self, id: &str, name: &str) -> Result<()> {
        self.write(|tx| {
            require_playlist(tx, id)?;
            tx.execute(
                "UPDATE playlist SET name = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, name, now_ms()],
            )?;
            Ok(())
        })
    }

    pub fn set_playlist_thumbnail(&self, id: &str, url: Option<&str>) -> Result<()> {
        self.write(|tx| {
            require_playlist(tx, id)?;
            tx.execute(
                "UPDATE playlist SET thumbnail_url = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, url, now_ms()],
            )?;
            Ok(())
        })
    }

    pub fn set_playlist_bookmarked(&self, id: &str, bookmarked: bool) -> Result<()> {
        self.read(|c| {
            c.execute(
                "UPDATE playlist SET bookmarked_at = CASE WHEN ?2 THEN COALESCE(bookmarked_at, ?3)
                                                          ELSE NULL END WHERE id = ?1",
                params![id, bookmarked, now_ms()],
            )
        })?;
        Ok(())
    }

    /// Deletes the playlist and its entries (songs stay in the library).
    pub fn delete_playlist(&self, id: &str) -> Result<()> {
        self.read(|c| c.execute("DELETE FROM playlist WHERE id = ?1", [id]))?;
        Ok(())
    }

    /// Entries in order.
    pub fn playlist_entries(&self, id: &str) -> Result<Vec<PlaylistEntry>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {SONG_COLS}, pe.id, pe.position, pe.set_video_id
                 FROM playlist_entry pe JOIN song s ON s.id = pe.song_id
                 WHERE pe.playlist_id = ?1 ORDER BY pe.position, pe.id"
            ))?;
            stmt.query_map([id], |r| {
                let n = SONG_COL_COUNT;
                Ok(PlaylistEntry {
                    song: song_from_row(r, 0)?,
                    entry_id: r.get(n)?,
                    position: r.get(n + 1)?,
                    set_video_id: r.get(n + 2)?,
                })
            })?
            .collect()
        })
    }

    /// Appends tracks (storing them first). Returns the new entry ids.
    pub fn add_to_playlist(&self, id: &str, tracks: &[Track]) -> Result<Vec<i64>> {
        let items: Vec<(Track, Option<String>)> =
            tracks.iter().map(|t| (t.clone(), None)).collect();
        self.insert_playlist_entries(id, None, &items)
    }

    /// Inserts `(track, setVideoId)` items at `at` (clamped; `None` appends). Returns entry ids.
    pub fn insert_playlist_entries(
        &self,
        id: &str,
        at: Option<usize>,
        items: &[(Track, Option<String>)],
    ) -> Result<Vec<i64>> {
        let now = now_ms();
        self.write(|tx| {
            require_playlist(tx, id)?;
            let len = entry_count(tx, id)? as usize;
            let at = at.unwrap_or(len).min(len);
            tx.execute(
                "UPDATE playlist_entry SET position = position + ?3
                 WHERE playlist_id = ?1 AND position >= ?2",
                params![id, at as i64, items.len() as i64],
            )?;
            let mut ids = Vec::with_capacity(items.len());
            for (i, (track, set_video_id)) in items.iter().enumerate() {
                upsert_track_tx(tx, track, None, now)?;
                tx.execute(
                    "INSERT INTO playlist_entry (playlist_id, song_id, position, set_video_id)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![id, track.video_id, (at + i) as i64, set_video_id],
                )?;
                ids.push(tx.last_insert_rowid());
            }
            touch(tx, id)?;
            Ok(ids)
        })
    }

    /// Replaces all entries (e.g. after fetching a YouTube playlist).
    pub fn replace_playlist_entries(
        &self,
        id: &str,
        items: &[(Track, Option<String>)],
    ) -> Result<Vec<i64>> {
        self.read(|c| c.execute("DELETE FROM playlist_entry WHERE playlist_id = ?1", [id]))?;
        self.insert_playlist_entries(id, None, items)
    }

    /// Removes one entry (by [`PlaylistEntry::entry_id`]); later entries move up.
    pub fn remove_playlist_entry(&self, entry_id: i64) -> Result<()> {
        self.write(|tx| {
            let playlist: Option<String> = tx
                .query_row(
                    "SELECT playlist_id FROM playlist_entry WHERE id = ?1",
                    [entry_id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(playlist) = playlist {
                tx.execute("DELETE FROM playlist_entry WHERE id = ?1", [entry_id])?;
                renumber(tx, &playlist)?;
                touch(tx, &playlist)?;
            }
            Ok(())
        })
    }

    /// Moves the entry at position `from` to position `to` (both clamped), shifting the
    /// entries in between.
    pub fn move_playlist_entry(&self, id: &str, from: usize, to: usize) -> Result<()> {
        self.write(|tx| {
            renumber(tx, id)?;
            let len = entry_count(tx, id)? as usize;
            if len == 0 {
                return Ok(());
            }
            let (from, to) = (from.min(len - 1) as i64, to.min(len - 1) as i64);
            if from == to {
                return Ok(());
            }
            let moved: i64 = tx.query_row(
                "SELECT id FROM playlist_entry WHERE playlist_id = ?1 AND position = ?2",
                params![id, from],
                |r| r.get(0),
            )?;
            if from < to {
                tx.execute(
                    "UPDATE playlist_entry SET position = position - 1
                     WHERE playlist_id = ?1 AND position > ?2 AND position <= ?3",
                    params![id, from, to],
                )?;
            } else {
                tx.execute(
                    "UPDATE playlist_entry SET position = position + 1
                     WHERE playlist_id = ?1 AND position >= ?3 AND position < ?2",
                    params![id, from, to],
                )?;
            }
            tx.execute(
                "UPDATE playlist_entry SET position = ?2 WHERE id = ?1",
                params![moved, to],
            )?;
            touch(tx, id)?;
            Ok(())
        })
    }

    /// Sets the YouTube `setVideoId` of an entry (after adding it remotely).
    pub fn set_playlist_entry_set_video_id(
        &self,
        entry_id: i64,
        set_video_id: Option<&str>,
    ) -> Result<()> {
        self.read(|c| {
            c.execute(
                "UPDATE playlist_entry SET set_video_id = ?2 WHERE id = ?1",
                params![entry_id, set_video_id],
            )
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::test_util::track;

    fn order(lib: &Library, id: &str) -> String {
        let entries = lib.playlist_entries(id).unwrap();
        for (i, e) in entries.iter().enumerate() {
            assert_eq!(e.position as usize, i);
        }
        entries
            .iter()
            .map(|e| e.song.track.video_id.as_str())
            .collect()
    }

    #[test]
    fn crud_and_ordering() {
        let lib = Library::open_in_memory().unwrap();
        let p = lib.create_playlist("Mix").unwrap();
        assert!(p.id.starts_with("LP") && p.editable && p.browse_id.is_none());
        let tracks: Vec<_> = "abcde"
            .chars()
            .map(|c| track(&c.to_string(), "T", "A", None))
            .collect();
        let ids = lib.add_to_playlist(&p.id, &tracks).unwrap();
        assert_eq!(order(&lib, &p.id), "abcde");
        assert_eq!(lib.playlist(&p.id).unwrap().unwrap().song_count, 5);

        lib.move_playlist_entry(&p.id, 0, 3).unwrap();
        assert_eq!(order(&lib, &p.id), "bcdae");
        lib.move_playlist_entry(&p.id, 4, 0).unwrap();
        assert_eq!(order(&lib, &p.id), "ebcda");
        lib.move_playlist_entry(&p.id, 2, 99).unwrap();
        assert_eq!(order(&lib, &p.id), "ebdac");
        lib.move_playlist_entry(&p.id, 1, 1).unwrap();
        assert_eq!(order(&lib, &p.id), "ebdac");

        lib.remove_playlist_entry(ids[3]).unwrap(); // "d"
        assert_eq!(order(&lib, &p.id), "ebac");
        lib.remove_playlist_entry(12345).unwrap();

        // Duplicates and inserting in the middle.
        lib.insert_playlist_entries(&p.id, Some(1), &[(tracks[0].clone(), Some("SET1".into()))])
            .unwrap();
        assert_eq!(order(&lib, &p.id), "eabac");
        let entries = lib.playlist_entries(&p.id).unwrap();
        assert_eq!(entries[1].set_video_id.as_deref(), Some("SET1"));
        lib.set_playlist_entry_set_video_id(entries[0].entry_id, Some("SET0"))
            .unwrap();
        assert_eq!(
            lib.playlist_entries(&p.id).unwrap()[0]
                .set_video_id
                .as_deref(),
            Some("SET0")
        );

        lib.rename_playlist(&p.id, "Renamed").unwrap();
        lib.set_playlist_thumbnail(&p.id, Some("https://t"))
            .unwrap();
        lib.set_playlist_bookmarked(&p.id, true).unwrap();
        let p2 = lib.playlist(&p.id).unwrap().unwrap();
        assert_eq!(p2.name, "Renamed");
        assert_eq!(p2.thumbnail_url.as_deref(), Some("https://t"));
        assert!(p2.bookmarked_at.is_some());
        assert!(p2.updated_at >= p.updated_at);
        assert!(lib.rename_playlist("missing", "x").is_err());
        assert!(lib.add_to_playlist("missing", &tracks).is_err());

        lib.delete_playlist(&p.id).unwrap();
        assert!(lib.playlist(&p.id).unwrap().is_none());
        // Songs survive.
        assert!(lib.song("a").unwrap().is_some());
    }

    #[test]
    fn remote_mirrors_and_listing() {
        let lib = Library::open_in_memory().unwrap();
        let remote = RemotePlaylist {
            browse_id: "PLxyz".into(),
            name: "Remote".into(),
            thumbnail_url: Some("https://r".into()),
            editable: false,
            song_count: Some(120),
        };
        let p = lib.upsert_remote_playlist(&remote).unwrap();
        assert_eq!(p.browse_id.as_deref(), Some("PLxyz"));
        assert!(!p.editable);
        assert_eq!(p.remote_song_count, Some(120));
        let again = lib
            .upsert_remote_playlist(&RemotePlaylist {
                name: "Remote 2".into(),
                thumbnail_url: None,
                ..remote.clone()
            })
            .unwrap();
        assert_eq!(again.id, p.id);
        assert_eq!(again.name, "Remote 2");
        assert_eq!(again.thumbnail_url.as_deref(), Some("https://r"));
        assert_eq!(
            lib.playlist_by_browse_id("PLxyz").unwrap().unwrap().id,
            p.id
        );

        let items = vec![
            (track("x", "X", "A", None), Some("s1".to_owned())),
            (track("y", "Y", "A", None), Some("s2".to_owned())),
        ];
        lib.replace_playlist_entries(&p.id, &items).unwrap();
        lib.replace_playlist_entries(&p.id, &items[..1]).unwrap();
        assert_eq!(order(&lib, &p.id), "x");

        std::thread::sleep(std::time::Duration::from_millis(3));
        let local = lib.create_playlist("a local").unwrap();
        lib.add_to_playlist(
            &local.id,
            &[track("q", "Q", "A", None), track("r", "R", "A", None)],
        )
        .unwrap();
        let names = |sort, desc| {
            lib.playlists(sort, desc)
                .unwrap()
                .into_iter()
                .map(|p| p.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(PlaylistSort::Name, false), ["a local", "Remote 2"]);
        assert_eq!(
            names(PlaylistSort::SongCount, true),
            ["a local", "Remote 2"]
        );
        assert_eq!(names(PlaylistSort::Created, false), ["Remote 2", "a local"]);
    }
}
