// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/db/DatabaseDao.kt and
// db/entities/{Event,SearchHistory,LyricsEntity}.kt — © Rukamori and ArchiveTune contributors, GPL-3.0

//! Play history, search history and the lyrics cache.

use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

use super::songs::{SONG_COL_COUNT, SONG_COLS, song_from_row, upsert_track_tx};
use super::{Library, LibrarySong, TimeZone, now_ms};
use crate::{Result, Track};

/// One play of a song.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    pub id: i64,
    pub song: LibrarySong,
    /// When playback started (Unix ms).
    pub played_at: i64,
    /// How long it was actually listened to.
    pub played_ms: u64,
    /// `YYYY-MM-DD` of `played_at` in the requested [`TimeZone`].
    pub day: String,
}

/// History entries of one calendar day, newest first.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryDay {
    pub day: String,
    pub entries: Vec<HistoryEntry>,
}

/// Groups consecutive entries (as returned by [`Library::history`]) by their `day`.
pub fn group_by_day(entries: Vec<HistoryEntry>) -> Vec<HistoryDay> {
    let mut days: Vec<HistoryDay> = Vec::new();
    for entry in entries {
        match days.last_mut() {
            Some(day) if day.day == entry.day => day.entries.push(entry),
            _ => days.push(HistoryDay {
                day: entry.day.clone(),
                entries: vec![entry],
            }),
        }
    }
    days
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHistoryEntry {
    pub query: String,
    pub searched_at: i64,
}

/// Lyrics text cached for a song from one provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedLyrics {
    pub video_id: String,
    /// Free-form provider name, e.g. `"lrclib"`, `"embedded"`.
    pub provider: String,
    /// Raw text as fetched (LRC or plain). Callers may store `""` to remember "not found".
    pub text: String,
    pub fetched_at: i64,
}

/// Recomputes the play aggregates on `song` from `play_event` for the given songs (all if `None`).
fn refresh_play_totals(c: &Connection, song_ids: Option<&[String]>) -> rusqlite::Result<()> {
    const SET: &str = "UPDATE song SET
        total_play_ms = (SELECT COALESCE(SUM(played_ms), 0) FROM play_event e WHERE e.song_id = song.id),
        play_count = (SELECT COUNT(*) FROM play_event e WHERE e.song_id = song.id),
        last_played_at = (SELECT MAX(played_at) FROM play_event e WHERE e.song_id = song.id)";
    match song_ids {
        None => c.execute(SET, []).map(|_| ()),
        Some(ids) => {
            let mut stmt = c.prepare(&format!("{SET} WHERE id = ?1"))?;
            for id in ids {
                stmt.execute([id])?;
            }
            Ok(())
        }
    }
}

impl Library {
    /// Records that `track` was listened to for `played_ms`, starting at `at` (Unix ms).
    /// Stores the track if needed and updates its play count, total time and last-played time.
    /// Returns the history entry id.
    pub fn record_play(&self, track: &Track, played_ms: u64, at: i64) -> Result<i64> {
        self.write(|tx| {
            upsert_track_tx(tx, track, None, now_ms())?;
            tx.execute(
                "INSERT INTO play_event (song_id, played_at, played_ms) VALUES (?1, ?2, ?3)",
                params![track.video_id, at, played_ms as i64],
            )?;
            let id = tx.last_insert_rowid();
            tx.execute(
                "UPDATE song SET total_play_ms = total_play_ms + ?2, play_count = play_count + 1,
                        last_played_at = MAX(COALESCE(last_played_at, ?3), ?3)
                 WHERE id = ?1",
                params![track.video_id, played_ms as i64, at],
            )?;
            Ok(id)
        })
    }

    /// Play history, newest first.
    pub fn history(&self, tz: TimeZone, limit: u32, offset: u32) -> Result<Vec<HistoryEntry>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {SONG_COLS}, e.id, e.played_at, e.played_ms,
                        strftime('%Y-%m-%d', e.played_at / 1000, 'unixepoch', ?3)
                 FROM play_event e JOIN song s ON s.id = e.song_id
                 ORDER BY e.played_at DESC, e.id DESC LIMIT ?1 OFFSET ?2"
            ))?;
            stmt.query_map(params![limit, offset, tz.utc_to_zone()], |r| {
                let n = SONG_COL_COUNT;
                Ok(HistoryEntry {
                    song: song_from_row(r, 0)?,
                    id: r.get(n)?,
                    played_at: r.get(n + 1)?,
                    played_ms: r.get::<_, i64>(n + 2)?.max(0) as u64,
                    day: r.get(n + 3)?,
                })
            })?
            .collect()
        })
    }

    /// [`Self::history`] grouped by day. A day can continue on the next page.
    pub fn history_by_day(&self, tz: TimeZone, limit: u32, offset: u32) -> Result<Vec<HistoryDay>> {
        Ok(group_by_day(self.history(tz, limit, offset)?))
    }

    /// Deletes history entries and adjusts the songs' play statistics.
    pub fn delete_history_entries(&self, ids: &[i64]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        self.write(|tx| {
            let placeholders = vec!["?"; ids.len()].join(",");
            let songs: Vec<String> = {
                let mut stmt = tx.prepare(&format!(
                    "SELECT DISTINCT song_id FROM play_event WHERE id IN ({placeholders})"
                ))?;
                stmt.query_map(params_from_iter(ids), |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?
            };
            tx.execute(
                &format!("DELETE FROM play_event WHERE id IN ({placeholders})"),
                params_from_iter(ids),
            )?;
            refresh_play_totals(tx, Some(&songs))?;
            Ok(())
        })
    }

    /// Removes every play of one song.
    pub fn delete_song_history(&self, video_id: &str) -> Result<()> {
        self.write(|tx| {
            tx.execute("DELETE FROM play_event WHERE song_id = ?1", [video_id])?;
            refresh_play_totals(tx, Some(&[video_id.to_owned()]))?;
            Ok(())
        })
    }

    /// Deletes all play history and resets play statistics.
    pub fn clear_history(&self) -> Result<()> {
        self.write(|tx| {
            tx.execute("DELETE FROM play_event", [])?;
            refresh_play_totals(tx, None)?;
            Ok(())
        })
    }

    // ---- search history --------------------------------------------------------------------

    /// Remembers a search; searching the same text again (in any ASCII letter case)
    /// moves it to the top, spelled as searched last.
    pub fn add_search_query(&self, query: &str) -> Result<()> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(());
        }
        // Strictly increasing timestamps keep the order stable within one millisecond.
        self.write(|tx| {
            tx.execute(
                "DELETE FROM search_history WHERE query = ?1 COLLATE NOCASE AND query <> ?1",
                [query],
            )?;
            tx.execute(
                "INSERT INTO search_history (query, searched_at)
                 VALUES (?1, MAX(?2, COALESCE((SELECT MAX(searched_at) + 1 FROM search_history), 0)))
                 ON CONFLICT(query) DO UPDATE SET searched_at = excluded.searched_at",
                params![query, now_ms()],
            )?;
            Ok(())
        })
    }

    /// Past searches starting with `prefix` (case-insensitive), most recent first.
    pub fn search_history(&self, prefix: &str, limit: u32) -> Result<Vec<SearchHistoryEntry>> {
        let mut pattern = super::like_pattern(prefix);
        pattern.remove(0);
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT query, searched_at FROM search_history WHERE query LIKE ?1 ESCAPE '\\'
                 ORDER BY searched_at DESC, id DESC LIMIT ?2",
            )?;
            stmt.query_map(params![pattern, limit], |r| {
                Ok(SearchHistoryEntry {
                    query: r.get(0)?,
                    searched_at: r.get(1)?,
                })
            })?
            .collect()
        })
    }

    /// Forgets a search (every ASCII letter-case variant, like [`Self::add_search_query`]).
    pub fn remove_search_query(&self, query: &str) -> Result<()> {
        self.read(|c| {
            c.execute(
                "DELETE FROM search_history WHERE query = ?1 COLLATE NOCASE",
                [query.trim()],
            )
        })?;
        Ok(())
    }

    pub fn clear_search_history(&self) -> Result<()> {
        self.read(|c| c.execute("DELETE FROM search_history", []))?;
        Ok(())
    }

    // ---- lyrics cache ----------------------------------------------------------------------

    /// Stores (replacing) lyrics for `video_id` from `provider`.
    pub fn cache_lyrics(&self, video_id: &str, provider: &str, text: &str) -> Result<()> {
        self.read(|c| {
            c.execute(
                "INSERT INTO lyrics (song_id, provider, text, fetched_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(song_id, provider) DO UPDATE SET text = excluded.text,
                                                              fetched_at = excluded.fetched_at",
                params![video_id, provider, text, now_ms()],
            )
        })?;
        Ok(())
    }

    /// Cached lyrics from `provider`, or the most recently fetched from any provider.
    pub fn cached_lyrics(
        &self,
        video_id: &str,
        provider: Option<&str>,
    ) -> Result<Option<CachedLyrics>> {
        self.read(|c| {
            c.query_row(
                "SELECT song_id, provider, text, fetched_at FROM lyrics
                 WHERE song_id = ?1 AND (?2 IS NULL OR provider = ?2)
                 ORDER BY fetched_at DESC LIMIT 1",
                params![video_id, provider],
                |r| {
                    Ok(CachedLyrics {
                        video_id: r.get(0)?,
                        provider: r.get(1)?,
                        text: r.get(2)?,
                        fetched_at: r.get(3)?,
                    })
                },
            )
            .optional()
        })
    }

    /// Drops cached lyrics from `provider` (or all providers).
    pub fn remove_cached_lyrics(&self, video_id: &str, provider: Option<&str>) -> Result<()> {
        self.read(|c| {
            c.execute(
                "DELETE FROM lyrics WHERE song_id = ?1 AND (?2 IS NULL OR provider = ?2)",
                params![video_id, provider],
            )
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::test_util::track;

    const DAY: i64 = 86_400_000;

    #[test]
    fn record_play_updates_totals_and_history() {
        let lib = Library::open_in_memory().unwrap();
        let a = track("a", "A", "X", None);
        let b = track("b", "B", "Y", None);
        let e1 = lib.record_play(&a, 1_000, 10 * DAY).unwrap();
        lib.record_play(&b, 2_000, 10 * DAY + 5).unwrap();
        let e3 = lib.record_play(&a, 3_000, 11 * DAY).unwrap();
        // An out-of-order (older) play doesn't move last_played_at back.
        lib.record_play(&a, 500, 9 * DAY).unwrap();

        let s = lib.song("a").unwrap().unwrap();
        assert_eq!((s.play_count, s.total_play_ms), (3, 4_500));
        assert_eq!(s.last_played_at, Some(11 * DAY));

        let tz = TimeZone::Offset(0);
        let h = lib.history(tz, 10, 0).unwrap();
        assert_eq!(
            h.iter()
                .map(|e| e.song.track.video_id.as_str())
                .collect::<String>(),
            "abaa"
        );
        assert_eq!(h[0].id, e3);
        assert_eq!(h[0].day, "1970-01-12");
        assert_eq!(lib.history(tz, 2, 1).unwrap().len(), 2);

        let days = lib.history_by_day(tz, 10, 0).unwrap();
        assert_eq!(
            days.iter()
                .map(|d| (d.day.as_str(), d.entries.len()))
                .collect::<Vec<_>>(),
            [("1970-01-12", 1), ("1970-01-11", 2), ("1970-01-10", 1)]
        );
        // Day boundaries follow the time zone: at UTC-1 the midnight plays fall on the day before.
        let shifted = lib.history_by_day(TimeZone::Offset(-3600), 10, 0).unwrap();
        assert_eq!(shifted[1].day, "1970-01-10");

        lib.delete_history_entries(&[e1, e3]).unwrap();
        let s = lib.song("a").unwrap().unwrap();
        assert_eq!((s.play_count, s.total_play_ms), (1, 500));
        assert_eq!(s.last_played_at, Some(9 * DAY));

        lib.delete_song_history("b").unwrap();
        assert_eq!(lib.song("b").unwrap().unwrap().play_count, 0);
        lib.clear_history().unwrap();
        assert!(lib.history(tz, 10, 0).unwrap().is_empty());
        let s = lib.song("a").unwrap().unwrap();
        assert_eq!(
            (s.play_count, s.total_play_ms, s.last_played_at),
            (0, 0, None)
        );
        lib.delete_history_entries(&[]).unwrap();
    }

    #[test]
    fn search_history() {
        let lib = Library::open_in_memory().unwrap();
        for q in ["daft punk", "Dua Lipa", "  ", "justice", "daft punk"] {
            lib.add_search_query(q).unwrap();
        }
        let all: Vec<_> = lib
            .search_history("", 10)
            .unwrap()
            .into_iter()
            .map(|e| e.query)
            .collect();
        assert_eq!(all, ["daft punk", "justice", "Dua Lipa"]);
        let d: Vec<_> = lib
            .search_history("d", 10)
            .unwrap()
            .into_iter()
            .map(|e| e.query)
            .collect();
        assert_eq!(d, ["daft punk", "Dua Lipa"]);
        assert_eq!(lib.search_history("", 1).unwrap().len(), 1);
        assert!(lib.search_history("100%", 10).unwrap().is_empty());
        lib.remove_search_query("justice").unwrap();
        assert_eq!(lib.search_history("", 10).unwrap().len(), 2);
        // Letter case: one entry, spelled as searched last; removal ignores case too.
        lib.add_search_query("DAFT PUNK").unwrap();
        let all: Vec<_> = lib
            .search_history("", 10)
            .unwrap()
            .into_iter()
            .map(|e| e.query)
            .collect();
        assert_eq!(all, ["DAFT PUNK", "Dua Lipa"]);
        lib.remove_search_query("dua lipa").unwrap();
        assert_eq!(lib.search_history("", 10).unwrap().len(), 1);
        lib.clear_search_history().unwrap();
        assert!(lib.search_history("", 10).unwrap().is_empty());
    }

    #[test]
    fn lyrics_cache() {
        let lib = Library::open_in_memory().unwrap();
        assert_eq!(lib.cached_lyrics("v", None).unwrap(), None);
        lib.cache_lyrics("v", "lrclib", "[00:01.00] hi").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(3));
        lib.cache_lyrics("v", "embedded", "plain").unwrap();
        assert_eq!(
            lib.cached_lyrics("v", None).unwrap().unwrap().provider,
            "embedded"
        );
        let l = lib.cached_lyrics("v", Some("lrclib")).unwrap().unwrap();
        assert_eq!(l.text, "[00:01.00] hi");
        lib.cache_lyrics("v", "lrclib", "").unwrap();
        assert_eq!(
            lib.cached_lyrics("v", Some("lrclib"))
                .unwrap()
                .unwrap()
                .text,
            ""
        );
        lib.remove_cached_lyrics("v", Some("lrclib")).unwrap();
        assert!(lib.cached_lyrics("v", Some("lrclib")).unwrap().is_none());
        lib.remove_cached_lyrics("v", None).unwrap();
        assert!(lib.cached_lyrics("v", None).unwrap().is_none());
    }
}
