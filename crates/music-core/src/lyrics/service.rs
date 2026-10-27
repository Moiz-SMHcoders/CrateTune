//! [`LyricsService`]: queries providers in priority order on worker threads and
//! picks one result. Selection follows ArchiveTune's `LyricsHelper`: providers run
//! concurrently (bounded), better-synced results win over higher priority when
//! `prefer_word_synced` is set, ties go to priority, and once something usable
//! arrived the search only waits a short grace period for something better.

use std::collections::HashMap;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::providers::{LyricsProvider, ProviderId, ProviderKeys};
use super::types::{LyricsQuery, LyricsResult, RawLyrics, SyncKind};
use crate::{Error, Result};

/// What a cache remembers for a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CachedLyrics {
    Found(RawLyrics),
    /// Every provider answered and none had lyrics (not stored after network errors).
    NotFound,
}

/// Storage hook (e.g. the library database). Called from the thread that calls
/// [`LyricsService::fetch`]. Use [`cache_key`] or your own track id as the key.
pub trait LyricsCache: Send + Sync {
    fn load(&self, query: &LyricsQuery) -> Option<CachedLyrics>;
    fn store(&self, query: &LyricsQuery, entry: &CachedLyrics);
}

/// A default key for a query: the video id, else title/artist/duration.
pub fn cache_key(query: &LyricsQuery) -> String {
    match query.video_id.as_deref().filter(|v| !v.is_empty()) {
        Some(id) => format!("yt:{id}"),
        None => format!(
            "meta:{}\u{1f}{}\u{1f}{}",
            query.title.trim().to_lowercase(),
            query.artist.trim().to_lowercase(),
            query.duration_secs.unwrap_or(0)
        ),
    }
}

/// In-memory [`LyricsCache`] (process lifetime, unbounded).
#[derive(Debug, Default)]
pub struct MemoryLyricsCache {
    entries: Mutex<HashMap<String, CachedLyrics>>,
}

impl MemoryLyricsCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&self) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}

impl LyricsCache for MemoryLyricsCache {
    fn load(&self, query: &LyricsQuery) -> Option<CachedLyrics> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&cache_key(query))
            .cloned()
    }

    fn store(&self, query: &LyricsQuery, entry: &CachedLyrics) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(cache_key(query), entry.clone());
    }
}

/// Service settings. `Default` matches ArchiveTune's defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct LyricsServiceConfig {
    /// Priority order. Providers from [`ProviderId::DEFAULT_ORDER`] missing here are
    /// appended in default order (like ArchiveTune's saved order).
    pub order: Vec<ProviderId>,
    /// Providers never queried.
    pub disabled: Vec<ProviderId>,
    /// Moved to the front of the order.
    pub preferred: Option<ProviderId>,
    /// Network timeout per request and the longest a provider may run.
    pub provider_timeout: Duration,
    /// Upper bound for one [`LyricsService::fetch`].
    pub total_timeout: Duration,
    /// A word-synced result from a lower-priority provider beats a line-synced one
    /// from a higher-priority provider. Off: the highest-priority synced result wins.
    /// (Synced always beats plain text.)
    pub prefer_word_synced: bool,
    /// After the first usable result, wait at most this long for a better one.
    pub quality_grace: Duration,
    /// … or this long once the best possible quality was found, for higher-priority
    /// providers to finish (ArchiveTune uses 350 ms, which lets a fast lower-priority
    /// provider win most races; the default favours the configured order).
    pub priority_grace: Duration,
    /// Providers queried at once (1 = strictly one after another).
    pub max_concurrent: usize,
    /// Overrides every provider's duration-match tolerance (seconds).
    pub duration_tolerance_secs: Option<u32>,
    /// Accept unsynced (plain text) lyrics as a last resort.
    pub accept_plain: bool,
    pub keys: ProviderKeys,
}

impl Default for LyricsServiceConfig {
    fn default() -> Self {
        Self {
            order: ProviderId::DEFAULT_ORDER.to_vec(),
            disabled: Vec::new(),
            preferred: None,
            provider_timeout: Duration::from_secs(8),
            total_timeout: Duration::from_secs(12),
            prefer_word_synced: true,
            quality_grace: Duration::from_millis(1500),
            priority_grace: Duration::from_millis(2000),
            max_concurrent: 6,
            duration_tolerance_secs: None,
            accept_plain: true,
            keys: ProviderKeys::default(),
        }
    }
}

impl LyricsServiceConfig {
    /// Effective provider order: preferred first, then `order`, then missing defaults,
    /// minus disabled ones.
    pub fn resolved_order(&self) -> Vec<ProviderId> {
        let mut out: Vec<ProviderId> = Vec::new();
        let candidates = self
            .preferred
            .into_iter()
            .chain(self.order.iter().copied())
            .chain(ProviderId::DEFAULT_ORDER);
        for id in candidates {
            if !out.contains(&id) && !self.disabled.contains(&id) {
                out.push(id);
            }
        }
        out
    }
}

/// Fetches lyrics from an ordered set of providers. Cheap to share (`Arc`) and
/// callable from any thread; every call blocks until it has an answer.
pub struct LyricsService {
    providers: Vec<Arc<dyn LyricsProvider>>,
    config: LyricsServiceConfig,
    cache: Option<Arc<dyn LyricsCache>>,
}

impl Default for LyricsService {
    fn default() -> Self {
        Self::new(LyricsServiceConfig::default())
    }
}

impl std::fmt::Debug for LyricsService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LyricsService")
            .field(
                "providers",
                &self.providers.iter().map(|p| p.name()).collect::<Vec<_>>(),
            )
            .field("config", &self.config)
            .field("cache", &self.cache.is_some())
            .finish()
    }
}

struct Outcome {
    index: usize,
    result: Result<Option<LyricsResult>>,
}

impl LyricsService {
    /// Built-in providers in `config`'s order; providers missing an API key are skipped.
    pub fn new(config: LyricsServiceConfig) -> Self {
        let providers = config
            .resolved_order()
            .into_iter()
            .map(|id| id.create(&config.keys))
            .filter(|p| p.is_configured())
            .collect();
        Self {
            providers,
            config,
            cache: None,
        }
    }

    /// Custom providers, used in the given order (`config.order`/`keys` are ignored).
    pub fn with_providers(
        providers: Vec<Arc<dyn LyricsProvider>>,
        config: LyricsServiceConfig,
    ) -> Self {
        Self {
            providers: providers
                .into_iter()
                .filter(|p| p.is_configured())
                .collect(),
            config,
            cache: None,
        }
    }

    pub fn with_cache(mut self, cache: Arc<dyn LyricsCache>) -> Self {
        self.cache = Some(cache);
        self
    }

    pub fn providers(&self) -> &[Arc<dyn LyricsProvider>] {
        &self.providers
    }

    pub fn config(&self) -> &LyricsServiceConfig {
        &self.config
    }

    /// Best lyrics for `query`: cache first, then the providers (result stored in
    /// the cache). `Ok(None)` when no provider has lyrics; `Err` only when every
    /// provider failed (offline, timeouts).
    pub fn fetch(&self, query: &LyricsQuery) -> Result<Option<LyricsResult>> {
        if let Some(cache) = &self.cache {
            match cache.load(query) {
                Some(CachedLyrics::NotFound) => return Ok(None),
                Some(CachedLyrics::Found(raw)) => {
                    if let Some(r) = raw.parse(query.duration_secs) {
                        return Ok(Some(r));
                    }
                }
                None => {}
            }
        }
        self.refresh(query)
    }

    /// Like [`fetch`](Self::fetch) but skips the cache lookup (still stores the result).
    pub fn refresh(&self, query: &LyricsQuery) -> Result<Option<LyricsResult>> {
        let (result, clean) = self.fetch_best(query);
        if let Some(cache) = &self.cache {
            match &result {
                Ok(Some(r)) => cache.store(query, &CachedLyrics::Found(r.to_raw())),
                Ok(None) if clean => cache.store(query, &CachedLyrics::NotFound),
                _ => {}
            }
        }
        result
    }

    /// Queries every provider and reports each usable result as it arrives (for a
    /// "choose lyrics" list). Returns them in priority order. Ignores the cache.
    pub fn fetch_all(
        &self,
        query: &LyricsQuery,
        mut on_result: impl FnMut(&LyricsResult),
    ) -> Vec<LyricsResult> {
        let query = self.prepared(query);
        let mut run = Run::new(&self.providers, &query);
        let deadline = Instant::now() + self.config.total_timeout;
        let mut results: Vec<(usize, LyricsResult)> = Vec::new();
        loop {
            let now = Instant::now();
            run.expire(now, self.config.provider_timeout);
            run.dispatch(self.config.max_concurrent);
            if run.all_completed() || now >= deadline {
                break;
            }
            let wait = run.next_expiry(self.config.provider_timeout, deadline);
            match run.rx.recv_timeout(
                wait.saturating_duration_since(now)
                    .max(Duration::from_millis(1)),
            ) {
                Ok(o) => {
                    if run.complete(o.index)
                        && let Ok(Some(r)) = o.result
                        && let Some(r) = self.accept(r, &query)
                    {
                        on_result(&r);
                        results.push((o.index, r));
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        results.sort_by_key(|(i, _)| *i);
        results.into_iter().map(|(_, r)| r).collect()
    }

    fn prepared(&self, query: &LyricsQuery) -> LyricsQuery {
        let mut q = query.clone();
        q.timeout = Some(q.timeout.unwrap_or(self.config.provider_timeout));
        if q.duration_tolerance_secs.is_none() {
            q.duration_tolerance_secs = self.config.duration_tolerance_secs;
        }
        q
    }

    /// Rejects plain text when not wanted and synced lyrics that start after the track ends.
    fn accept(&self, r: LyricsResult, query: &LyricsQuery) -> Option<LyricsResult> {
        if r.synced.is_none() && !self.config.accept_plain {
            return None;
        }
        if let (Some(total), Some(l)) = (query.duration(), &r.synced)
            && l.lines.first().is_some_and(|first| first.start > total)
        {
            log::debug!("{}: lyrics start after the track ends; ignored", r.provider);
            return None;
        }
        Some(r)
    }

    fn quality(&self, r: &LyricsResult) -> u8 {
        match (r.sync_kind(), self.config.prefer_word_synced) {
            (SyncKind::Plain, _) => 0,
            (SyncKind::Line, _) | (SyncKind::Word, false) => 1,
            (SyncKind::Word, true) => 2,
        }
    }

    /// The selection loop. Returns the result and whether every provider answered
    /// without error (so a miss may be cached).
    fn fetch_best(&self, query: &LyricsQuery) -> (Result<Option<LyricsResult>>, bool) {
        let n = self.providers.len();
        if n == 0 {
            return (Ok(None), false);
        }
        let query = self.prepared(query);
        let mut run = Run::new(&self.providers, &query);
        let max_quality = if self.config.prefer_word_synced { 2 } else { 1 };
        let mut deadline = Instant::now() + self.config.total_timeout;
        let mut best: Option<(usize, u8, LyricsResult)> = None;
        let mut errors = 0usize;
        let mut last_error: Option<Error> = None;

        loop {
            let now = Instant::now();
            for i in run.expire(now, self.config.provider_timeout) {
                errors += 1;
                last_error = Some(Error::Parse(format!(
                    "{}: timed out",
                    self.providers[i].name()
                )));
            }
            if run.all_completed() || now >= deadline {
                break;
            }
            if let Some((bi, bq, _)) = &best
                && *bq == max_quality
                && run.completed[..*bi].iter().all(|c| *c)
            {
                break;
            }
            run.dispatch(self.config.max_concurrent);
            let wait = run.next_expiry(self.config.provider_timeout, deadline);
            match run.rx.recv_timeout(
                wait.saturating_duration_since(now)
                    .max(Duration::from_millis(1)),
            ) {
                Ok(o) => {
                    if !run.complete(o.index) {
                        continue; // already counted as timed out
                    }
                    match o.result {
                        Err(e) => {
                            errors += 1;
                            last_error = Some(e);
                        }
                        Ok(None) => {}
                        Ok(Some(r)) => {
                            if let Some(r) = self.accept(r, &query) {
                                let q = self.quality(&r);
                                let better = match &best {
                                    None => true,
                                    Some((bi, bq, _)) => q > *bq || (q == *bq && o.index < *bi),
                                };
                                if better {
                                    best = Some((o.index, q, r));
                                }
                                let grace = if best.as_ref().is_some_and(|b| b.1 == max_quality) {
                                    self.config.priority_grace
                                } else {
                                    self.config.quality_grace
                                };
                                deadline = deadline.min(Instant::now() + grace);
                            }
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        let answered = run.completed.iter().filter(|c| **c).count();
        match best {
            Some((_, _, r)) => (Ok(Some(r)), false),
            None if errors > 0 && errors == answered => (
                Err(last_error
                    .unwrap_or_else(|| Error::Parse("lyrics: all providers failed".into()))),
                false,
            ),
            None => (Ok(None), answered == n && errors == 0),
        }
    }
}

/// One fetch: the main thread dispatches providers (one thread each, at most
/// `max_concurrent` running) so it can stop before starting lower-priority ones.
/// Abandoned threads finish on their own; their sends fail silently.
struct Run {
    providers: Arc<[Arc<dyn LyricsProvider>]>,
    query: Arc<LyricsQuery>,
    tx: mpsc::Sender<Outcome>,
    rx: mpsc::Receiver<Outcome>,
    next: usize,
    running: usize,
    started: Vec<Option<Instant>>,
    completed: Vec<bool>,
}

impl Run {
    fn new(providers: &[Arc<dyn LyricsProvider>], query: &LyricsQuery) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            providers: providers.to_vec().into(),
            query: Arc::new(query.clone()),
            tx,
            rx,
            next: 0,
            running: 0,
            started: vec![None; providers.len()],
            completed: vec![false; providers.len()],
        }
    }

    fn dispatch(&mut self, max_concurrent: usize) {
        while self.running < max_concurrent.max(1) && self.next < self.providers.len() {
            let index = self.next;
            self.next += 1;
            let (provider, query, tx) = (
                self.providers[index].clone(),
                self.query.clone(),
                self.tx.clone(),
            );
            let spawned = thread::Builder::new()
                .name("lyrics-provider".into())
                .spawn(move || {
                    let result = provider.fetch(&query);
                    if let Err(e) = &result {
                        log::debug!("lyrics provider {}: {e}", provider.name());
                    }
                    let _ = tx.send(Outcome { index, result });
                });
            match spawned {
                Ok(_) => {
                    self.started[index] = Some(Instant::now());
                    self.running += 1;
                }
                Err(e) => {
                    log::warn!("lyrics: cannot spawn worker: {e}");
                    self.completed[index] = true;
                }
            }
        }
    }

    /// Marks `index` finished; false if it already was (timed out earlier).
    fn complete(&mut self, index: usize) -> bool {
        if std::mem::replace(&mut self.completed[index], true) {
            return false;
        }
        self.running -= 1;
        true
    }

    /// Marks providers running past `timeout` as finished; returns their indices.
    fn expire(&mut self, now: Instant, timeout: Duration) -> Vec<usize> {
        let expired: Vec<usize> = (0..self.providers.len())
            .filter(|&i| !self.completed[i] && self.started[i].is_some_and(|s| now >= s + timeout))
            .collect();
        for &i in &expired {
            self.complete(i);
        }
        expired
    }

    fn next_expiry(&self, timeout: Duration, deadline: Instant) -> Instant {
        self.started
            .iter()
            .zip(&self.completed)
            .filter(|(_, c)| !**c)
            .filter_map(|(s, _)| s.map(|s| s + timeout))
            .min()
            .map_or(deadline, |t| t.min(deadline))
    }

    fn all_completed(&self) -> bool {
        self.completed.iter().all(|c| *c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::LyricsFormat;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Mock {
        name: &'static str,
        delay: Duration,
        reply: Option<&'static str>,
        fail: bool,
        calls: AtomicUsize,
    }

    impl Mock {
        fn new(name: &'static str, delay_ms: u64, reply: Option<&'static str>) -> Arc<Self> {
            Arc::new(Self {
                name,
                delay: Duration::from_millis(delay_ms),
                reply,
                fail: false,
                calls: AtomicUsize::new(0),
            })
        }

        fn failing(name: &'static str) -> Arc<Self> {
            Arc::new(Self {
                name,
                delay: Duration::ZERO,
                reply: None,
                fail: true,
                calls: AtomicUsize::new(0),
            })
        }
    }

    impl LyricsProvider for Mock {
        fn name(&self) -> &str {
            self.name
        }

        fn fetch(&self, _: &LyricsQuery) -> Result<Option<LyricsResult>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            thread::sleep(self.delay);
            if self.fail {
                return Err(Error::Parse("offline".into()));
            }
            Ok(self
                .reply
                .and_then(|r| LyricsResult::parse(self.name, r, None)))
        }
    }

    const PLAIN: &str = "just text";
    const LINE: &str = "[00:01.00]line synced";
    const WORD: &str = "[00:01.00]<00:01.00>word <00:01.50>synced";

    fn service(providers: Vec<Arc<Mock>>, config: LyricsServiceConfig) -> LyricsService {
        LyricsService::with_providers(
            providers
                .into_iter()
                .map(|p| p as Arc<dyn LyricsProvider>)
                .collect(),
            config,
        )
    }

    fn q() -> LyricsQuery {
        LyricsQuery::new("t", "a")
    }

    #[test]
    fn word_synced_beats_priority() {
        let s = service(
            vec![
                Mock::new("a", 0, Some(LINE)),
                Mock::new("b", 30, Some(WORD)),
            ],
            LyricsServiceConfig::default(),
        );
        let r = s.fetch(&q()).unwrap().unwrap();
        assert_eq!(r.provider, "b");
        assert_eq!(r.sync_kind(), SyncKind::Word);
    }

    #[test]
    fn priority_wins_without_word_preference() {
        let s = service(
            vec![
                Mock::new("a", 30, Some(LINE)),
                Mock::new("b", 0, Some(WORD)),
            ],
            LyricsServiceConfig {
                prefer_word_synced: false,
                ..LyricsServiceConfig::default()
            },
        );
        assert_eq!(s.fetch(&q()).unwrap().unwrap().provider, "a");
    }

    #[test]
    fn synced_beats_plain_and_plain_is_fallback() {
        let s = service(
            vec![
                Mock::new("a", 0, Some(PLAIN)),
                Mock::new("b", 0, Some(LINE)),
            ],
            LyricsServiceConfig::default(),
        );
        assert_eq!(s.fetch(&q()).unwrap().unwrap().provider, "b");
        let s = service(
            vec![Mock::new("a", 0, None), Mock::new("b", 0, Some(PLAIN))],
            LyricsServiceConfig::default(),
        );
        assert_eq!(s.fetch(&q()).unwrap().unwrap().format, LyricsFormat::Plain);
        let s = service(
            vec![Mock::new("b", 0, Some(PLAIN))],
            LyricsServiceConfig {
                accept_plain: false,
                ..LyricsServiceConfig::default()
            },
        );
        assert!(s.fetch(&q()).unwrap().is_none());
    }

    #[test]
    fn stops_early_and_respects_timeouts() {
        let slow = Mock::new("slow", 3000, Some(LINE));
        let s = service(
            vec![Mock::new("fast", 0, Some(WORD)), slow.clone()],
            LyricsServiceConfig::default(),
        );
        let t = Instant::now();
        assert_eq!(s.fetch(&q()).unwrap().unwrap().provider, "fast");
        assert!(t.elapsed() < Duration::from_millis(1000));

        // A provider that exceeds its timeout is abandoned.
        let s = service(
            vec![slow, Mock::new("b", 0, Some(LINE))],
            LyricsServiceConfig {
                provider_timeout: Duration::from_millis(100),
                ..LyricsServiceConfig::default()
            },
        );
        let t = Instant::now();
        assert_eq!(s.fetch(&q()).unwrap().unwrap().provider, "b");
        assert!(t.elapsed() < Duration::from_millis(1000));
    }

    #[test]
    fn sequential_mode_stops_at_first_word_synced() {
        let third = Mock::new("c", 0, Some(WORD));
        let s = service(
            vec![
                Mock::new("a", 0, None),
                Mock::new("b", 0, Some(WORD)),
                third.clone(),
            ],
            LyricsServiceConfig {
                max_concurrent: 1,
                ..LyricsServiceConfig::default()
            },
        );
        assert_eq!(s.fetch(&q()).unwrap().unwrap().provider, "b");
        thread::sleep(Duration::from_millis(50));
        assert_eq!(third.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn errors_and_cache() {
        let s = service(
            vec![Mock::failing("x"), Mock::failing("y")],
            LyricsServiceConfig::default(),
        );
        assert!(s.fetch(&q()).is_err());

        let cache = Arc::new(MemoryLyricsCache::new());
        let provider = Mock::new("a", 0, Some(LINE));
        let s = service(vec![provider.clone()], LyricsServiceConfig::default())
            .with_cache(cache.clone());
        assert!(s.fetch(&q()).unwrap().is_some());
        assert!(s.fetch(&q()).unwrap().is_some());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert!(matches!(cache.load(&q()), Some(CachedLyrics::Found(r)) if r.provider == "a"));

        let missing = Mock::new("none", 0, None);
        let s = service(vec![missing.clone()], LyricsServiceConfig::default())
            .with_cache(cache.clone());
        let other = LyricsQuery::new("other", "song");
        assert!(s.fetch(&other).unwrap().is_none());
        assert!(s.fetch(&other).unwrap().is_none());
        assert_eq!(missing.calls.load(Ordering::SeqCst), 1);
        assert_eq!(cache.load(&other), Some(CachedLyrics::NotFound));

        // Errors are not cached as "not found".
        let s = service(vec![Mock::failing("x")], LyricsServiceConfig::default())
            .with_cache(cache.clone());
        let third = LyricsQuery::new("third", "song");
        assert!(s.fetch(&third).is_err());
        assert_eq!(cache.load(&third), None);
    }

    #[test]
    fn fetch_all_in_priority_order() {
        let s = service(
            vec![
                Mock::new("a", 40, Some(LINE)),
                Mock::new("b", 0, None),
                Mock::new("c", 0, Some(WORD)),
            ],
            LyricsServiceConfig::default(),
        );
        let mut seen = Vec::new();
        let all = s.fetch_all(&q(), |r| seen.push(r.provider.clone()));
        assert_eq!(seen, ["c", "a"]);
        assert_eq!(
            all.iter().map(|r| r.provider.as_str()).collect::<Vec<_>>(),
            ["a", "c"]
        );
    }

    #[test]
    fn config_order() {
        let c = LyricsServiceConfig {
            order: vec![ProviderId::LrcLib, ProviderId::KuGou],
            disabled: vec![ProviderId::Megalobiz],
            preferred: Some(ProviderId::YouTube),
            ..LyricsServiceConfig::default()
        };
        let order = c.resolved_order();
        assert_eq!(
            &order[..4],
            &[
                ProviderId::YouTube,
                ProviderId::LrcLib,
                ProviderId::KuGou,
                ProviderId::BetterLyrics
            ]
        );
        assert!(!order.contains(&ProviderId::Megalobiz));
        assert_eq!(order.len(), ProviderId::DEFAULT_ORDER.len() - 1);
        // Without a Paxsenix key those providers are skipped.
        let s = LyricsService::new(LyricsServiceConfig::default());
        assert!(
            s.providers()
                .iter()
                .all(|p| !p.name().starts_with("Paxsenix"))
        );
        assert_eq!(s.providers()[0].name(), "BetterLyrics");
    }
}
