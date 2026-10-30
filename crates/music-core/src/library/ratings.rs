//! Song ratings (like / dislike / neither) and the ones still owed to YouTube Music.
//!
//! A like is `song.liked_at`, a dislike `song.disliked_at` (schema v2); a song has
//! at most one of them. A rating the signed-in account couldn't send (offline, a
//! 5xx) waits in `pending_rating`, one row per song holding the latest wish, until
//! [`Library::push_pending_ratings`] delivers it. Library syncs consult the queue so
//! that a rating made offline isn't undone by YouTube's older state.

use std::collections::{HashMap, HashSet};

use rusqlite::{OptionalExtension, params};

use super::{Library, now_ms, upsert_track_tx};
use crate::innertube::{InnerTube, Rating};
use crate::{Result, Track};

/// Failed deliveries after which a queued rating is dropped.
pub const MAX_RATING_ATTEMPTS: u32 = 20;

/// A rating waiting to reach YouTube Music.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRating {
    pub video_id: String,
    pub rating: Rating,
    pub queued_at: i64,
    pub attempts: u32,
    pub last_error: Option<String>,
}

/// What [`Library::push_pending_ratings`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RatingPush {
    /// Delivered and removed from the queue.
    pub sent: usize,
    /// Dropped after [`MAX_RATING_ATTEMPTS`] failures.
    pub dropped: usize,
    /// Still queued.
    pub left: usize,
    /// The first error (the push stops there: offline, every request would fail).
    pub error: Option<String>,
    /// The push stopped because YouTube rejected the sign-in; nothing was counted as
    /// an attempt. Retry once the session works again.
    pub auth_failed: bool,
}

impl Library {
    /// The song's local rating (`Indifferent` for unknown songs).
    pub fn rating(&self, video_id: &str) -> Result<Rating> {
        let row: Option<(Option<i64>, Option<i64>)> = self.read(|c| {
            c.query_row(
                "SELECT liked_at, disliked_at FROM song WHERE id = ?1",
                [video_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
        })?;
        Ok(match row {
            Some((Some(_), _)) => Rating::Like,
            Some((None, Some(_))) => Rating::Dislike,
            _ => Rating::Indifferent,
        })
    }

    /// Rates `track` (storing it first): a like as [`Library::like`] (also adds it to
    /// the library), a dislike unlikes it, `Indifferent` clears both.
    pub fn set_rating(&self, track: &Track, rating: Rating) -> Result<()> {
        match rating {
            Rating::Like => self.like(track),
            Rating::Dislike => self.write(|tx| {
                let now = now_ms();
                upsert_track_tx(tx, track, None, now)?;
                tx.execute(
                    "UPDATE song SET liked_at = NULL, disliked_at = COALESCE(disliked_at, ?2)
                     WHERE id = ?1",
                    params![track.video_id, now],
                )?;
                Ok(())
            }),
            Rating::Indifferent => {
                self.read(|c| {
                    c.execute(
                        "UPDATE song SET liked_at = NULL, disliked_at = NULL WHERE id = ?1",
                        [&track.video_id],
                    )
                })?;
                Ok(())
            }
        }
    }

    /// Video ids of every disliked song.
    pub fn disliked_ids(&self) -> Result<HashSet<String>> {
        self.read(|c| {
            let mut stmt = c.prepare("SELECT id FROM song WHERE disliked_at IS NOT NULL")?;
            stmt.query_map([], |r| r.get(0))?.collect()
        })
    }

    // ---- ratings owed to YouTube Music ---------------------------------------------

    /// Queues `rating` for `video_id`, replacing an older queued one (attempts reset).
    pub fn queue_rating(&self, video_id: &str, rating: Rating) -> Result<()> {
        self.read(|c| {
            c.execute(
                "INSERT INTO pending_rating (song_id, rating, queued_at, attempts, last_error)
                 VALUES (?1, ?2, ?3, 0, NULL)
                 ON CONFLICT(song_id) DO UPDATE SET rating = excluded.rating,
                    queued_at = excluded.queued_at, attempts = 0, last_error = NULL",
                params![video_id, rating.as_str(), now_ms()],
            )
        })?;
        Ok(())
    }

    /// Queued ratings, oldest first.
    pub fn pending_ratings(&self) -> Result<Vec<PendingRating>> {
        let rows: Vec<(String, String, i64, u32, Option<String>)> = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT song_id, rating, queued_at, attempts, last_error FROM pending_rating
                 ORDER BY queued_at, song_id",
            )?;
            stmt.query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect()
        })?;
        Ok(rows
            .into_iter()
            .filter_map(|(video_id, rating, queued_at, attempts, last_error)| {
                Some(PendingRating {
                    video_id,
                    rating: Rating::parse(&rating)?,
                    queued_at,
                    attempts,
                    last_error,
                })
            })
            .collect())
    }

    /// Queued ratings by video id (what a sync must not undo).
    pub fn pending_rating_map(&self) -> Result<HashMap<String, Rating>> {
        Ok(self
            .pending_ratings()?
            .into_iter()
            .map(|p| (p.video_id, p.rating))
            .collect())
    }

    /// Removes the queued rating of `video_id` if it is still `rating` (a newer wish
    /// queued meanwhile stays). `None` removes whatever is queued.
    pub fn finish_pending_rating(&self, video_id: &str, rating: Option<Rating>) -> Result<()> {
        self.read(|c| match rating {
            Some(r) => c.execute(
                "DELETE FROM pending_rating WHERE song_id = ?1 AND rating = ?2",
                params![video_id, r.as_str()],
            ),
            None => c.execute("DELETE FROM pending_rating WHERE song_id = ?1", [video_id]),
        })?;
        Ok(())
    }

    /// Records a failed delivery; drops the entry after [`MAX_RATING_ATTEMPTS`].
    /// Returns whether it was dropped.
    pub fn pending_rating_failed(&self, video_id: &str, error: &str) -> Result<bool> {
        self.write(|tx| {
            tx.execute(
                "UPDATE pending_rating SET attempts = attempts + 1, last_error = ?2
                 WHERE song_id = ?1",
                params![video_id, error],
            )?;
            let dropped = tx.execute(
                "DELETE FROM pending_rating WHERE song_id = ?1 AND attempts >= ?2",
                params![video_id, MAX_RATING_ATTEMPTS],
            )?;
            Ok(dropped > 0)
        })
    }

    /// Forgets every queued rating (sign-out, another account: they were that
    /// account's).
    pub fn clear_pending_ratings(&self) -> Result<()> {
        self.read(|c| c.execute("DELETE FROM pending_rating", []))?;
        Ok(())
    }

    /// Sends queued ratings with `it` (signed in), oldest first, at most `limit`.
    /// Stops at the first failure (counted as an attempt, unless it was a rejected
    /// sign-in: see [`RatingPush::auth_failed`]).
    pub fn push_pending_ratings(&self, it: &InnerTube, limit: usize) -> Result<RatingPush> {
        let pending = self.pending_ratings()?;
        let mut report = RatingPush::default();
        for p in pending.iter().take(limit) {
            match it.rate_song(&p.video_id, p.rating) {
                Ok(()) => {
                    self.finish_pending_rating(&p.video_id, Some(p.rating))?;
                    report.sent += 1;
                }
                Err(e) => {
                    let error = e.to_string();
                    if e.is_auth() {
                        // Not this rating's fault: it waits for a working session.
                        report.auth_failed = true;
                    } else if self.pending_rating_failed(&p.video_id, &error)? {
                        report.dropped += 1;
                    }
                    report.error = Some(error);
                    break;
                }
            }
        }
        report.left = self.pending_ratings()?.len();
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::test_util::track;
    use crate::library::{SongQuery, SongScope};
    use crate::testutil::{Reply, TestServer};

    fn liked_ids(lib: &Library) -> Vec<String> {
        lib.songs(&SongQuery {
            scope: SongScope::Liked,
            ..Default::default()
        })
        .unwrap()
        .into_iter()
        .map(|s| s.track.video_id)
        .collect()
    }

    #[test]
    fn dislike_and_like_exclude_each_other() {
        let lib = Library::open_in_memory().unwrap();
        let t = track("v1", "Song", "Artist", None);
        assert_eq!(lib.rating("v1").unwrap(), Rating::Indifferent, "unknown");

        lib.like(&t).unwrap();
        assert_eq!(lib.rating("v1").unwrap(), Rating::Like);

        lib.set_rating(&t, Rating::Dislike).unwrap();
        let song = lib.song("v1").unwrap().unwrap();
        assert!(song.disliked() && !song.liked());
        assert!(
            song.in_library(),
            "the like had added it; a dislike keeps that"
        );
        assert_eq!(lib.rating("v1").unwrap(), Rating::Dislike);
        assert!(liked_ids(&lib).is_empty());
        assert_eq!(
            lib.disliked_ids().unwrap(),
            HashSet::from(["v1".to_owned()])
        );

        // Disliking again keeps the first time.
        let at = song.disliked_at;
        lib.set_rating(&t, Rating::Dislike).unwrap();
        assert_eq!(lib.song("v1").unwrap().unwrap().disliked_at, at);

        // A like (here or from a sync) lifts the dislike.
        lib.set_liked_at(&t, Some(5)).unwrap();
        let song = lib.song("v1").unwrap().unwrap();
        assert!(song.liked() && !song.disliked());

        lib.set_rating(&t, Rating::Dislike).unwrap();
        lib.set_rating(&t, Rating::Indifferent).unwrap();
        let song = lib.song("v1").unwrap().unwrap();
        assert!(!song.liked() && !song.disliked());
        assert!(lib.disliked_ids().unwrap().is_empty());

        // Disliking a song the library never saw stores it.
        let new = track("v2", "New", "Artist", None);
        lib.set_rating(&new, Rating::Dislike).unwrap();
        assert_eq!(lib.song("v2").unwrap().unwrap().track.title, "New");
    }

    #[test]
    fn version_one_databases_gain_the_dislike_column() {
        let dir = crate::library::test_util::tempdir();
        let path = dir.join("v1.sqlite");
        {
            // A database as v1 left it, with a liked song.
            let mut c = rusqlite::Connection::open(&path).unwrap();
            let tx = c.transaction().unwrap();
            tx.execute_batch(crate::library::schema::MIGRATIONS[0])
                .unwrap();
            tx.execute(
                "INSERT INTO song (id, title, liked_at, created_at, updated_at)
                 VALUES ('v1', 'Old', 7, 1, 1)",
                [],
            )
            .unwrap();
            tx.pragma_update(None, "user_version", 1).unwrap();
            tx.commit().unwrap();
        }
        let lib = Library::open(&path).unwrap();
        assert_eq!(
            lib.schema_version().unwrap(),
            crate::library::SCHEMA_VERSION
        );
        const { assert!(crate::library::SCHEMA_VERSION >= 2) };
        let song = lib.song("v1").unwrap().unwrap();
        assert_eq!(song.liked_at, Some(7), "kept");
        assert_eq!(song.disliked_at, None);
        assert!(lib.pending_ratings().unwrap().is_empty());
        lib.set_rating(&song.track, Rating::Dislike).unwrap();
        drop(lib);
        // Reopening doesn't migrate twice.
        let lib = Library::open(&path).unwrap();
        assert_eq!(lib.rating("v1").unwrap(), Rating::Dislike);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn queue_keeps_the_latest_wish() {
        let lib = Library::open_in_memory().unwrap();
        lib.queue_rating("a", Rating::Like).unwrap();
        lib.queue_rating("b", Rating::Dislike).unwrap();
        assert!(!lib.pending_rating_failed("a", "offline").unwrap());
        // A newer wish replaces the older one and starts over.
        lib.queue_rating("a", Rating::Indifferent).unwrap();
        let map = lib.pending_rating_map().unwrap();
        assert_eq!(map["a"], Rating::Indifferent);
        assert_eq!(map["b"], Rating::Dislike);
        let a = &lib.pending_ratings().unwrap()[0];
        assert_eq!((a.attempts, a.last_error.as_deref()), (0, None));

        // Delivering an older value doesn't remove a newer one.
        lib.finish_pending_rating("a", Some(Rating::Like)).unwrap();
        assert!(lib.pending_rating_map().unwrap().contains_key("a"));
        lib.finish_pending_rating("a", Some(Rating::Indifferent))
            .unwrap();
        lib.finish_pending_rating("b", None).unwrap();
        assert!(lib.pending_ratings().unwrap().is_empty());

        // Given up after MAX_RATING_ATTEMPTS failures.
        lib.queue_rating("c", Rating::Like).unwrap();
        for n in 1..MAX_RATING_ATTEMPTS {
            assert!(!lib.pending_rating_failed("c", "500").unwrap(), "{n}");
        }
        assert!(lib.pending_rating_failed("c", "500").unwrap());
        assert!(lib.pending_ratings().unwrap().is_empty());

        lib.queue_rating("d", Rating::Like).unwrap();
        lib.clear_pending_ratings().unwrap();
        assert!(lib.pending_ratings().unwrap().is_empty());
    }

    /// A like made offline waits in the queue and reaches YouTube once it answers.
    #[test]
    fn offline_ratings_are_retried() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};
        let online = Arc::new(AtomicBool::new(false));
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (on, log) = (online.clone(), sent.clone());
        let server = TestServer::start_http(move |req| {
            if !on.load(Ordering::SeqCst) {
                return Reply::Status(503);
            }
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            let endpoint = req
                .path
                .trim_start_matches("/youtubei/v1/")
                .split('?')
                .next()
                .unwrap()
                .to_owned();
            log.lock().unwrap().push((
                endpoint,
                body["target"]["videoId"].as_str().unwrap_or("").to_owned(),
            ));
            Reply::Full {
                status: 200,
                headers: vec!["Content-Type: application/json".into()],
                body: b"{}".to_vec(),
            }
        });
        let it = InnerTube::default()
            .with_api_base(&server.base)
            .with_auth(crate::innertube::Auth::new("SAPISID=fake; SID=fake"));
        let lib = Library::open_in_memory().unwrap();
        let (a, b) = (track("a", "A", "X", None), track("b", "B", "X", None));
        // Rated while offline: saved locally, queued for YouTube.
        lib.like(&a).unwrap();
        lib.queue_rating("a", Rating::Like).unwrap();
        lib.set_rating(&b, Rating::Dislike).unwrap();
        lib.queue_rating("b", Rating::Dislike).unwrap();

        let push = lib.push_pending_ratings(&it, 50).unwrap();
        assert_eq!((push.sent, push.left, push.dropped), (0, 2, 0));
        assert!(push.error.is_some());
        let first = &lib.pending_ratings().unwrap()[0];
        assert_eq!(first.attempts, 1, "stopped at the first failure");
        assert!(first.last_error.is_some());
        assert_eq!(lib.pending_ratings().unwrap()[1].attempts, 0);

        online.store(true, Ordering::SeqCst);
        let push = lib.push_pending_ratings(&it, 50).unwrap();
        assert_eq!((push.sent, push.left, push.error), (2, 0, None));
        assert_eq!(
            *sent.lock().unwrap(),
            [
                ("like/like".to_owned(), "a".to_owned()),
                ("like/dislike".to_owned(), "b".to_owned())
            ]
        );
        assert_eq!(lib.rating("a").unwrap(), Rating::Like, "local state kept");
        assert_eq!(lib.rating("b").unwrap(), Rating::Dislike);
    }

    /// An expired sign-in pauses the queue: no attempt is counted, nothing is
    /// dropped, and it all goes out once the session works again.
    #[test]
    fn expired_sign_in_pauses_the_queue() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let signed_in = Arc::new(AtomicBool::new(false));
        let ok = signed_in.clone();
        let server = TestServer::start_http(move |_| {
            let status = if ok.load(Ordering::SeqCst) { 200 } else { 401 };
            Reply::Full {
                status,
                headers: vec!["Content-Type: application/json".into()],
                body: b"{}".to_vec(),
            }
        });
        let it = InnerTube::default()
            .with_api_base(&server.base)
            .with_auth(crate::innertube::Auth::new("SAPISID=fake; SID=fake"));
        let lib = Library::open_in_memory().unwrap();
        lib.queue_rating("a", Rating::Like).unwrap();
        lib.queue_rating("b", Rating::Like).unwrap();
        // Far more pushes than the attempt budget.
        for _ in 0..MAX_RATING_ATTEMPTS + 5 {
            let push = lib.push_pending_ratings(&it, 50).unwrap();
            assert!(push.auth_failed);
            assert_eq!((push.sent, push.dropped, push.left), (0, 0, 2));
        }
        assert!(
            lib.pending_ratings()
                .unwrap()
                .iter()
                .all(|p| p.attempts == 0)
        );

        signed_in.store(true, Ordering::SeqCst);
        let push = lib.push_pending_ratings(&it, 50).unwrap();
        assert!(!push.auth_failed);
        assert_eq!((push.sent, push.left), (2, 0));
    }
}
