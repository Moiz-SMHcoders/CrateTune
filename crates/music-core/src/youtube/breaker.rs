// Ported from refryt crates/refryt-resolve/src/chain.rs (the circuit breaker) — © refryt contributors, GPL-3.0
//! A circuit breaker per [`WebClient`]: a client that failed
//! [`FAILURES_BEFORE_TRIP`] times in a row is skipped for [`COOLDOWN`], so a
//! route YouTube broke costs at most a few songs' worth of time, not every song.
//!
//! * A failure that belongs to one video (gone, private, age- or Premium-gated,
//!   blocked in the country) says nothing about the client and isn't counted;
//!   neither is a network outage (every client fails the same way).
//! * After the cooldown the client gets one more try ("half-open"); failing
//!   again restarts the cooldown, a success closes the breaker.
//! * The resolver never skips the last enabled client, so a run of failures
//!   can't leave nothing to try.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::WebClient;
use crate::Error;

/// Consecutive failures before a client is skipped.
pub const FAILURES_BEFORE_TRIP: u32 = 3;
/// How long a tripped client is skipped before it is tried again.
pub const COOLDOWN: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Default, Clone)]
struct Breaker {
    consecutive_failures: u32,
    tripped_at: Option<Instant>,
    last_error: Option<String>,
}

impl Breaker {
    fn open(&self, now: Instant) -> bool {
        self.tripped_at
            .is_some_and(|at| now.saturating_duration_since(at) < COOLDOWN)
    }
}

/// The breakers of every client.
#[derive(Debug, Default)]
pub(crate) struct Breakers {
    inner: Mutex<HashMap<WebClient, Breaker>>,
}

impl Breakers {
    /// Whether `client` is to be skipped now (tripped and cooling down).
    pub(crate) fn should_skip(&self, client: WebClient, now: Instant) -> bool {
        self.lock().get(&client).is_some_and(|b| b.open(now))
    }

    /// Closes `client`'s breaker; true if that changed anything.
    pub(crate) fn record_success(&self, client: WebClient) -> bool {
        self.lock()
            .remove(&client)
            .is_some_and(|b| b.consecutive_failures > 0)
    }

    /// Counts a failure; true if this one tripped the breaker (or restarted its cooldown).
    pub(crate) fn record_failure(&self, client: WebClient, error: &str, now: Instant) -> bool {
        let mut map = self.lock();
        let b = map.entry(client).or_default();
        b.consecutive_failures += 1;
        b.last_error = Some(error.chars().take(200).collect());
        match b.tripped_at {
            None if b.consecutive_failures >= FAILURES_BEFORE_TRIP => {
                b.tripped_at = Some(now);
                true
            }
            // Its half-open try failed too: another full cooldown.
            Some(at) if now.saturating_duration_since(at) >= COOLDOWN => {
                b.tripped_at = Some(now);
                true
            }
            _ => false,
        }
    }

    /// Every client with failures, for [`ResolverStatus`].
    pub(crate) fn health(&self, now: Instant) -> Vec<ClientHealth> {
        let mut out: Vec<ClientHealth> = self
            .lock()
            .iter()
            .map(|(client, b)| ClientHealth {
                client: *client,
                consecutive_failures: b.consecutive_failures,
                skipped_for: b
                    .tripped_at
                    .filter(|_| b.open(now))
                    .map(|at| COOLDOWN.saturating_sub(now.saturating_duration_since(at))),
                last_error: b.last_error.clone(),
            })
            .collect();
        out.sort_by_key(|h| WebClient::ALL.iter().position(|c| *c == h.client));
        out
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<WebClient, Breaker>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Whether a failure is about this one video (or the network) rather than the
/// client: the next video has no reason to fail the same way, so the breaker
/// doesn't count it.
pub(crate) fn is_video_specific(error: &Error) -> bool {
    if error.is_network() {
        return true;
    }
    let Error::Unplayable(reason) = error else {
        return false;
    };
    let r = reason.to_lowercase();
    // "Sign in to confirm you're not a bot" is the route being refused: counted.
    if r.contains("not a bot") {
        return false;
    }
    [
        "unavailable",
        "not available",
        "private video",
        "removed",
        "terminated",
        "deleted",
        "confirm your age",
        "age-restricted",
        "inappropriate",
        "premium",
        "members",
        "purchase",
        "copyright",
        "country",
        "blocked",
        "live event",
        "will begin",
        "premieres",
        "no stream clients enabled",
    ]
    .iter()
    .any(|needle| r.contains(needle))
}

/// The health of one client (only those that failed lately are listed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientHealth {
    pub client: WebClient,
    pub consecutive_failures: u32,
    /// Skipped for this much longer (the breaker is open), if at all.
    pub skipped_for: Option<Duration>,
    /// What went wrong last (English, technical).
    pub last_error: Option<String>,
}

/// What the stream resolver did lately, for display (see
/// [`super::WebResolver::set_status_hook`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolverStatus {
    /// The client that delivered the last full stream (`None`: none yet).
    pub last_client: Option<WebClient>,
    /// The last song only got the ~1 min preview (every client failed).
    pub last_was_preview: bool,
    /// Clients that failed lately, in [`WebClient::ALL`] order.
    pub clients: Vec<ClientHealth>,
    /// Serial of the player-fixes manifest in use (`None`: the built-in data).
    pub fixes_serial: Option<u64>,
}

impl ResolverStatus {
    /// Clients the breaker skips right now.
    pub fn skipped(&self) -> impl Iterator<Item = &ClientHealth> {
        self.clients.iter().filter(|h| h.skipped_for.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trips_after_three_failures_and_recovers_after_the_cooldown() {
        let b = Breakers::default();
        let t0 = Instant::now();
        let c = WebClient::Tv;
        assert!(!b.record_failure(c, "x", t0));
        assert!(!b.record_failure(c, "x", t0));
        assert!(!b.should_skip(c, t0), "two failures: still tried");
        assert!(b.record_failure(c, "boom", t0), "the third trips it");
        assert!(b.should_skip(c, t0));
        assert!(b.should_skip(c, t0 + COOLDOWN - Duration::from_secs(1)));
        assert!(!b.should_skip(WebClient::VisionOs, t0), "per client");
        let h = b.health(t0 + Duration::from_secs(60));
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].consecutive_failures, 3);
        assert_eq!(h[0].skipped_for, Some(COOLDOWN - Duration::from_secs(60)));
        assert_eq!(h[0].last_error.as_deref(), Some("boom"));

        // Half-open after the cooldown; failing again restarts it.
        let later = t0 + COOLDOWN;
        assert!(!b.should_skip(c, later));
        assert!(b.record_failure(c, "again", later));
        assert!(b.should_skip(c, later + Duration::from_secs(1)));
        // A success closes it.
        assert!(b.record_success(c));
        assert!(!b.should_skip(c, later + Duration::from_secs(1)));
        assert!(b.health(later).is_empty());
        assert!(!b.record_success(c), "nothing to change");
    }

    #[test]
    fn a_success_resets_the_count() {
        let b = Breakers::default();
        let t0 = Instant::now();
        let c = WebClient::VisionOs;
        b.record_failure(c, "x", t0);
        b.record_failure(c, "x", t0);
        b.record_success(c);
        b.record_failure(c, "x", t0);
        b.record_failure(c, "x", t0);
        assert!(!b.should_skip(c, t0));
    }

    #[test]
    fn video_specific_failures() {
        let u = |s: &str| Error::Unplayable(s.into());
        assert!(is_video_specific(&u("ERROR: Video unavailable")));
        assert!(is_video_specific(&u(
            "UNPLAYABLE: This video is not available"
        )));
        assert!(is_video_specific(&u(
            "LOGIN_REQUIRED: Sign in to confirm your age"
        )));
        assert!(is_video_specific(&u(
            "UNPLAYABLE: This video is only available to Music Premium members"
        )));
        assert!(is_video_specific(&u("LOGIN_REQUIRED: Private video")));
        assert!(is_video_specific(&Error::Http(ureq::Error::HostNotFound)));
        // The route itself being refused or broken counts.
        assert!(!is_video_specific(&u(
            "LOGIN_REQUIRED: Sign in to confirm you’re not a bot"
        )));
        assert!(!is_video_specific(&u(
            "no audio format in the player response"
        )));
        assert!(!is_video_specific(&Error::Parse(
            "challenge solver: no n function".into()
        )));
        assert!(!is_video_specific(&Error::Http(ureq::Error::StatusCode(
            403
        ))));
    }
}
