// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/db/DatabaseDao.kt
// (mostPlayed*, listeningBy*, listeningTotals) and viewmodels/YearInMusicViewModel.kt
// — © Rukamori and ArchiveTune contributors, GPL-3.0

//! Listening statistics over `play_event`: top songs/artists/albums, totals, listening by
//! hour/weekday/day/month and a "year in music" summary.
//!
//! A play counts in full for the moment it started (as in ArchiveTune); plays of songs with
//! several artists count for each artist.

use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::songs::{SONG_COL_COUNT, SONG_COLS, song_from_row};
use super::{Library, LibraryAlbum, LibraryArtist, LibrarySong};
use crate::Result;

/// How timestamps map to calendar days and hours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TimeZone {
    /// The system's local time zone (SQLite `localtime`, i.e. the C library's).
    #[default]
    Local,
    /// A fixed offset from UTC in seconds (east positive).
    Offset(i32),
}

impl TimeZone {
    /// SQLite modifier converting a UTC time to this zone.
    pub(crate) fn utc_to_zone(self) -> String {
        match self {
            TimeZone::Local => "localtime".into(),
            TimeZone::Offset(s) => format!("{s:+} seconds"),
        }
    }

    /// SQLite modifier converting a time in this zone to UTC.
    fn zone_to_utc(self) -> String {
        match self {
            TimeZone::Local => "utc".into(),
            TimeZone::Offset(s) => format!("{:+} seconds", -i64::from(s)),
        }
    }
}

/// Half-open interval `[from_ms, to_ms)` of Unix milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRange {
    pub from_ms: i64,
    pub to_ms: i64,
}

impl TimeRange {
    pub const ALL: TimeRange = TimeRange {
        from_ms: i64::MIN,
        to_ms: i64::MAX,
    };

    pub fn new(from_ms: i64, to_ms: i64) -> Self {
        Self { from_ms, to_ms }
    }

    /// The `days` × 24 h ending at `now_ms` (rolling window, e.g. "last 7 days").
    pub fn last_days(days: u32, now_ms: i64) -> Self {
        Self {
            from_ms: now_ms - i64::from(days) * 86_400_000,
            to_ms: now_ms.saturating_add(1),
        }
    }
}

impl Default for TimeRange {
    fn default() -> Self {
        Self::ALL
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SongStat {
    pub song: LibrarySong,
    pub plays: u64,
    pub play_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArtistStat {
    pub artist: LibraryArtist,
    pub plays: u64,
    pub play_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AlbumStat {
    pub album: LibraryAlbum,
    pub plays: u64,
    pub play_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ListeningTotals {
    pub plays: u64,
    pub play_ms: u64,
    pub unique_songs: u64,
    pub unique_artists: u64,
    pub unique_albums: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayListening {
    /// `YYYY-MM-DD`.
    pub date: String,
    pub plays: u64,
    pub play_ms: u64,
}

/// "Year in music" recap (ArchiveTune's `YearInMusicScreen`).
#[derive(Debug, Clone, PartialEq)]
pub struct YearInMusic {
    pub year: i32,
    pub range: TimeRange,
    pub totals: ListeningTotals,
    pub top_songs: Vec<SongStat>,
    pub top_artists: Vec<ArtistStat>,
    pub top_albums: Vec<AlbumStat>,
    /// Listening ms per month, January first.
    pub by_month: [u64; 12],
    /// Listening ms per hour of day.
    pub by_hour: [u64; 24],
    /// Hour with the most listening.
    pub top_hour: Option<u8>,
    /// Day with the most listening.
    pub busiest_day: Option<DayListening>,
}

fn u(v: i64) -> u64 {
    v.max(0) as u64
}

impl Library {
    /// `[Jan 1 00:00, next Jan 1 00:00)` of `year` in `tz`.
    pub fn year_range(&self, year: i32, tz: TimeZone) -> Result<TimeRange> {
        self.read(|c| {
            c.query_row(
                "SELECT CAST(strftime('%s', printf('%04d-01-01 00:00:00', ?1), ?2) AS INTEGER) * 1000,
                        CAST(strftime('%s', printf('%04d-01-01 00:00:00', ?1 + 1), ?2) AS INTEGER) * 1000",
                params![year, tz.zone_to_utc()],
                |r| Ok(TimeRange::new(r.get(0)?, r.get(1)?)),
            )
        })
    }

    /// Years (in `tz`) that have history, newest first.
    pub fn available_years(&self, tz: TimeZone) -> Result<Vec<i32>> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT DISTINCT CAST(strftime('%Y', played_at / 1000, 'unixepoch', ?1) AS INTEGER) AS y
                 FROM play_event ORDER BY y DESC",
            )?;
            stmt.query_map([tz.utc_to_zone()], |r| r.get(0))?.collect()
        })
    }

    /// Most listened songs in `range` (by listening time, then play count).
    pub fn top_songs(&self, range: TimeRange, limit: u32) -> Result<Vec<SongStat>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {SONG_COLS}, COUNT(*) AS plays, SUM(e.played_ms) AS ms
                 FROM play_event e JOIN song s ON s.id = e.song_id
                 WHERE e.played_at >= ?1 AND e.played_at < ?2
                 GROUP BY s.id ORDER BY ms DESC, plays DESC, s.title COLLATE NOCASE, s.id
                 LIMIT ?3"
            ))?;
            stmt.query_map(params![range.from_ms, range.to_ms, limit], |r| {
                Ok(SongStat {
                    song: song_from_row(r, 0)?,
                    plays: u(r.get(SONG_COL_COUNT)?),
                    play_ms: u(r.get(SONG_COL_COUNT + 1)?),
                })
            })?
            .collect()
        })
    }

    /// Most listened artists in `range`.
    pub fn top_artists(&self, range: TimeRange, limit: u32) -> Result<Vec<ArtistStat>> {
        let rows: Vec<(String, i64, i64)> = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT a.id, COUNT(*) AS plays, SUM(e.played_ms) AS ms
                 FROM play_event e JOIN song_artist sa ON sa.song_id = e.song_id
                      JOIN artist a ON a.id = sa.artist_id
                 WHERE e.played_at >= ?1 AND e.played_at < ?2
                 GROUP BY a.id ORDER BY ms DESC, plays DESC, a.name COLLATE NOCASE, a.id
                 LIMIT ?3",
            )?;
            stmt.query_map(params![range.from_ms, range.to_ms, limit], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect()
        })?;
        let mut out = Vec::with_capacity(rows.len());
        for (id, plays, ms) in rows {
            if let Some(artist) = self.artist(&id)? {
                out.push(ArtistStat {
                    artist,
                    plays: u(plays),
                    play_ms: u(ms),
                });
            }
        }
        Ok(out)
    }

    /// Most listened albums in `range`.
    pub fn top_albums(&self, range: TimeRange, limit: u32) -> Result<Vec<AlbumStat>> {
        let rows: Vec<(String, i64, i64)> = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT al.id, COUNT(*) AS plays, SUM(e.played_ms) AS ms
                 FROM play_event e JOIN song_album sa ON sa.song_id = e.song_id
                      JOIN album al ON al.id = sa.album_id
                 WHERE e.played_at >= ?1 AND e.played_at < ?2
                 GROUP BY al.id ORDER BY ms DESC, plays DESC, al.title COLLATE NOCASE, al.id
                 LIMIT ?3",
            )?;
            stmt.query_map(params![range.from_ms, range.to_ms, limit], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect()
        })?;
        let mut out = Vec::with_capacity(rows.len());
        for (id, plays, ms) in rows {
            if let Some(album) = self.album(&id)? {
                out.push(AlbumStat {
                    album,
                    plays: u(plays),
                    play_ms: u(ms),
                });
            }
        }
        Ok(out)
    }

    pub fn listening_totals(&self, range: TimeRange) -> Result<ListeningTotals> {
        self.read(|c| {
            c.query_row(
                "SELECT COUNT(*), COALESCE(SUM(played_ms), 0), COUNT(DISTINCT song_id),
                    (SELECT COUNT(DISTINCT sa.artist_id) FROM play_event e
                       JOIN song_artist sa ON sa.song_id = e.song_id
                      WHERE e.played_at >= ?1 AND e.played_at < ?2),
                    (SELECT COUNT(DISTINCT sa.album_id) FROM play_event e
                       JOIN song_album sa ON sa.song_id = e.song_id
                      WHERE e.played_at >= ?1 AND e.played_at < ?2)
                 FROM play_event WHERE played_at >= ?1 AND played_at < ?2",
                params![range.from_ms, range.to_ms],
                |r| {
                    Ok(ListeningTotals {
                        plays: u(r.get(0)?),
                        play_ms: u(r.get(1)?),
                        unique_songs: u(r.get(2)?),
                        unique_artists: u(r.get(3)?),
                        unique_albums: u(r.get(4)?),
                    })
                },
            )
        })
    }

    /// `(slot, ms)` sums of `strftime(format)` over `range`, e.g. `%H` for hours.
    fn listening_by(
        &self,
        format: &str,
        range: TimeRange,
        tz: TimeZone,
    ) -> Result<Vec<(String, i64, i64)>> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT strftime(?3, played_at / 1000, 'unixepoch', ?4) AS slot,
                        COUNT(*), COALESCE(SUM(played_ms), 0)
                 FROM play_event WHERE played_at >= ?1 AND played_at < ?2
                 GROUP BY slot ORDER BY slot",
            )?;
            stmt.query_map(
                params![range.from_ms, range.to_ms, format, tz.utc_to_zone()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?
            .collect()
        })
    }

    fn listening_slots<const N: usize>(
        &self,
        format: &str,
        offset: usize,
        range: TimeRange,
        tz: TimeZone,
    ) -> Result<[u64; N]> {
        let mut out = [0u64; N];
        for (slot, _, ms) in self.listening_by(format, range, tz)? {
            if let Some(i) = slot
                .parse::<usize>()
                .ok()
                .and_then(|s| s.checked_sub(offset))
                .filter(|&i| i < N)
            {
                out[i] = u(ms);
            }
        }
        Ok(out)
    }

    /// Listening ms per hour of day (0–23) in `tz`.
    pub fn listening_by_hour(&self, range: TimeRange, tz: TimeZone) -> Result<[u64; 24]> {
        self.listening_slots("%H", 0, range, tz)
    }

    /// Listening ms per weekday, Sunday first (0 = Sunday … 6 = Saturday).
    pub fn listening_by_weekday(&self, range: TimeRange, tz: TimeZone) -> Result<[u64; 7]> {
        self.listening_slots("%w", 0, range, tz)
    }

    /// Listening ms per month of year (index 0 = January), across all years in `range`.
    pub fn listening_by_month(&self, range: TimeRange, tz: TimeZone) -> Result<[u64; 12]> {
        self.listening_slots("%m", 1, range, tz)
    }

    /// Days with listening in `range`, oldest first.
    pub fn listening_by_day(&self, range: TimeRange, tz: TimeZone) -> Result<Vec<DayListening>> {
        Ok(self
            .listening_by("%Y-%m-%d", range, tz)?
            .into_iter()
            .map(|(date, plays, ms)| DayListening {
                date,
                plays: u(plays),
                play_ms: u(ms),
            })
            .collect())
    }

    /// Recap of `year` in `tz` with the top `limit` songs, artists and albums.
    pub fn year_in_music(&self, year: i32, tz: TimeZone, limit: u32) -> Result<YearInMusic> {
        let range = self.year_range(year, tz)?;
        let by_hour = self.listening_by_hour(range, tz)?;
        let top_hour = by_hour
            .iter()
            .enumerate()
            .filter(|(_, ms)| **ms > 0)
            .max_by_key(|(h, ms)| (**ms, std::cmp::Reverse(*h)))
            .map(|(h, _)| h as u8);
        let busiest_day = self
            .listening_by_day(range, tz)?
            .into_iter()
            .max_by(|a, b| a.play_ms.cmp(&b.play_ms).then(b.date.cmp(&a.date)));
        Ok(YearInMusic {
            year,
            range,
            totals: self.listening_totals(range)?,
            top_songs: self.top_songs(range, limit)?,
            top_artists: self.top_artists(range, limit)?,
            top_albums: self.top_albums(range, limit)?,
            by_month: self.listening_by_month(range, tz)?,
            by_hour,
            top_hour,
            busiest_day,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::test_util::track;

    const H: i64 = 3_600_000;
    const DAY: i64 = 24 * H;
    /// 2024-01-01T00:00:00Z.
    const Y2024: i64 = 1_704_067_200_000;
    /// 2025-01-01T00:00:00Z.
    const Y2025: i64 = 1_735_689_600_000;

    fn seeded() -> Library {
        let lib = Library::open_in_memory().unwrap();
        let a = track("a", "Alpha", "Ann", Some(("MPREb_1", "First")));
        let b = track("b", "Beta", "Bob", Some(("MPREb_1", "First")));
        let c = track("c", "Gamma", "Ann", Some(("MPREb_2", "Second")));
        // 2024: a 3× (Mon Jan 1 09:00, 09:30; Feb 3 21:00), b 1× long, c 1×.
        lib.record_play(&a, 60_000, Y2024 + 9 * H).unwrap();
        lib.record_play(&a, 60_000, Y2024 + 9 * H + H / 2).unwrap();
        lib.record_play(&a, 60_000, Y2024 + 33 * DAY + 21 * H)
            .unwrap();
        lib.record_play(&b, 200_000, Y2024 + 33 * DAY + 22 * H)
            .unwrap();
        lib.record_play(&c, 10_000, Y2024 + 40 * DAY).unwrap();
        // 2025 and 2023.
        lib.record_play(&c, 99_000, Y2025 + H).unwrap();
        lib.record_play(&b, 1_000, Y2024 - H).unwrap();
        lib
    }

    #[test]
    fn year_range_follows_time_zone() {
        let lib = Library::open_in_memory().unwrap();
        assert_eq!(
            lib.year_range(2024, TimeZone::Offset(0)).unwrap(),
            TimeRange::new(Y2024, Y2025)
        );
        assert_eq!(
            lib.year_range(2024, TimeZone::Offset(3600)).unwrap(),
            TimeRange::new(Y2024 - H, Y2025 - H)
        );
        assert_eq!(
            lib.year_range(2024, TimeZone::Offset(-5 * 3600)).unwrap(),
            TimeRange::new(Y2024 + 5 * H, Y2025 + 5 * H)
        );
        let local = lib.year_range(2024, TimeZone::Local).unwrap();
        assert!((local.from_ms - Y2024).abs() <= 14 * H);
    }

    #[test]
    fn tops_and_totals() {
        let lib = seeded();
        let range = TimeRange::new(Y2024, Y2025);
        let songs = lib.top_songs(range, 10).unwrap();
        let summary: Vec<_> = songs
            .iter()
            .map(|s| (s.song.track.video_id.as_str(), s.plays, s.play_ms))
            .collect();
        assert_eq!(
            summary,
            [("b", 1, 200_000), ("a", 3, 180_000), ("c", 1, 10_000)]
        );
        assert_eq!(lib.top_songs(range, 1).unwrap().len(), 1);

        let artists: Vec<_> = lib
            .top_artists(range, 10)
            .unwrap()
            .into_iter()
            .map(|a| (a.artist.name, a.plays, a.play_ms))
            .collect();
        assert_eq!(
            artists,
            [
                ("Bob".to_owned(), 1, 200_000),
                ("Ann".to_owned(), 4, 190_000)
            ]
        );

        let albums: Vec<_> = lib
            .top_albums(range, 10)
            .unwrap()
            .into_iter()
            .map(|a| (a.album.meta.title, a.plays, a.play_ms))
            .collect();
        assert_eq!(
            albums,
            [
                ("First".to_owned(), 4, 380_000),
                ("Second".to_owned(), 1, 10_000)
            ]
        );

        assert_eq!(
            lib.listening_totals(range).unwrap(),
            ListeningTotals {
                plays: 5,
                play_ms: 390_000,
                unique_songs: 3,
                unique_artists: 2,
                unique_albums: 2,
            }
        );
        assert_eq!(lib.listening_totals(TimeRange::ALL).unwrap().plays, 7);
        assert_eq!(
            lib.listening_totals(TimeRange::new(0, 1)).unwrap(),
            ListeningTotals::default()
        );
        assert!(lib.top_songs(TimeRange::new(0, 1), 5).unwrap().is_empty());
    }

    #[test]
    fn listening_by_hour_day_weekday_month() {
        let lib = seeded();
        let range = TimeRange::new(Y2024, Y2025);
        let utc = TimeZone::Offset(0);
        let hours = lib.listening_by_hour(range, utc).unwrap();
        assert_eq!(hours[9], 120_000);
        assert_eq!(hours[21], 60_000);
        assert_eq!(hours[22], 200_000);
        assert_eq!(hours[0], 10_000);
        assert_eq!(hours.iter().sum::<u64>(), 390_000);
        // Shifting the zone shifts the hours.
        let plus2 = lib
            .listening_by_hour(range, TimeZone::Offset(7200))
            .unwrap();
        assert_eq!(plus2[11], 120_000);
        assert_eq!(plus2[0], 200_000);

        // 2024-01-01 was a Monday, 2024-02-03 a Saturday, 2024-02-10 a Saturday.
        let weekdays = lib.listening_by_weekday(range, utc).unwrap();
        assert_eq!(weekdays, [0, 120_000, 0, 0, 0, 0, 270_000]);

        let months = lib.listening_by_month(range, utc).unwrap();
        assert_eq!(months[0], 120_000);
        assert_eq!(months[1], 270_000);
        assert_eq!(months[2..].iter().sum::<u64>(), 0);

        let days = lib.listening_by_day(range, utc).unwrap();
        assert_eq!(
            days.iter()
                .map(|d| (d.date.as_str(), d.plays, d.play_ms))
                .collect::<Vec<_>>(),
            [
                ("2024-01-01", 2, 120_000),
                ("2024-02-03", 2, 260_000),
                ("2024-02-10", 1, 10_000)
            ]
        );
    }

    #[test]
    fn year_in_music() {
        let lib = seeded();
        let utc = TimeZone::Offset(0);
        assert_eq!(lib.available_years(utc).unwrap(), [2025, 2024, 2023]);
        let recap = lib.year_in_music(2024, utc, 2).unwrap();
        assert_eq!(recap.year, 2024);
        assert_eq!(recap.totals.plays, 5);
        assert_eq!(recap.top_songs.len(), 2);
        assert_eq!(recap.top_songs[0].song.track.video_id, "b");
        assert_eq!(recap.top_artists[0].artist.name, "Bob");
        assert_eq!(recap.top_albums[0].album.meta.title, "First");
        assert_eq!(recap.top_hour, Some(22));
        assert_eq!(recap.busiest_day.as_ref().unwrap().date, "2024-02-03");
        assert_eq!(recap.by_month[1], 270_000);

        let empty = lib.year_in_music(2019, utc, 5).unwrap();
        assert_eq!(empty.totals, ListeningTotals::default());
        assert_eq!(empty.top_hour, None);
        assert_eq!(empty.busiest_day, None);
        assert!(empty.top_songs.is_empty());
        // The local zone works too (values depend on the machine's zone).
        lib.year_in_music(2024, TimeZone::Local, 5).unwrap();
    }

    #[test]
    fn last_days_range() {
        let r = TimeRange::last_days(7, 10 * DAY);
        assert_eq!(r, TimeRange::new(3 * DAY, 10 * DAY + 1));
    }
}
