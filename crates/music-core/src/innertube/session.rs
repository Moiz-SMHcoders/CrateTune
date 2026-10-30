//! The live signed-in session behind every clone of an [`Auth`](super::Auth).
//!
//! The app builds one [`Session`] per sign-in and attaches it to the `Auth` it hands
//! out ([`Auth::with_session`](super::Auth::with_session)), so the browse client, the
//! account client and the playback resolver all sign with the same, current cookie:
//!
//! * **Rotation.** Signed-in responses' `Set-Cookie` lines (see `cookies.rs`) are
//!   merged into the cookie, like a browser does. The app writes the new value back
//!   to the secrets store, throttled ([`Session::take_persist`]).
//! * **Recovery.** A request YouTube answers with "signed out" (401, or 403 saying
//!   so) asks the app's [`SessionHooks::refresh`] once (silent re-login), at most
//!   every [`REFRESH_MIN_INTERVAL`], and is retried once with the new cookie.
//! * **Expiry.** When that fails the session is marked expired and the app is told
//!   ([`SessionHooks::expired_changed`]) once, not per request.
//!
//! Cookie values never appear in `Debug` output or logs.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use super::cookies::SetCookie;

/// Silent re-logins are tried at most this often.
pub const REFRESH_MIN_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// A rotated cookie is handed out for saving at most this often.
pub const PERSIST_MIN_INTERVAL: Duration = Duration::from_secs(60);

/// What the app does for a session (implemented by the GUI).
pub trait SessionHooks: Send + Sync {
    /// Called on the worker thread of a request YouTube rejected as signed out.
    /// Blocks while the app tries to renew the session without the user (a hidden
    /// sign-in web view); on success the app puts the new cookie in with
    /// [`Session::replace_cookie`] and returns `true`.
    fn refresh(&self, session: &Session) -> bool;
    /// The session stopped working (`true`: refresh failed or unavailable) or works
    /// again (`false`). Called on transitions only.
    fn expired_changed(&self, session: &Session, expired: bool);
}

struct State {
    cookie: String,
    /// Bumped whenever `cookie` changes.
    revision: u64,
    expired: bool,
    last_refresh: Option<Instant>,
    /// The revision / value last handed out by `take_persist` (initially the cookie
    /// the session started with, which the app already stored).
    persisted_revision: u64,
    persisted_value: String,
    last_persist: Option<Instant>,
}

/// See the module docs. Shared as `Arc<Session>`.
pub struct Session {
    id: u64,
    state: Mutex<State>,
    hooks: RwLock<Option<Arc<dyn SessionHooks>>>,
    /// Held while a refresh runs: concurrent failures wait for it, then retry.
    refreshing: Mutex<()>,
    refresh_interval: Duration,
    persist_interval: Duration,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let st = self.lock();
        f.debug_struct("Session")
            .field("id", &self.id)
            .field("revision", &st.revision)
            .field("expired", &st.expired)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// A session starting with `cookie` (already stored by the app).
    pub fn new(cookie: impl Into<String>) -> Arc<Self> {
        Self::with_intervals(cookie, REFRESH_MIN_INTERVAL, PERSIST_MIN_INTERVAL)
    }

    /// [`Session::new`] with other rate limits (tests).
    pub fn with_intervals(
        cookie: impl Into<String>,
        refresh_interval: Duration,
        persist_interval: Duration,
    ) -> Arc<Self> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let cookie = cookie.into();
        Arc::new(Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            state: Mutex::new(State {
                persisted_value: cookie.clone(),
                cookie,
                revision: 0,
                expired: false,
                last_refresh: None,
                persisted_revision: 0,
                last_persist: None,
            }),
            hooks: RwLock::new(None),
            refreshing: Mutex::new(()),
            refresh_interval,
            persist_interval,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Unique per session in this process.
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn set_hooks(&self, hooks: Arc<dyn SessionHooks>) {
        *self.hooks.write().unwrap_or_else(PoisonError::into_inner) = Some(hooks);
    }

    fn hooks(&self) -> Option<Arc<dyn SessionHooks>> {
        self.hooks
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The current `Cookie` header.
    pub fn cookie(&self) -> String {
        self.lock().cookie.clone()
    }

    /// Changes whenever the cookie does.
    pub fn revision(&self) -> u64 {
        self.lock().revision
    }

    /// The cookie and its revision, read together.
    pub(crate) fn snapshot(&self) -> (String, u64) {
        let st = self.lock();
        (st.cookie.clone(), st.revision)
    }

    pub fn is_expired(&self) -> bool {
        self.lock().expired
    }

    /// Whether `cookie` is this session's current value or the one last saved (a
    /// value the app wrote back must not look like a new sign-in).
    pub fn is_known_value(&self, cookie: &str) -> bool {
        let st = self.lock();
        st.cookie == cookie || st.persisted_value == cookie
    }

    /// Replaces the whole cookie (silent re-login, or signing in again to the same
    /// login): the session works again. Returns whether the value changed.
    pub fn replace_cookie(&self, cookie: impl Into<String>) -> bool {
        let cookie = cookie.into();
        let changed = {
            let mut st = self.lock();
            let changed = st.cookie != cookie;
            if changed {
                st.cookie = cookie;
                st.revision += 1;
            }
            changed
        };
        self.set_expired(false);
        changed
    }

    /// Applies `Set-Cookie` updates (already filtered by `cookies::applicable`).
    /// Returns whether the cookie changed.
    pub(crate) fn absorb(&self, updates: &[SetCookie]) -> bool {
        if updates.is_empty() {
            return false;
        }
        let mut st = self.lock();
        match super::cookies::merge(&st.cookie, updates) {
            Some(merged) => {
                st.cookie = merged;
                st.revision += 1;
                true
            }
            None => false,
        }
    }

    /// Marks the session expired or working; tells the hooks on a transition.
    pub(crate) fn set_expired(&self, expired: bool) {
        let changed = {
            let mut st = self.lock();
            let changed = st.expired != expired;
            st.expired = expired;
            changed
        };
        if changed && let Some(hooks) = self.hooks() {
            hooks.expired_changed(self, expired);
        }
    }

    /// A signed-in request went through.
    pub(crate) fn note_success(&self) {
        if self.is_expired() {
            self.set_expired(false);
        }
    }

    /// A request signed with revision `failed` was rejected as signed out. Returns
    /// whether to retry it: the cookie changed meanwhile, or a refresh just renewed
    /// it. Otherwise the session is marked expired. Blocks during a refresh.
    pub(crate) fn recover(&self, failed: u64) -> bool {
        let _one_at_a_time = self
            .refreshing
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        {
            let mut st = self.lock();
            if st.revision != failed {
                return true;
            }
            let now = Instant::now();
            if st
                .last_refresh
                .is_some_and(|t| now.duration_since(t) < self.refresh_interval)
            {
                drop(st);
                self.set_expired(true);
                return false;
            }
            st.last_refresh = Some(now);
        }
        let refreshed = self.hooks().is_some_and(|h| h.refresh(self)) && self.revision() != failed;
        if !refreshed {
            self.set_expired(true);
        }
        refreshed
    }

    /// The cookie to save, if it changed since the last save and the last save is at
    /// least [`PERSIST_MIN_INTERVAL`] old. Call periodically.
    pub fn take_persist(&self) -> Option<String> {
        self.take_persist_at(Instant::now(), false)
    }

    /// The cookie to save if it changed, regardless of the throttle (on quit).
    pub fn take_persist_now(&self) -> Option<String> {
        self.take_persist_at(Instant::now(), true)
    }

    pub(crate) fn take_persist_at(&self, now: Instant, force: bool) -> Option<String> {
        let mut st = self.lock();
        if st.revision == st.persisted_revision {
            return None;
        }
        if !force
            && st
                .last_persist
                .is_some_and(|t| now.saturating_duration_since(t) < self.persist_interval)
        {
            return None;
        }
        st.persisted_revision = st.revision;
        if st.cookie == st.persisted_value {
            // Rotated and rotated back: nothing new to write.
            return None;
        }
        st.persisted_value = st.cookie.clone();
        st.last_persist = Some(now);
        Some(st.cookie.clone())
    }

    /// Forgets when the last refresh ran, as if the interval were over (tests).
    #[cfg(test)]
    pub(crate) fn forget_last_refresh(&self) {
        self.lock().last_refresh = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::innertube::cookies::parse_set_cookie;
    use std::sync::atomic::AtomicUsize;

    fn sc(line: &str) -> SetCookie {
        parse_set_cookie(line, 0).unwrap()
    }

    #[test]
    fn persist_is_throttled_and_only_on_change() {
        let s = Session::with_intervals("A=1; B=2", REFRESH_MIN_INTERVAL, Duration::from_secs(60));
        let t0 = Instant::now();
        assert_eq!(s.take_persist_at(t0, false), None, "nothing changed yet");
        assert!(s.absorb(&[sc("A=2")]));
        assert_eq!(s.take_persist_at(t0, false).as_deref(), Some("A=2; B=2"));
        assert_eq!(s.take_persist_at(t0, false), None, "already saved");
        // Another rotation within the minute waits…
        assert!(s.absorb(&[sc("B=3")]));
        let t1 = t0 + Duration::from_secs(30);
        assert_eq!(s.take_persist_at(t1, false), None);
        // …until the minute is over,
        let t2 = t0 + Duration::from_secs(61);
        assert_eq!(s.take_persist_at(t2, false).as_deref(), Some("A=2; B=3"));
        // or the app quits.
        assert!(s.absorb(&[sc("B=4")]));
        assert_eq!(s.take_persist_at(t2, false), None);
        assert_eq!(s.take_persist_at(t2, true).as_deref(), Some("A=2; B=4"));
        assert_eq!(s.take_persist_at(t2, true), None);
        // The same value again isn't a change; rotating back to the saved value isn't
        // written either.
        assert!(!s.absorb(&[sc("B=4")]));
        assert!(s.absorb(&[sc("B=5")]));
        assert!(s.absorb(&[sc("B=4")]));
        let t3 = t2 + Duration::from_secs(120);
        assert_eq!(s.take_persist_at(t3, false), None);
        assert!(s.is_known_value("A=2; B=4"));
        assert!(!s.is_known_value("A=2; B=5"));
    }

    #[derive(Default)]
    struct Hooks {
        refreshes: AtomicUsize,
        /// What a refresh puts in (`None`: it fails).
        new_cookie: Mutex<Option<String>>,
        expired: Mutex<Vec<bool>>,
    }

    impl SessionHooks for Hooks {
        fn refresh(&self, session: &Session) -> bool {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            match self.new_cookie.lock().unwrap().clone() {
                Some(c) => {
                    session.replace_cookie(c);
                    true
                }
                None => false,
            }
        }
        fn expired_changed(&self, _: &Session, expired: bool) {
            self.expired.lock().unwrap().push(expired);
        }
    }

    #[test]
    fn recover_refreshes_once_per_interval() {
        let s = Session::new("SAPISID=old");
        let hooks = Arc::new(Hooks::default());
        s.set_hooks(hooks.clone());
        *hooks.new_cookie.lock().unwrap() = Some("SAPISID=new".into());

        let rev = s.revision();
        assert!(s.recover(rev), "refreshed");
        assert_eq!(s.cookie(), "SAPISID=new");
        assert_eq!(hooks.refreshes.load(Ordering::SeqCst), 1);
        assert!(!s.is_expired());

        // A request signed before the refresh just retries.
        assert!(s.recover(rev));
        assert_eq!(hooks.refreshes.load(Ordering::SeqCst), 1);

        // The new cookie fails too, within ten minutes: no second refresh, expired once.
        let rev = s.revision();
        assert!(!s.recover(rev));
        assert!(!s.recover(rev));
        assert_eq!(hooks.refreshes.load(Ordering::SeqCst), 1);
        assert!(s.is_expired());
        assert_eq!(*hooks.expired.lock().unwrap(), [true]);

        // Later (interval over) a failing refresh keeps it expired, without a new event.
        s.forget_last_refresh();
        *hooks.new_cookie.lock().unwrap() = None;
        assert!(!s.recover(rev));
        assert_eq!(hooks.refreshes.load(Ordering::SeqCst), 2);
        assert_eq!(*hooks.expired.lock().unwrap(), [true]);

        // Signing in again (same login) makes it work again.
        s.replace_cookie("SAPISID=again");
        assert!(!s.is_expired());
        assert_eq!(*hooks.expired.lock().unwrap(), [true, false]);
        // A success while expired also clears it.
        s.set_expired(true);
        s.note_success();
        assert_eq!(*hooks.expired.lock().unwrap(), [true, false, true, false]);
    }

    #[test]
    fn without_hooks_recovery_fails() {
        let s = Session::new("SAPISID=x");
        assert!(!s.recover(s.revision()));
        assert!(s.is_expired());
        assert!(!format!("{s:?}").contains("SAPISID"));
    }
}
