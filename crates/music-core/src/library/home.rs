// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/db/DatabaseDao.kt
// (forgottenFavorites, mostPlayedSongs, recentSongs, librarySongIds) — © Rukamori and
// ArchiveTune contributors, GPL-3.0

//! Queries behind the personal Home sections (ArchiveTune's `HomeViewModel`): songs you
//! loved and stopped playing, the most played songs of a period (by play count, as
//! ArchiveTune ranks them), the last played songs and the ids of the songs saved to the
//! library (quick picks leave those out).

use rusqlite::params;

use super::songs::{SONG_COL_COUNT, SONG_COLS, song_from_row};
use super::{Library, LibrarySong, SongStat, TimeRange};
use crate::Result;

const DAY_MS: i64 = 86_400_000;

/// When a song counts as a forgotten favourite.
///
/// ArchiveTune's rule: listening time in the last `recent_days` is below `ratio` of the
/// listening time before them. Its query also requires a play in the recent window; here
/// a song not played at all lately qualifies too (the clearest case of a forgotten
/// favourite), and it needs at least `min_plays` plays before the window so one old
/// play isn't a "favourite".
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ForgottenRule {
    /// The recent window (ArchiveTune: 30 days).
    pub recent_days: u32,
    /// Recent listening time must stay below this share of the older listening time
    /// (ArchiveTune: 0.2).
    pub ratio: f64,
    /// Plays before the window.
    pub min_plays: u32,
    /// Rows returned, most listened (before the window) first (ArchiveTune: 100).
    pub limit: u32,
}

impl Default for ForgottenRule {
    fn default() -> Self {
        Self {
            recent_days: 30,
            ratio: 0.2,
            min_plays: 3,
            limit: 100,
        }
    }
}

fn u(v: i64) -> u64 {
    v.max(0) as u64
}

impl Library {
    /// Songs listened to a lot before the last `rule.recent_days` and little or not at
    /// all since ([`ForgottenRule`]), most listened first.
    pub fn forgotten_favorites(&self, now_ms: i64, rule: &ForgottenRule) -> Result<Vec<SongStat>> {
        let cutoff = now_ms - i64::from(rule.recent_days) * DAY_MS;
        self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {SONG_COLS}, old.plays, old.ms
                 FROM (SELECT song_id, COUNT(*) AS plays, SUM(played_ms) AS ms
                         FROM play_event WHERE played_at < ?1 GROUP BY song_id) old
                 JOIN song s ON s.id = old.song_id
                 LEFT JOIN (SELECT song_id, SUM(played_ms) AS ms
                              FROM play_event WHERE played_at >= ?1 GROUP BY song_id) new
                        ON new.song_id = old.song_id
                 WHERE old.plays >= ?2 AND COALESCE(new.ms, 0) < ?3 * old.ms
                 ORDER BY old.ms DESC, old.plays DESC, s.id
                 LIMIT ?4"
            ))?;
            stmt.query_map(
                params![cutoff, rule.min_plays, rule.ratio, rule.limit],
                |r| {
                    Ok(SongStat {
                        song: song_from_row(r, 0)?,
                        plays: u(r.get(SONG_COL_COUNT)?),
                        play_ms: u(r.get(SONG_COL_COUNT + 1)?),
                    })
                },
            )?
            .collect()
        })
    }

    /// Songs played most often in `range` (play count, then listening time), skipping
    /// the first `offset` — ArchiveTune's `mostPlayedSongs` (Home's "Keep listening"
    /// skips the top five, which seed the quick picks).
    pub fn most_played_songs(
        &self,
        range: TimeRange,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<SongStat>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {SONG_COLS}, COUNT(*) AS plays, SUM(e.played_ms) AS ms
                 FROM play_event e JOIN song s ON s.id = e.song_id
                 WHERE e.played_at >= ?1 AND e.played_at < ?2
                 GROUP BY s.id ORDER BY plays DESC, ms DESC, s.id
                 LIMIT ?3 OFFSET ?4"
            ))?;
            stmt.query_map(params![range.from_ms, range.to_ms, limit, offset], |r| {
                Ok(SongStat {
                    song: song_from_row(r, 0)?,
                    plays: u(r.get(SONG_COL_COUNT)?),
                    play_ms: u(r.get(SONG_COL_COUNT + 1)?),
                })
            })?
            .collect()
        })
    }

    /// The last `limit` different songs played, most recent first.
    pub fn recent_songs(&self, limit: u32) -> Result<Vec<LibrarySong>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {SONG_COLS}
                 FROM (SELECT song_id, MAX(played_at) AS last, MAX(id) AS row FROM play_event
                        GROUP BY song_id ORDER BY last DESC, row DESC LIMIT ?1) r
                 JOIN song s ON s.id = r.song_id
                 ORDER BY r.last DESC, r.row DESC"
            ))?;
            stmt.query_map([limit], |r| song_from_row(r, 0))?.collect()
        })
    }

    /// Ids of the songs saved to the library (not just liked or played).
    pub fn library_song_ids(&self) -> Result<Vec<String>> {
        self.read(|c| {
            let mut stmt = c.prepare("SELECT id FROM song WHERE in_library_at IS NOT NULL")?;
            stmt.query_map([], |r| r.get(0))?.collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::test_util::track;

    const NOW: i64 = 1_760_000_000_000;

    fn plays(lib: &Library, id: &str, artist: &str, at: &[i64]) {
        let t = track(id, id, artist, None);
        for &at in at {
            lib.record_play(&t, 180_000, at).unwrap();
        }
    }

    fn ids(stats: &[SongStat]) -> Vec<&str> {
        stats
            .iter()
            .map(|s| s.song.track.video_id.as_str())
            .collect()
    }

    #[test]
    fn forgotten_favorites_thresholds() {
        let lib = Library::open_in_memory().unwrap();
        let old = |d: i64| NOW - d * DAY_MS;
        // Loved two months ago, silent since: forgotten.
        plays(
            &lib,
            "gone",
            "A",
            &[old(60), old(61), old(62), old(63), old(64)],
        );
        // Loved, played once lately: 1 recent play vs 4 old = 25 % → still a current song.
        plays(
            &lib,
            "kept",
            "B",
            &[old(40), old(41), old(42), old(43), old(2)],
        );
        // Many old plays, one recent: 1/6 ≈ 17 % < 20 % → forgotten.
        plays(
            &lib,
            "fading",
            "C",
            &[old(40), old(41), old(42), old(43), old(44), old(45), old(1)],
        );
        // Too few old plays to be a favourite.
        plays(&lib, "once", "D", &[old(90), old(91)]);
        // Only played lately.
        plays(&lib, "new", "E", &[old(1), old(2), old(3), old(4)]);
        // Exactly at the edge of the window counts as recent.
        plays(&lib, "edge", "F", &[old(30), old(30), old(30)]);

        let rule = ForgottenRule::default();
        let found = lib.forgotten_favorites(NOW, &rule).unwrap();
        // Most old listening first.
        assert_eq!(ids(&found), ["fading", "gone"]);
        assert_eq!(found[0].plays, 6, "plays before the window");

        let lenient = ForgottenRule {
            min_plays: 2,
            ratio: 0.3,
            ..rule
        };
        let found = lib.forgotten_favorites(NOW, &lenient).unwrap();
        assert_eq!(ids(&found), ["fading", "gone", "kept", "once"]);

        let short = ForgottenRule {
            recent_days: 7,
            ..rule
        };
        // With a one-week window the plays 30 days ago are "old" too.
        let found = lib.forgotten_favorites(NOW, &short).unwrap();
        assert_eq!(ids(&found), ["fading", "gone", "edge"]);
        let one = ForgottenRule { limit: 1, ..rule };
        assert_eq!(lib.forgotten_favorites(NOW, &one).unwrap().len(), 1);
    }

    #[test]
    fn most_played_recent_and_library_ids() {
        let lib = Library::open_in_memory().unwrap();
        plays(&lib, "a", "A", &[NOW - 5, NOW - 4, NOW - 3]);
        plays(&lib, "b", "B", &[NOW - 2]);
        plays(&lib, "c", "C", &[NOW - 9, NOW - 8]);
        plays(&lib, "old", "D", &[NOW - 40 * DAY_MS]);
        let range = TimeRange::last_days(14, NOW);
        let top = lib.most_played_songs(range, 10, 0).unwrap();
        assert_eq!(ids(&top), ["a", "c", "b"]);
        assert_eq!(
            ids(&lib.most_played_songs(range, 10, 1).unwrap()),
            ["c", "b"]
        );
        assert_eq!(ids(&lib.most_played_songs(range, 1, 2).unwrap()), ["b"]);

        let recent: Vec<_> = lib
            .recent_songs(3)
            .unwrap()
            .into_iter()
            .map(|s| s.track.video_id)
            .collect();
        // By the time of the last play.
        assert_eq!(recent, ["b", "a", "c"]);

        assert!(lib.library_song_ids().unwrap().is_empty());
        lib.add_to_library(&track("b", "b", "B", None)).unwrap();
        assert_eq!(lib.library_song_ids().unwrap(), ["b"]);
    }
}
