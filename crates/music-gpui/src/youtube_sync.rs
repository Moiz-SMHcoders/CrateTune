//! Keeping likes (and the rest of the library) in step with YouTube Music without
//! pressing "Sync now".
//!
//! ArchiveTune syncs the whole library when the app starts signed in, after a
//! sign-in and on an account switch (`SyncUtils.performFullSync`, gated by
//! `YtmSyncKey`, default on), and the liked songs when their page opens. This
//! does the same, plus:
//!
//! * **When.** At start (unless the last sync is under [`STARTUP_GAP`] old), every
//!   [`PERIODIC_GAP`] while running, when the window comes back to the front after
//!   [`FOCUS_GAP`], and when the Liked tab opens ([`LIKED_PAGE_GAP`]). These
//!   automatic syncs are "liked songs only" (one `VLLM` request plus continuations)
//!   unless the last full sync is [`FULL_EVERY`] old. Sign-in, account switch and
//!   "Sync now" always run a full sync. Settings → Account → "Sync with YouTube
//!   Music automatically" turns the automatic ones off ([`SyncSchedule::plan`]).
//! * **One at a time.** [`SyncSchedule`] runs one sync; a request made meanwhile
//!   waits and runs after it, unless the finished sync already covered it.
//! * **Each song's like from YouTube.** A few seconds into a song (signed in, sync
//!   on), its rating is read from YouTube's own like button (`next`, see
//!   `InnerTube::like_status`) unless a radio response already carried it, and the
//!   local like / dislike follows ([`MusicApp::apply_remote_ratings`]). So a song
//!   liked on the phone shows the filled heart as soon as it plays here.
//! * **Offline ratings.** A like / dislike YouTube didn't take waits in the
//!   library's queue (`Library::queue_rating`); it is retried every
//!   [`RETRY_EVERY`] and before each sync, and syncs don't undo it meanwhile.
//!
//! YouTube stays the source of truth: a full sync (and a liked-songs sync) unlikes
//! local songs YouTube doesn't list as liked, except local files, ratings still
//! queued, and, for automatic syncs, an empty or truncated list (a failed parse
//! must not wipe the likes).
//!
//! **Not verified against a real account**: the requests are music-core's cookie
//! layer, tested against fixtures and loopback fakes only.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{Context, Window};
use music_core::Track;
use music_core::innertube::{Rating, WatchQueue};
use music_core::library::now_ms;

use crate::app::MusicApp;
use crate::store::{rating_flag, set_rating_flag};
use crate::util::off_thread;

const MINUTE: i64 = 60_000;
/// No sync at start if the last one is younger.
pub const STARTUP_GAP: i64 = 10 * MINUTE;
/// A sync while running at most this often.
pub const PERIODIC_GAP: i64 = 45 * MINUTE;
/// Coming back to the window syncs if the last sync is this old.
pub const FOCUS_GAP: i64 = 30 * MINUTE;
/// Opening the Liked tab syncs the liked songs if they are this old.
pub const LIKED_PAGE_GAP: i64 = 10 * MINUTE;
/// An automatic sync is a full one when the last full sync is this old.
pub const FULL_EVERY: i64 = 6 * 60 * MINUTE;
/// How often the watcher looks (cheap: no request unless something is due).
const TICK: Duration = Duration::from_secs(60);
/// Queued ratings are retried this often.
pub const RETRY_EVERY: Duration = Duration::from_secs(5 * 60);
/// A song's like is asked this long after it started (skipped songs cost nothing,
/// and a radio response may answer first).
const LIKE_CHECK_DELAY: Duration = Duration::from_secs(3);
/// The same song's like isn't asked again sooner.
const LIKE_RECHECK: Duration = Duration::from_secs(30 * 60);
/// Queued ratings sent per retry.
const RETRY_BATCH: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SyncKind {
    /// Liked songs only.
    Liked,
    /// Liked songs, library songs, albums, subscriptions, playlists.
    Full,
}

/// Why a sync is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Startup,
    Periodic,
    Focus,
    /// The Liked tab opened.
    LikedPage,
    SignIn,
    AccountSwitch,
    /// "Sync now" (Settings, `@library=sync`).
    Manual,
    /// The Liked tab's sync button.
    ManualLiked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncRequest {
    pub kind: SyncKind,
    /// Unlike local songs even when YouTube's list comes back empty (the user
    /// asked, or another account's library replaces this one).
    pub authoritative: bool,
    /// Toast the result.
    pub announce: bool,
}

impl SyncRequest {
    fn merge(self, other: Self) -> Self {
        Self {
            kind: self.kind.max(other.kind),
            authoritative: self.authoritative || other.authoritative,
            announce: self.announce || other.announce,
        }
    }

    /// Whether running `self` makes `other` unnecessary.
    fn covers(self, other: Self) -> bool {
        self.kind >= other.kind && (self.authoritative || !other.authoritative)
    }
}

/// What [`SyncSchedule::finish`] says to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Finished {
    /// Start this one now (it is marked running).
    pub next: Option<SyncRequest>,
    /// Toast the finished sync's result (it or a request it covered asked for it).
    pub announce: bool,
}

/// The sync guard and throttle: pure state, no I/O (tested below).
#[derive(Debug, Default)]
pub struct SyncSchedule {
    running: Option<SyncRequest>,
    queued: Option<SyncRequest>,
    /// Last automatic attempt (Unix ms), successful or not: an offline app doesn't
    /// retry every minute.
    last_attempt: Option<i64>,
}

impl SyncSchedule {
    pub fn running(&self) -> Option<SyncRequest> {
        self.running
    }

    /// What `trigger` should run now, if anything. `last_full` / `last_liked` are the
    /// last successful syncs (Unix ms); `auto` is the "sync automatically" setting.
    pub fn plan(
        &self,
        trigger: Trigger,
        auto: bool,
        now: i64,
        last_full: Option<i64>,
        last_liked: Option<i64>,
    ) -> Option<SyncRequest> {
        let full = |authoritative, announce| SyncRequest {
            kind: SyncKind::Full,
            authoritative,
            announce,
        };
        let gap = match trigger {
            Trigger::Manual => return Some(full(true, true)),
            Trigger::SignIn | Trigger::AccountSwitch => return Some(full(true, true)),
            Trigger::ManualLiked => {
                return Some(SyncRequest {
                    kind: SyncKind::Liked,
                    authoritative: true,
                    announce: true,
                });
            }
            Trigger::Startup => STARTUP_GAP,
            Trigger::Periodic => PERIODIC_GAP,
            Trigger::Focus => FOCUS_GAP,
            Trigger::LikedPage => LIKED_PAGE_GAP,
        };
        if !auto {
            return None;
        }
        let last = [last_full, last_liked, self.last_attempt]
            .into_iter()
            .flatten()
            .max();
        if last.is_some_and(|t| now.saturating_sub(t) < gap) {
            return None;
        }
        let full_due = last_full.is_none_or(|t| now.saturating_sub(t) >= FULL_EVERY);
        Some(SyncRequest {
            kind: if full_due && trigger != Trigger::LikedPage {
                SyncKind::Full
            } else {
                SyncKind::Liked
            },
            authoritative: false,
            announce: false,
        })
    }

    /// Marks `req` running and returns `true`, or (one is running) keeps it for
    /// later and returns `false`.
    pub fn begin(&mut self, req: SyncRequest, now: i64) -> bool {
        if self.running.is_some() {
            self.queued = Some(self.queued.map_or(req, |q| q.merge(req)));
            return false;
        }
        self.running = Some(req);
        self.last_attempt = Some(now);
        true
    }

    /// The running sync ended. A waiting request it covered is dropped (its toast
    /// moves to this result); another one is started.
    pub fn finish(&mut self, now: i64) -> Finished {
        let Some(done) = self.running.take() else {
            return Finished {
                next: None,
                announce: false,
            };
        };
        let mut announce = done.announce;
        let next = match self.queued.take() {
            Some(q) if done.covers(q) => {
                announce |= q.announce;
                None
            }
            Some(q) => {
                self.running = Some(q);
                self.last_attempt = Some(now);
                Some(q)
            }
            None => None,
        };
        Finished { next, announce }
    }

    /// Signed out: nothing waits; a sync still running finishes on its own.
    pub fn forget_queued(&mut self) {
        self.queued = None;
    }
}

/// Automatic-sync state kept with the account (`Account::yt`).
#[derive(Debug, Default)]
pub struct YtSync {
    pub schedule: SyncSchedule,
    /// When each song's like was last asked (or carried by a radio response).
    checked: HashMap<String, Instant>,
    /// When each song was last rated here (a remote answer asked before that is
    /// stale).
    rated: HashMap<String, Instant>,
    last_retry: Option<Instant>,
    retrying: bool,
}

impl YtSync {
    /// The user rated `video_id` here just now.
    pub fn rated_here(&mut self, video_id: &str) {
        let now = Instant::now();
        self.rated.insert(video_id.to_owned(), now);
        // Our own rating is the newest; no need to ask YouTube for it.
        self.checked.insert(video_id.to_owned(), now);
    }

    fn recently_checked(&self, video_id: &str, now: Instant) -> bool {
        self.checked
            .get(video_id)
            .is_some_and(|t| now.saturating_duration_since(*t) < LIKE_RECHECK)
    }

    fn mark_checked(&mut self, video_id: &str, now: Instant) {
        if self.checked.len() > 2000 {
            self.checked
                .retain(|_, t| now.saturating_duration_since(*t) < LIKE_RECHECK);
        }
        self.checked.insert(video_id.to_owned(), now);
    }

    /// Whether a remote answer asked at `asked_at` is still news for `video_id`.
    fn fresh(&self, video_id: &str, asked_at: Instant) -> bool {
        self.rated.get(video_id).is_none_or(|t| *t < asked_at)
    }

    /// Account switch / sign-out: what was asked was another account's.
    pub fn forget_account(&mut self) {
        self.checked.clear();
        self.rated.clear();
        self.schedule.forget_queued();
    }
}

/// Keeps the remote ratings that change something locally: `rated(id)` is the
/// local rating, `fresh(id)` whether nothing was rated here since asking.
pub fn ratings_to_apply(
    remote: Vec<(Track, Rating)>,
    rated: impl Fn(&str) -> Rating,
    fresh: impl Fn(&str) -> bool,
) -> Vec<(Track, Rating)> {
    remote
        .into_iter()
        .filter(|(t, r)| !t.is_local() && fresh(&t.video_id) && rated(&t.video_id) != *r)
        .collect()
}

impl MusicApp {
    fn auto_sync_on(&self) -> bool {
        self.store.prefs.auto_sync
    }

    /// Starts the watcher: periodic syncs, retries of queued ratings, and a sync
    /// when the window comes back to the front after a while.
    pub(crate) fn start_youtube_sync_watch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            loop {
                executor.timer(TICK).await;
                let alive = this.update(cx, |this, cx| {
                    this.request_sync(Trigger::Periodic, cx);
                    this.retry_queued_ratings(false, cx);
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.request_sync(Trigger::Focus, cx);
            }
        })
        .detach();
    }

    /// Asks for a sync (see [`SyncSchedule::plan`]); it runs now, after the one
    /// running, or not at all (too soon, automatic syncs off, signed out).
    pub(crate) fn request_sync(&mut self, trigger: Trigger, cx: &mut Context<Self>) {
        let account = &self.store.account;
        if !account.is_signed_in() || self.store.library.is_none() {
            return;
        }
        let config = account.config.as_ref();
        let (last_full, last_liked) = (
            config.and_then(|c| c.last_sync_at),
            config.and_then(|c| c.last_liked_sync_at.or(c.last_sync_at)),
        );
        let now = now_ms();
        let Some(req) =
            account
                .yt
                .schedule
                .plan(trigger, self.auto_sync_on(), now, last_full, last_liked)
        else {
            return;
        };
        if self.store.account.yt.schedule.begin(req, now) {
            self.run_sync(req, cx);
        }
    }

    /// The running sync finished: start what waited, if anything. Returns whether
    /// the result should be toasted.
    pub(crate) fn sync_finished(&mut self, cx: &mut Context<Self>) -> bool {
        let finished = self.store.account.yt.schedule.finish(now_ms());
        if let Some(next) = finished.next {
            self.run_sync(next, cx);
        }
        finished.announce
    }

    /// Sends queued ratings to YouTube (every [`RETRY_EVERY`], or now with `force`).
    pub(crate) fn retry_queued_ratings(&mut self, force: bool, cx: &mut Context<Self>) {
        let yt = &self.store.account.yt;
        let due = force || yt.last_retry.is_none_or(|t| t.elapsed() >= RETRY_EVERY);
        // An expired sign-in pauses the queue until it works again
        // (`session_keeper.rs` sends it then).
        if !due || yt.retrying || yt.schedule.running().is_some() || self.store.account.expired {
            return;
        }
        let Some(library) = self.store.library.clone() else {
            return;
        };
        let client = self.account_client();
        if !client.is_signed_in() {
            return;
        }
        let yt = &mut self.store.account.yt;
        yt.last_retry = Some(Instant::now());
        yt.retrying = true;
        cx.spawn(async move |this, cx| {
            let result = off_thread(move || {
                if library.pending_ratings()?.is_empty() {
                    return Ok(None);
                }
                library.push_pending_ratings(&client, RETRY_BATCH).map(Some)
            })
            .await;
            this.update(cx, |this, _| {
                this.store.account.yt.retrying = false;
                match result {
                    Ok(Some(push)) if push.auth_failed => eprintln!(
                        "music-gpui: queued ratings: {} left, waiting for a working sign-in",
                        push.left
                    ),
                    Ok(Some(push)) if this.trace.is_some() || push.error.is_some() => eprintln!(
                        "music-gpui: queued ratings: {} sent, {} left, {} dropped{}",
                        push.sent,
                        push.left,
                        push.dropped,
                        push.error.map(|e| format!(" ({e})")).unwrap_or_default()
                    ),
                    Err(e) => eprintln!("music-gpui: queued ratings: {e}"),
                    _ => {}
                }
            })
            .ok();
        })
        .detach();
    }

    /// A song started playing: a few seconds in, ask YouTube for its rating.
    pub(crate) fn check_like_on_play(&mut self, track: &Track, cx: &mut Context<Self>) {
        if track.is_local()
            || !self.auto_sync_on()
            || !self.store.account.is_signed_in()
            || self.store.library.is_none()
            || self
                .store
                .account
                .yt
                .recently_checked(&track.video_id, Instant::now())
        {
            return;
        }
        let track = track.clone();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            executor.timer(LIKE_CHECK_DELAY).await;
            let ask = this
                .update(cx, |this, _| {
                    let now = Instant::now();
                    if this.current_video_id() != Some(track.video_id.as_str())
                        || this.store.account.yt.recently_checked(&track.video_id, now)
                    {
                        return None;
                    }
                    this.store.account.yt.mark_checked(&track.video_id, now);
                    Some((this.account_client(), now))
                })
                .ok()
                .flatten();
            let Some((client, asked_at)) = ask else {
                return;
            };
            let id = track.video_id.clone();
            let result = off_thread(move || client.like_status(&id)).await;
            this.update(cx, |this, cx| match result {
                Ok(Some(rating)) => this.apply_remote_ratings(vec![(track, rating)], asked_at, cx),
                Ok(None) => {}
                Err(e) => eprintln!("music-gpui: like status: {e}"),
            })
            .ok();
        })
        .detach();
    }

    /// A `next` response (radio) carried ratings: use them instead of asking again.
    pub(crate) fn note_queue_ratings(
        &mut self,
        queue: &WatchQueue,
        asked_at: Instant,
        cx: &mut Context<Self>,
    ) {
        if !self.auto_sync_on() || !self.store.account.is_signed_in() {
            return;
        }
        let now = Instant::now();
        let current = self.queue.current_item().map(|i| i.track.clone());
        let ratings: Vec<(Track, Rating)> = queue
            .trusted_ratings()
            .filter_map(|r| {
                let track = queue
                    .tracks
                    .iter()
                    .find(|t| t.video_id == r.video_id)
                    .or(current.as_ref().filter(|t| t.video_id == r.video_id))?;
                Some((track.clone(), r.rating))
            })
            .collect();
        for (t, _) in &ratings {
            self.store.account.yt.mark_checked(&t.video_id, now);
        }
        self.apply_remote_ratings(ratings, asked_at, cx);
    }

    /// Makes local likes / dislikes match what YouTube said (asked at `asked_at`),
    /// except where the user rated since or a rating still waits to be sent.
    pub(crate) fn apply_remote_ratings(
        &mut self,
        remote: Vec<(Track, Rating)>,
        asked_at: Instant,
        cx: &mut Context<Self>,
    ) {
        let yt = &self.store.account.yt;
        let changes = ratings_to_apply(remote, rating_flag, |id| yt.fresh(id, asked_at));
        let Some(library) = self.store.library.clone() else {
            return;
        };
        if changes.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = off_thread(move || {
                let pending = library.pending_rating_map()?;
                let mut applied = Vec::new();
                for (track, rating) in changes {
                    if pending.contains_key(&track.video_id) {
                        continue;
                    }
                    library.set_rating(&track, rating)?;
                    applied.push((track.video_id, rating));
                }
                music_core::Result::Ok(applied)
            })
            .await;
            this.update(cx, |this, cx| match result {
                Ok(applied) if !applied.is_empty() => {
                    for (id, rating) in &applied {
                        if this.store.account.yt.fresh(id, asked_at) {
                            set_rating_flag(id, *rating);
                        }
                    }
                    if this.trace.is_some() {
                        eprintln!("music-gpui: ratings from YouTube: {applied:?}");
                    }
                    this.library_changed(cx);
                }
                Ok(_) => {}
                Err(e) => eprintln!("music-gpui: ratings from YouTube: {e}"),
            })
            .ok();
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: i64 = 60 * MINUTE;
    const NOW: i64 = 1_000 * H;

    fn liked(authoritative: bool, announce: bool) -> SyncRequest {
        SyncRequest {
            kind: SyncKind::Liked,
            authoritative,
            announce,
        }
    }

    fn full(authoritative: bool, announce: bool) -> SyncRequest {
        SyncRequest {
            kind: SyncKind::Full,
            authoritative,
            announce,
        }
    }

    #[test]
    fn automatic_syncs_are_throttled() {
        let s = SyncSchedule::default();
        let plan = |t, last_full, last_liked| s.plan(t, true, NOW, last_full, last_liked);

        // Never synced: the first start syncs everything.
        assert_eq!(plan(Trigger::Startup, None, None), Some(full(false, false)));
        // Synced 5 min ago: nothing at start; 20 min ago: the liked songs.
        let recent = Some(NOW - 5 * MINUTE);
        assert_eq!(plan(Trigger::Startup, recent, recent), None);
        let a_while = Some(NOW - 20 * MINUTE);
        assert_eq!(
            plan(Trigger::Startup, a_while, a_while),
            Some(liked(false, false))
        );
        // The full sync is 7 h old: a full one again.
        assert_eq!(
            plan(Trigger::Startup, Some(NOW - 7 * H), a_while),
            Some(full(false, false))
        );

        // Periodic: 45 min; focus: 30 min; Liked tab: 10 min and never a full sync.
        let m = |n: i64| Some(NOW - n * MINUTE);
        assert_eq!(plan(Trigger::Periodic, m(60), m(40)), None);
        assert_eq!(
            plan(Trigger::Periodic, m(60), m(46)),
            Some(liked(false, false))
        );
        assert_eq!(plan(Trigger::Focus, m(60), m(29)), None);
        assert_eq!(
            plan(Trigger::Focus, m(60), m(31)),
            Some(liked(false, false))
        );
        assert_eq!(plan(Trigger::LikedPage, m(60), m(9)), None);
        assert_eq!(
            plan(Trigger::LikedPage, None, m(11)),
            Some(liked(false, false))
        );

        // Explicit ones always run, authoritative and announced.
        for t in [Trigger::Manual, Trigger::SignIn, Trigger::AccountSwitch] {
            assert_eq!(plan(t, recent, recent), Some(full(true, true)), "{t:?}");
        }
        assert_eq!(
            plan(Trigger::ManualLiked, recent, recent),
            Some(liked(true, true))
        );

        // "Sync automatically" off: only the explicit ones.
        for t in [
            Trigger::Startup,
            Trigger::Periodic,
            Trigger::Focus,
            Trigger::LikedPage,
        ] {
            assert_eq!(s.plan(t, false, NOW, None, None), None, "{t:?}");
        }
        assert!(s.plan(Trigger::Manual, false, NOW, None, None).is_some());
        assert!(s.plan(Trigger::SignIn, false, NOW, None, None).is_some());
    }

    #[test]
    fn a_failed_attempt_waits_like_a_sync() {
        let mut s = SyncSchedule::default();
        let req = s.plan(Trigger::Startup, true, NOW, None, None).unwrap();
        assert!(s.begin(req, NOW));
        s.finish(NOW + MINUTE); // offline: no sync time is saved
        let later = NOW + 2 * MINUTE;
        assert_eq!(s.plan(Trigger::Periodic, true, later, None, None), None);
        assert_eq!(s.plan(Trigger::Focus, true, later, None, None), None);
        assert!(
            s.plan(Trigger::Periodic, true, NOW + PERIODIC_GAP, None, None)
                .is_some()
        );
        assert!(s.plan(Trigger::Manual, true, later, None, None).is_some());
    }

    #[test]
    fn syncs_never_overlap() {
        let mut s = SyncSchedule::default();
        assert!(s.begin(liked(false, false), NOW));
        assert_eq!(s.running(), Some(liked(false, false)));
        // Requests while it runs wait, merged into one.
        assert!(!s.begin(liked(false, false), NOW));
        assert!(!s.begin(full(true, true), NOW));
        assert!(!s.begin(liked(false, false), NOW));
        let done = s.finish(NOW);
        assert_eq!(
            done.next,
            Some(full(true, true)),
            "the bigger one runs next"
        );
        assert!(!done.announce);
        assert_eq!(s.running(), Some(full(true, true)));
        let done = s.finish(NOW);
        assert_eq!(
            done,
            Finished {
                next: None,
                announce: true
            }
        );
        assert_eq!(s.running(), None);
        // Nothing running: finishing again does nothing.
        assert_eq!(s.finish(NOW).next, None);
    }

    #[test]
    fn a_covered_request_is_not_run_twice() {
        let mut s = SyncSchedule::default();
        assert!(s.begin(full(true, false), NOW));
        // "Sync now" on the Liked tab while a full sync runs: covered, but toasted.
        assert!(!s.begin(liked(true, true), NOW));
        assert_eq!(
            s.finish(NOW),
            Finished {
                next: None,
                announce: true
            }
        );
        // An automatic full sync doesn't cover an authoritative one.
        assert!(s.begin(full(false, false), NOW));
        assert!(!s.begin(liked(true, false), NOW));
        assert_eq!(s.finish(NOW).next, Some(liked(true, false)));
        s.finish(NOW);
        // Signed out meanwhile: what waited is dropped.
        assert!(s.begin(liked(false, false), NOW));
        assert!(!s.begin(full(true, true), NOW));
        s.forget_queued();
        assert_eq!(s.finish(NOW).next, None);
    }

    #[test]
    fn remote_ratings_only_change_what_differs() {
        let t = |id: &str| Track {
            video_id: id.into(),
            ..Track::default()
        };
        let local = |id: &str| match id {
            "liked" => Rating::Like,
            "disliked" => Rating::Dislike,
            _ => Rating::Indifferent,
        };
        let remote = vec![
            (t("liked"), Rating::Like),           // same: nothing
            (t("new"), Rating::Like),             // liked elsewhere
            (t("disliked"), Rating::Indifferent), // dislike taken back elsewhere
            (t("rated-here"), Rating::Like),      // rated here after asking: stale
            (t("liked"), Rating::Dislike),        // disliked elsewhere
        ];
        let mut file = t("file");
        file.local_path = Some("/music/a.mp3".into());
        let mut remote = remote;
        remote.push((file, Rating::Like)); // local files aren't YouTube's
        let out = ratings_to_apply(remote, local, |id| id != "rated-here");
        let got: Vec<_> = out.iter().map(|(t, r)| (t.video_id.as_str(), *r)).collect();
        assert_eq!(
            got,
            [
                ("new", Rating::Like),
                ("disliked", Rating::Indifferent),
                ("liked", Rating::Dislike)
            ]
        );
    }

    #[test]
    fn rating_here_wins_over_older_answers() {
        let mut yt = YtSync::default();
        let asked = Instant::now();
        assert!(yt.fresh("a", asked));
        assert!(!yt.recently_checked("a", asked));
        yt.rated_here("a");
        assert!(!yt.fresh("a", asked), "rated after asking");
        assert!(yt.fresh("a", Instant::now() + Duration::from_millis(1)));
        assert!(yt.recently_checked("a", Instant::now()));
        assert!(!yt.recently_checked("a", Instant::now() + LIKE_RECHECK));
        yt.forget_account();
        assert!(yt.fresh("a", asked) && !yt.recently_checked("a", Instant::now()));
    }
}
