//! Resolved stream URLs kept for the next track ([`crate::StreamResolver::prefetch_track`]),
//! so it starts without a resolve (and without a pre-roll ad wait).
//!
//! googlevideo URLs carry an `expire` time (~6 h), but a signed link is only
//! kept for [`MAX_AGE`] (as Deviload does) and never closer than
//! [`EXPIRY_MARGIN`] to its expiry; anything older is resolved again. An entry
//! is handed out once.

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use crate::{Quality, ResolvedStream};

/// A resolved URL is used at most this long after it was resolved.
pub(crate) const MAX_AGE: Duration = Duration::from_secs(60 * 60);
/// ...and not when it expires within this.
pub(crate) const EXPIRY_MARGIN: Duration = Duration::from_secs(10 * 60);
/// Entries kept (the next track, plus a few the queue moved past).
const CAPACITY: usize = 4;

struct Entry {
    video_id: String,
    quality: Quality,
    stream: ResolvedStream,
    resolved_at: SystemTime,
}

impl Entry {
    fn fresh(&self, now: SystemTime) -> bool {
        let young = now
            .duration_since(self.resolved_at)
            .is_ok_and(|age| age < MAX_AGE);
        let valid = self
            .stream
            .expires_at()
            .is_none_or(|at| now + EXPIRY_MARGIN < at);
        young && valid
    }

    /// googlevideo already accepts the URL (its pre-roll ad wait is over).
    fn usable(&self, now: SystemTime) -> bool {
        self.stream.not_before.is_none_or(|at| at <= now)
    }
}

#[derive(Default)]
pub(crate) struct ResolvedCache {
    entries: Mutex<VecDeque<Entry>>,
}

impl ResolvedCache {
    pub(crate) fn put(
        &self,
        video_id: &str,
        quality: Quality,
        stream: ResolvedStream,
        now: SystemTime,
    ) {
        let mut entries = self.lock();
        entries.retain(|e| !(e.video_id == video_id && e.quality == quality) && e.fresh(now));
        while entries.len() >= CAPACITY {
            entries.pop_front();
        }
        entries.push_back(Entry {
            video_id: video_id.to_owned(),
            quality,
            // No preview head: the URL is usable by the time it is needed.
            stream: ResolvedStream {
                prefix: None,
                ..stream
            },
            resolved_at: now,
        });
    }

    /// A fresh, usable stream for `video_id` at `quality`, removed from the
    /// cache. A stale one is dropped (the caller resolves again); one still in
    /// its ad wait stays for later (the caller resolves with a preview head now).
    pub(crate) fn take(
        &self,
        video_id: &str,
        quality: Quality,
        now: SystemTime,
    ) -> Option<ResolvedStream> {
        let mut entries = self.lock();
        let i = entries
            .iter()
            .position(|e| e.video_id == video_id && e.quality == quality)?;
        if !entries[i].fresh(now) {
            log::debug!("youtube: {video_id}: prefetched URL is stale; resolving again");
            entries.remove(i);
            return None;
        }
        if !entries[i].usable(now) {
            return None;
        }
        entries.remove(i).map(|e| e.stream)
    }

    /// Whether a fresh entry exists (no need to prefetch again).
    pub(crate) fn has_fresh(&self, video_id: &str, quality: Quality, now: SystemTime) -> bool {
        self.lock()
            .iter()
            .any(|e| e.video_id == video_id && e.quality == quality && e.fresh(now))
    }

    pub(crate) fn clear(&self) {
        self.lock().clear();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn stream(expire: SystemTime) -> ResolvedStream {
        let secs = expire.duration_since(UNIX_EPOCH).unwrap().as_secs();
        ResolvedStream {
            url: format!("https://r.googlevideo.com/videoplayback?expire={secs}&itag=251"),
            itag: 251,
            ..Default::default()
        }
    }

    #[test]
    fn serves_a_fresh_url_once() {
        let cache = ResolvedCache::default();
        let now = SystemTime::now();
        cache.put(
            "a",
            Quality::High,
            stream(now + Duration::from_secs(6 * 3600)),
            now,
        );
        assert!(cache.has_fresh("a", Quality::High, now));
        assert!(
            cache.take("a", Quality::Normal, now).is_none(),
            "per quality"
        );
        assert!(cache.take("b", Quality::High, now).is_none());
        let later = now + Duration::from_secs(30 * 60);
        assert_eq!(cache.take("a", Quality::High, later).unwrap().itag, 251);
        assert!(
            cache.take("a", Quality::High, later).is_none(),
            "handed out once"
        );
    }

    #[test]
    fn re_resolves_after_an_hour_or_near_expiry() {
        let cache = ResolvedCache::default();
        let now = SystemTime::now();
        // Valid for 6 h, but kept 1 h at most.
        cache.put(
            "a",
            Quality::High,
            stream(now + Duration::from_secs(6 * 3600)),
            now,
        );
        let after_hour = now + MAX_AGE + Duration::from_secs(1);
        assert!(!cache.has_fresh("a", Quality::High, after_hour));
        assert!(cache.take("a", Quality::High, after_hour).is_none());
        assert!(
            cache.take("a", Quality::High, now).is_none(),
            "a stale entry is dropped"
        );

        // googlevideo's `expire` is sooner than an hour away.
        cache.put(
            "b",
            Quality::High,
            stream(now + Duration::from_secs(15 * 60)),
            now,
        );
        assert!(cache.take("b", Quality::High, now).is_some());
        cache.put(
            "b",
            Quality::High,
            stream(now + Duration::from_secs(15 * 60)),
            now,
        );
        assert!(
            cache
                .take("b", Quality::High, now + Duration::from_secs(6 * 60))
                .is_none(),
            "within 10 min of its expiry"
        );

        // A module-style declared expiry counts the same.
        let declared = ResolvedStream {
            valid_until: Some(now + Duration::from_secs(5 * 60)),
            ..Default::default()
        };
        cache.put("c", Quality::High, declared, now);
        assert!(cache.take("c", Quality::High, now).is_none());
    }

    #[test]
    fn waits_for_the_ad_and_keeps_a_bounded_set() {
        let cache = ResolvedCache::default();
        let now = SystemTime::now();
        let waiting = ResolvedStream {
            not_before: Some(now + Duration::from_secs(10)),
            ..stream(now + Duration::from_secs(6 * 3600))
        };
        cache.put("a", Quality::Normal, waiting, now);
        assert!(
            cache.take("a", Quality::Normal, now).is_none(),
            "not usable yet"
        );
        assert!(
            cache
                .take("a", Quality::Normal, now + Duration::from_secs(11))
                .is_some(),
            "kept until it is"
        );
        for id in ["1", "2", "3", "4", "5"] {
            cache.put(
                id,
                Quality::Normal,
                stream(now + Duration::from_secs(6 * 3600)),
                now,
            );
        }
        assert!(
            !cache.has_fresh("1", Quality::Normal, now),
            "oldest evicted"
        );
        assert!(cache.has_fresh("5", Quality::Normal, now));
        cache.clear();
        assert!(!cache.has_fresh("5", Quality::Normal, now));
    }
}
