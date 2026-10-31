//! [`LyricsService`]: asks providers on worker threads, in waves, and picks one
//! result. Selection started from ArchiveTune's `LyricsHelper` (bounded concurrency,
//! better-synced results over priority, a short grace period for something better);
//! the waves (fast, good sources first; the next wave only without a good synced
//! result) follow Zuno's lyrics source table, and ranking by the matched
//! recording's length (closest cut first) follows Limusic's `best_by_duration`.
//! Zuno: © noFAYZ, Apache-2.0. Limusic: © SimoHypers, GPL-3.0.

use std::cmp::Reverse;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::providers::{LyricsProvider, ProviderId, ProviderKeys, set_cancel_flag};
use super::types::{DurationFit, LyricsQuery, LyricsResult, RawLyrics, SyncKind};
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

/// Service settings. `Default` asks the fast, good providers first (see
/// [`ProviderId::default_wave`]).
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
    /// Upper bound for one search.
    pub total_timeout: Duration,
    /// A word-synced result beats a line-synced one from a higher-priority provider
    /// (after length and names). Off: priority decides between synced results.
    pub prefer_word_synced: bool,
    /// Rank results by how well the matched recording's length fits the track
    /// (within [`super::EXACT_DURATION_SECS`]) before anything but synced-vs-plain, so
    /// another cut (radio edit, extended mix, live version) is only a fallback.
    pub match_duration: bool,
    /// Ask providers in waves ([`ProviderId::default_wave`]): the next wave starts
    /// only when the previous one has no good synced result (or ran past
    /// `wave_timeout`). Off: every provider at once (up to `max_concurrent`).
    pub waves: bool,
    /// How long a wave may run before the next one starts alongside it.
    pub wave_timeout: Duration,
    /// After a good result, wait at most this long for a better one…
    pub quality_grace: Duration,
    /// …or this long when it is as good as it gets, for higher-priority providers
    /// still running.
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
            match_duration: true,
            waves: true,
            wave_timeout: Duration::from_millis(2500),
            quality_grace: Duration::from_millis(800),
            priority_grace: Duration::from_millis(300),
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

/// What one provider did during a search (for a "sources tried" list).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptStatus {
    /// Usable lyrics.
    Found {
        sync: SyncKind,
        fit: DurationFit,
    },
    NotFound,
    /// Lyrics that cannot be used: only credits, starting after the track ends, or
    /// plain text when it is not wanted.
    Unusable,
    /// The provider says the song is instrumental.
    Instrumental,
    Failed(String),
    /// No answer within the provider timeout.
    TimedOut,
    /// Still running when a result was picked: stopped.
    Stopped,
    /// Not asked: an earlier wave had good lyrics (or time ran out).
    NotAsked,
}

/// One provider's line in [`LyricsSearch::attempts`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceAttempt {
    /// [`LyricsProvider::name`].
    pub provider: String,
    /// The wave it belongs to (1 = asked first).
    pub wave: u8,
    pub status: AttemptStatus,
    /// From the request to the answer (or to being stopped); `None` when not asked.
    pub elapsed: Option<Duration>,
}

/// Options for [`LyricsService::search`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchOptions {
    /// Skip the cache lookup (the result is still stored).
    pub refresh: bool,
    /// A provider ([`LyricsProvider::name`]) picked for this track: it joins the
    /// first wave and its lyrics win when it has some.
    pub pinned: Option<String>,
}

/// Everything a search found out.
#[derive(Debug)]
pub struct LyricsSearch {
    /// The pick. `Ok(None)` when no provider has lyrics; `Err` only when every
    /// provider asked failed (offline, timeouts).
    pub result: Result<Option<LyricsResult>>,
    /// Other providers' usable lyrics that arrived, best first (to switch to).
    pub alternates: Vec<LyricsResult>,
    /// Every provider, in the order they were asked (not-asked ones last).
    pub attempts: Vec<SourceAttempt>,
    /// Answered from the cache: no provider was asked.
    pub from_cache: bool,
    pub elapsed: Duration,
}

/// Fetches lyrics from an ordered set of providers, asked in waves. Cheap to share
/// (`Arc`) and callable from any thread; every call blocks until it has an answer.
///
/// A search starts the first wave's providers in parallel. Results are ranked:
/// pinned provider, synced over plain, then (with `match_duration`) how well the
/// matched recording's length fits the track ([`DurationFit`]), whether the matched
/// title/artist resemble the track's, word over line timing, and finally the
/// configured priority. A *good* result (synced, not another cut, the right names)
/// stops the search after a short grace for something better; providers still
/// running are cancelled (their remaining requests are skipped). Without a good
/// result, the next wave starts when the current one is done or has run for
/// `wave_timeout`.
pub struct LyricsService {
    providers: Vec<Arc<dyn LyricsProvider>>,
    /// Wave of each provider (same index).
    waves: Vec<u8>,
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
            .field("waves", &self.waves)
            .field("config", &self.config)
            .field("cache", &self.cache.is_some())
            .finish()
    }
}

struct Outcome {
    index: usize,
    result: Result<Option<LyricsResult>>,
}

/// How a result ranks (derived `Ord`: field order is significance).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Rank {
    pinned: bool,
    synced: bool,
    fit: DurationFit,
    names: bool,
    quality: u8,
    priority: Reverse<usize>,
}

impl Rank {
    /// Synced, not another cut, the right song: no need for later waves.
    fn good(&self) -> bool {
        self.synced && self.fit != DurationFit::Different && self.names
    }
}

/// Names below this similarity (0..=1) are another song.
const NAME_MATCH: f64 = 0.6;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    Waiting,
    Running(Instant),
    Done,
}

impl LyricsService {
    /// Built-in providers in `config`'s order; providers missing an API key are
    /// skipped. With `config.waves`, each one is asked in its
    /// [`ProviderId::default_wave`], the first provider of the order in wave 1.
    pub fn new(config: LyricsServiceConfig) -> Self {
        let order = config.resolved_order();
        let mut providers = Vec::new();
        let mut waves = Vec::new();
        for (i, id) in order.into_iter().enumerate() {
            let p = id.create(&config.keys);
            if !p.is_configured() {
                continue;
            }
            providers.push(p);
            waves.push(match (config.waves, i) {
                (false, _) | (true, 0) => 1,
                (true, _) => id.default_wave(),
            });
        }
        Self {
            providers,
            waves,
            config,
            cache: None,
        }
    }

    /// Custom providers, used in the given order and all in one wave
    /// (`config.order`/`keys` are ignored).
    pub fn with_providers(
        providers: Vec<Arc<dyn LyricsProvider>>,
        config: LyricsServiceConfig,
    ) -> Self {
        Self::with_waved_providers(providers.into_iter().map(|p| (p, 1)).collect(), config)
    }

    /// Custom providers with their wave (1 = asked first), in priority order.
    pub fn with_waved_providers(
        providers: Vec<(Arc<dyn LyricsProvider>, u8)>,
        config: LyricsServiceConfig,
    ) -> Self {
        let (providers, waves) = providers
            .into_iter()
            .filter(|(p, _)| p.is_configured())
            .map(|(p, w)| (p, if config.waves { w.max(1) } else { 1 }))
            .unzip();
        Self {
            providers,
            waves,
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

    /// Wave of each of [`providers`](Self::providers).
    pub fn waves(&self) -> &[u8] {
        &self.waves
    }

    pub fn config(&self) -> &LyricsServiceConfig {
        &self.config
    }

    /// Best lyrics for `query`: cache first, then the providers (result stored in
    /// the cache). `Ok(None)` when no provider has lyrics; `Err` only when every
    /// provider failed (offline, timeouts).
    pub fn fetch(&self, query: &LyricsQuery) -> Result<Option<LyricsResult>> {
        self.search(query, &SearchOptions::default()).result
    }

    /// Like [`fetch`](Self::fetch) but skips the cache lookup (still stores the result).
    pub fn refresh(&self, query: &LyricsQuery) -> Result<Option<LyricsResult>> {
        self.search(
            query,
            &SearchOptions {
                refresh: true,
                ..SearchOptions::default()
            },
        )
        .result
    }

    /// The full search: cache (unless `refresh`, or it holds another provider's
    /// lyrics than the pinned one), then the providers in waves. Also reports what
    /// each provider did and the other usable lyrics found on the way.
    pub fn search(&self, query: &LyricsQuery, options: &SearchOptions) -> LyricsSearch {
        let started = Instant::now();
        if !options.refresh
            && let Some(cache) = &self.cache
        {
            let wanted = |provider: &str| options.pinned.as_deref().is_none_or(|p| p == provider);
            let cached = match cache.load(query) {
                Some(CachedLyrics::NotFound) if options.pinned.is_none() => Some(Ok(None)),
                Some(CachedLyrics::Found(raw)) if wanted(&raw.provider) => raw
                    .parse(query.duration_secs)
                    .and_then(|r| r.without_credits(query))
                    .map(|r| Ok(Some(r))),
                _ => None,
            };
            if let Some(result) = cached {
                return LyricsSearch {
                    result,
                    alternates: Vec::new(),
                    attempts: Vec::new(),
                    from_cache: true,
                    elapsed: started.elapsed(),
                };
            }
        }
        let pinned = options
            .pinned
            .as_deref()
            .and_then(|name| self.providers.iter().position(|p| p.name() == name));
        let race = self.race(query, pinned, false, &mut |_| {});
        if let Some(cache) = &self.cache {
            match &race.result {
                Ok(Some(r)) => cache.store(query, &CachedLyrics::Found(r.to_raw())),
                Ok(None) if race.clean => cache.store(query, &CachedLyrics::NotFound),
                _ => {}
            }
        }
        LyricsSearch {
            result: race.result,
            alternates: race.alternates,
            attempts: race.attempts,
            from_cache: false,
            elapsed: started.elapsed(),
        }
    }

    /// Asks one provider (by [`LyricsProvider::name`]) on this thread, e.g. to try a
    /// source a search did not need. Ignores the cache.
    pub fn fetch_from(
        &self,
        query: &LyricsQuery,
        provider: &str,
    ) -> (Result<Option<LyricsResult>>, SourceAttempt) {
        let Some(index) = self.providers.iter().position(|p| p.name() == provider) else {
            let attempt = SourceAttempt {
                provider: provider.to_owned(),
                wave: 0,
                status: AttemptStatus::NotAsked,
                elapsed: None,
            };
            return (Ok(None), attempt);
        };
        let query = self.prepared(query);
        let started = Instant::now();
        let fetched = self.providers[index].fetch(&query);
        let (result, status) = match fetched {
            Err(e) => {
                let status = AttemptStatus::Failed(e.to_string());
                (Err(e), status)
            }
            Ok(None) => (Ok(None), AttemptStatus::NotFound),
            Ok(Some(r)) if r.is_instrumental() => (Ok(None), AttemptStatus::Instrumental),
            Ok(Some(r)) => match self.accept(r, &query) {
                Some(r) => {
                    let status = AttemptStatus::Found {
                        sync: r.sync_kind(),
                        fit: r.duration_fit(&query),
                    };
                    (Ok(Some(r)), status)
                }
                None => (Ok(None), AttemptStatus::Unusable),
            },
        };
        let attempt = SourceAttempt {
            provider: provider.to_owned(),
            wave: self.waves[index],
            status,
            elapsed: Some(started.elapsed()),
        };
        (result, attempt)
    }

    /// Queries every provider and reports each usable result as it arrives (for a
    /// "choose lyrics" list). Returns them in priority order. Ignores the cache.
    pub fn fetch_all(
        &self,
        query: &LyricsQuery,
        mut on_result: impl FnMut(&LyricsResult),
    ) -> Vec<LyricsResult> {
        let race = self.race(query, None, true, &mut on_result);
        let mut all: Vec<LyricsResult> = race.result.ok().flatten().into_iter().collect();
        all.extend(race.alternates);
        let index = |r: &LyricsResult| {
            self.providers
                .iter()
                .position(|p| p.name() == r.provider)
                .unwrap_or(usize::MAX)
        };
        all.sort_by_key(index);
        all
    }

    fn prepared(&self, query: &LyricsQuery) -> LyricsQuery {
        let mut q = query.clone();
        q.timeout = Some(q.timeout.unwrap_or(self.config.provider_timeout));
        if q.duration_tolerance_secs.is_none() {
            q.duration_tolerance_secs = self.config.duration_tolerance_secs;
        }
        q
    }

    /// Drops credit lines; rejects instrumental markers, plain text when not wanted and
    /// synced lyrics that start after the track ends.
    fn accept(&self, r: LyricsResult, query: &LyricsQuery) -> Option<LyricsResult> {
        if r.is_instrumental() {
            return None;
        }
        let r = r.without_credits(query)?;
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

    fn rank(&self, r: &LyricsResult, index: usize, pinned: Option<usize>, q: &LyricsQuery) -> Rank {
        let synced = r.synced.is_some();
        Rank {
            pinned: pinned == Some(index),
            synced,
            fit: if self.config.match_duration {
                r.duration_fit(q)
            } else {
                DurationFit::Unknown
            },
            names: r.name_match(q) >= NAME_MATCH,
            quality: match (r.sync_kind(), self.config.prefer_word_synced) {
                (SyncKind::Plain, _) => 0,
                (SyncKind::Line, _) | (SyncKind::Word, false) => 1,
                (SyncKind::Word, true) => 2,
            },
            priority: Reverse(index),
        }
    }

    /// The search itself. `exhaustive` asks every wave and never stops early
    /// (`fetch_all`). Every accepted result is passed to `on_found`.
    fn race(
        &self,
        query: &LyricsQuery,
        pinned: Option<usize>,
        exhaustive: bool,
        on_found: &mut dyn FnMut(&LyricsResult),
    ) -> Race {
        let n = self.providers.len();
        let query = self.prepared(query);
        let max_quality = if self.config.prefer_word_synced { 2 } else { 1 };
        // The best fit any result can have for this track.
        let best_fit = if self.config.match_duration && query.duration_secs.is_some_and(|d| d > 0) {
            DurationFit::Exact
        } else {
            DurationFit::Unknown
        };
        let mut waves = self.waves.clone();
        if let (Some(p), Some(first)) = (pinned, waves.iter().copied().min()) {
            waves[p] = first;
        }
        let mut wave_numbers: Vec<u8> = waves.clone();
        wave_numbers.sort_unstable();
        wave_numbers.dedup();

        let shared_query = Arc::new(query.clone());
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<Outcome>();
        let mut slots = vec![Slot::Waiting; n];
        let mut statuses: Vec<Option<AttemptStatus>> = vec![None; n];
        let mut elapsed: Vec<Option<Duration>> = vec![None; n];
        let mut queue: VecDeque<usize> = VecDeque::new();
        let mut wave_at = 0usize;
        let start = Instant::now();
        let total_deadline = start + self.config.total_timeout;
        let mut wave_deadline = start + self.config.wave_timeout;
        if let Some(&w) = wave_numbers.first() {
            queue.extend((0..n).filter(|&i| waves[i] == w));
        }
        let mut running = 0usize;
        let mut found: Vec<(Rank, usize, LyricsResult)> = Vec::new();
        let mut grace: Option<Instant> = None;
        let mut errors = 0usize;
        let mut last_error: Option<Error> = None;
        // Providers that say the song is instrumental ("纯音乐，请欣赏").
        let mut instrumental = 0usize;

        loop {
            let now = Instant::now();
            for i in 0..n {
                if let Slot::Running(s) = slots[i]
                    && now >= s + self.config.provider_timeout
                {
                    slots[i] = Slot::Done;
                    running -= 1;
                    errors += 1;
                    statuses[i] = Some(AttemptStatus::TimedOut);
                    elapsed[i] = Some(now - s);
                    last_error = Some(Error::Parse(format!(
                        "{}: timed out",
                        self.providers[i].name()
                    )));
                }
            }
            let best = found
                .iter()
                .map(|(r, i, _)| (*r, *i))
                .max()
                .filter(|(r, _)| !exhaustive && r.good());
            let good = best.is_some();
            if let Some((rank, index)) = best {
                // As good as it gets, and nobody ahead of it in the order is still out.
                let ahead_pending =
                    (0..index).any(|i| matches!(slots[i], Slot::Running(_)) || queue.contains(&i));
                let top = rank.fit >= best_fit && rank.quality == max_quality && !ahead_pending;
                let pinned_waiting = pinned.is_some_and(|p| slots[p] != Slot::Done);
                let grace_over = grace.is_some_and(|g| now >= g);
                if !pinned_waiting && (rank.pinned || top || grace_over) {
                    break;
                }
            }
            if now >= total_deadline {
                break;
            }
            let more_waves = wave_at + 1 < wave_numbers.len();
            let wave_done = queue.is_empty() && running == 0;
            if !good && more_waves && (wave_done || now >= wave_deadline) {
                wave_at += 1;
                let w = wave_numbers[wave_at];
                queue.extend((0..n).filter(|&i| waves[i] == w));
                wave_deadline = now + self.config.wave_timeout;
                continue;
            }
            if wave_done && (good || !more_waves) {
                break;
            }
            // Start what the cap allows.
            while running < self.config.max_concurrent.max(1)
                && let Some(index) = queue.pop_front()
            {
                let (provider, q, tx, flag) = (
                    self.providers[index].clone(),
                    shared_query.clone(),
                    tx.clone(),
                    cancel.clone(),
                );
                let spawned =
                    thread::Builder::new()
                        .name("lyrics-provider".into())
                        .spawn(move || {
                            set_cancel_flag(Some(flag));
                            let result = provider.fetch(&q);
                            if let Err(e) = &result {
                                log::debug!("lyrics provider {}: {e}", provider.name());
                            }
                            let _ = tx.send(Outcome { index, result });
                        });
                match spawned {
                    Ok(_) => {
                        slots[index] = Slot::Running(Instant::now());
                        running += 1;
                    }
                    Err(e) => {
                        log::warn!("lyrics: cannot spawn worker: {e}");
                        slots[index] = Slot::Done;
                        statuses[index] = Some(AttemptStatus::Failed(e.to_string()));
                    }
                }
            }
            if running == 0 && queue.is_empty() {
                continue; // a wave whose providers could not start: next wave / done
            }
            let mut wake = total_deadline;
            for s in &slots {
                if let Slot::Running(s) = s {
                    wake = wake.min(*s + self.config.provider_timeout);
                }
            }
            if let Some(g) = grace {
                wake = wake.min(g);
            }
            if !good && more_waves {
                wake = wake.min(wave_deadline);
            }
            let wait = wake
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1));
            let o = match rx.recv_timeout(wait) {
                Ok(o) => o,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            };
            let Slot::Running(s) = slots[o.index] else {
                continue; // already counted as timed out
            };
            slots[o.index] = Slot::Done;
            running -= 1;
            elapsed[o.index] = Some(s.elapsed());
            statuses[o.index] = Some(match o.result {
                Err(e) => {
                    errors += 1;
                    let status = AttemptStatus::Failed(e.to_string());
                    last_error = Some(e);
                    status
                }
                Ok(None) => AttemptStatus::NotFound,
                Ok(Some(r)) if r.is_instrumental() => {
                    log::debug!("{}: instrumental", r.provider);
                    instrumental += 1;
                    AttemptStatus::Instrumental
                }
                Ok(Some(r)) => match self.accept(r, &query) {
                    Some(r) => {
                        let rank = self.rank(&r, o.index, pinned, &query);
                        on_found(&r);
                        let status = AttemptStatus::Found {
                            sync: r.sync_kind(),
                            fit: r.duration_fit(&query),
                        };
                        if rank.good() {
                            let wait = if rank.fit >= best_fit && rank.quality == max_quality {
                                self.config.priority_grace
                            } else {
                                self.config.quality_grace
                            };
                            let until = Instant::now() + wait;
                            grace = Some(grace.map_or(until, |g| g.min(until)));
                        }
                        found.push((rank, o.index, r));
                        status
                    }
                    None => AttemptStatus::Unusable,
                },
            });
        }
        // Whatever still runs is not needed: skip its remaining requests.
        cancel.store(true, Ordering::Relaxed);
        let now = Instant::now();
        for i in 0..n {
            match slots[i] {
                Slot::Running(s) => {
                    statuses[i] = Some(AttemptStatus::Stopped);
                    elapsed[i] = Some(now - s);
                }
                Slot::Waiting => statuses[i] = Some(AttemptStatus::NotAsked),
                Slot::Done => {}
            }
        }
        let answered = slots.iter().filter(|s| **s == Slot::Done).count();
        let all_answered_cleanly = statuses.iter().all(|s| {
            matches!(
                s,
                Some(
                    AttemptStatus::NotFound | AttemptStatus::Unusable | AttemptStatus::Instrumental
                )
            )
        });

        // Asked order: by wave, then priority; not asked last.
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&i| (slots[i] == Slot::Waiting, waves[i], i));
        let attempts = order
            .into_iter()
            .map(|i| SourceAttempt {
                provider: self.providers[i].name().to_owned(),
                wave: waves[i],
                status: statuses[i].clone().unwrap_or(AttemptStatus::NotAsked),
                elapsed: elapsed[i],
            })
            .collect();

        found.sort_by_key(|a| Reverse(a.0));
        let mut found = found.into_iter().map(|(_, _, r)| r);
        let best = found.next();
        let alternates: Vec<LyricsResult> = found.collect();
        let result =
            match best {
                // An instrumental verdict beats guesses: line-synced or plain text found for
                // an instrumental is nearly always another song (or a fan-written poem).
                Some(r) if instrumental > 0 && r.sync_kind() != SyncKind::Word && !exhaustive => {
                    log::debug!("lyrics: instrumental; ignoring {}", r.provider);
                    return Race {
                        result: Ok(None),
                        clean: false,
                        alternates: Vec::new(),
                        attempts,
                    };
                }
                Some(r) => Ok(Some(r)),
                None if errors > 0 && errors == answered => Err(last_error
                    .unwrap_or_else(|| Error::Parse("lyrics: all providers failed".into()))),
                None => Ok(None),
            };
        let clean = matches!(result, Ok(None)) && n > 0 && all_answered_cleanly;
        Race {
            result,
            clean,
            alternates,
            attempts,
        }
    }
}

/// What [`LyricsService::race`] found. `clean`: every provider answered without
/// error and none had lyrics (so a miss may be cached).
struct Race {
    result: Result<Option<LyricsResult>>,
    clean: bool,
    alternates: Vec<LyricsResult>,
    attempts: Vec<SourceAttempt>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::{LyricsFormat, MatchInfo};
    use std::sync::atomic::AtomicUsize;

    struct Mock {
        name: &'static str,
        delay: Duration,
        reply: Option<&'static str>,
        fail: bool,
        calls: AtomicUsize,
        matched: MatchInfo,
        /// Saw the search's cancel flag after its delay.
        saw_cancel: AtomicBool,
        /// Shared count of mocks running right now, and the most seen at once.
        gauge: Option<Arc<(AtomicUsize, AtomicUsize)>>,
    }

    impl Mock {
        fn build(name: &'static str, delay_ms: u64, reply: Option<&'static str>) -> Self {
            Self {
                name,
                delay: Duration::from_millis(delay_ms),
                reply,
                fail: false,
                calls: AtomicUsize::new(0),
                matched: MatchInfo::default(),
                saw_cancel: AtomicBool::new(false),
                gauge: None,
            }
        }

        fn new(name: &'static str, delay_ms: u64, reply: Option<&'static str>) -> Arc<Self> {
            Arc::new(Self::build(name, delay_ms, reply))
        }

        /// Replies with lyrics matched to a recording of `secs` seconds.
        fn lasting(name: &'static str, delay_ms: u64, reply: &'static str, secs: f64) -> Arc<Self> {
            Arc::new(Self {
                matched: MatchInfo::duration(secs),
                ..Self::build(name, delay_ms, Some(reply))
            })
        }

        fn failing(name: &'static str) -> Arc<Self> {
            Arc::new(Self {
                fail: true,
                ..Self::build(name, 0, None)
            })
        }
    }

    impl LyricsProvider for Mock {
        fn name(&self) -> &str {
            self.name
        }

        fn fetch(&self, _: &LyricsQuery) -> Result<Option<LyricsResult>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(g) = &self.gauge {
                let now = g.0.fetch_add(1, Ordering::SeqCst) + 1;
                g.1.fetch_max(now, Ordering::SeqCst);
            }
            thread::sleep(self.delay);
            if let Some(g) = &self.gauge {
                g.0.fetch_sub(1, Ordering::SeqCst);
            }
            self.saw_cancel
                .store(crate::lyrics::providers::cancelled(), Ordering::SeqCst);
            if self.fail {
                return Err(Error::Parse("offline".into()));
            }
            Ok(self.reply.and_then(|r| {
                LyricsResult::parse(self.name, r, None).map(|r| r.with_match(self.matched.clone()))
            }))
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
    fn instrumental_verdict_and_credits() {
        const PURE: &str = "[00:00.00]纯音乐，请欣赏";
        // A line-synced guess for an instrumental is dropped …
        let s = service(
            vec![Mock::new("a", 0, Some(LINE)), Mock::new("b", 0, Some(PURE))],
            LyricsServiceConfig::default(),
        );
        assert!(s.fetch(&q()).unwrap().is_none());
        // … word-synced lyrics are trusted over it.
        let s = service(
            vec![Mock::new("a", 0, Some(WORD)), Mock::new("b", 0, Some(PURE))],
            LyricsServiceConfig::default(),
        );
        assert_eq!(s.fetch(&q()).unwrap().unwrap().provider, "a");
        // Credit lines naming the track's artist are removed with the query.
        const CREDITED: &str =
            "[00:01.00]Julian Casablancas/Thomas Bangalter\n[00:05.00]I didn't want to be the one";
        let s = service(
            vec![Mock::new("a", 0, Some(CREDITED))],
            LyricsServiceConfig::default(),
        );
        let r = s
            .fetch(&LyricsQuery::new(
                "Instant Crush",
                "Daft Punk, Julian Casablancas",
            ))
            .unwrap()
            .unwrap();
        assert_eq!(
            r.synced.unwrap().lines[0].text,
            "I didn't want to be the one"
        );
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

    fn waved(providers: Vec<(Arc<Mock>, u8)>, config: LyricsServiceConfig) -> LyricsService {
        LyricsService::with_waved_providers(
            providers
                .into_iter()
                .map(|(p, w)| (p as Arc<dyn LyricsProvider>, w))
                .collect(),
            config,
        )
    }

    fn picked(search: &LyricsSearch) -> &str {
        &search.result.as_ref().unwrap().as_ref().unwrap().provider
    }

    fn status_of<'a>(search: &'a LyricsSearch, provider: &str) -> &'a AttemptStatus {
        &search
            .attempts
            .iter()
            .find(|a| a.provider == provider)
            .unwrap_or_else(|| panic!("no attempt for {provider}"))
            .status
    }

    #[test]
    fn duration_match_ranks_first() {
        let q = || LyricsQuery::new("t", "a").with_duration(200);
        // A word-synced extended mix (230 s) from the top provider loses to the
        // line-synced lyrics of the same cut.
        let s = service(
            vec![
                Mock::lasting("extended", 0, WORD, 230.0),
                Mock::lasting("same-cut", 20, LINE, 201.0),
            ],
            LyricsServiceConfig::default(),
        );
        let r = s.fetch(&q()).unwrap().unwrap();
        assert_eq!(r.provider, "same-cut");
        assert_eq!(r.duration_fit(&q()), DurationFit::Exact);
        // A known match beats an unknown length too…
        let s = service(
            vec![
                Mock::new("unknown", 0, Some(WORD)),
                Mock::lasting("exact", 20, LINE, 199.0),
            ],
            LyricsServiceConfig::default(),
        );
        assert_eq!(s.fetch(&q()).unwrap().unwrap().provider, "exact");
        // …and an unknown length beats another cut.
        let s = service(
            vec![
                Mock::lasting("radio-edit", 0, WORD, 180.0),
                Mock::new("unknown", 20, Some(LINE)),
            ],
            LyricsServiceConfig::default(),
        );
        assert_eq!(s.fetch(&q()).unwrap().unwrap().provider, "unknown");
        // Another cut is still better than nothing.
        let s = service(
            vec![Mock::lasting("radio-edit", 0, LINE, 180.0)],
            LyricsServiceConfig::default(),
        );
        let r = s.fetch(&q()).unwrap().unwrap();
        assert_eq!(r.duration_fit(&q()), DurationFit::Different);
        // Off: the old order (word timing, then priority).
        let s = service(
            vec![
                Mock::lasting("extended", 0, WORD, 230.0),
                Mock::lasting("same-cut", 20, LINE, 201.0),
            ],
            LyricsServiceConfig {
                match_duration: false,
                ..LyricsServiceConfig::default()
            },
        );
        assert_eq!(s.fetch(&q()).unwrap().unwrap().provider, "extended");
    }

    #[test]
    fn names_rank_after_duration() {
        let q = LyricsQuery::new("Shape of You", "Ed Sheeran").with_duration(234);
        let other_song = Arc::new(Mock {
            matched: MatchInfo::duration(234.0).with_names(Some("Galway Girl"), Some("Ed Sheeran")),
            ..Mock::build("other-song", 0, Some(WORD))
        });
        let right_song = Arc::new(Mock {
            matched: MatchInfo::duration(233.7)
                .with_names(Some("Shape of You (Remastered)"), Some("Ed Sheeran")),
            ..Mock::build("right-song", 20, Some(LINE))
        });
        let s = service(vec![other_song, right_song], LyricsServiceConfig::default());
        assert_eq!(s.fetch(&q).unwrap().unwrap().provider, "right-song");
        let r = LyricsResult::parse("x", LINE, None)
            .unwrap()
            .with_match(MatchInfo::default().with_names(Some("Shape of You"), None));
        assert!(r.name_match(&q) > 0.99);
    }

    #[test]
    fn later_waves_only_without_a_good_result() {
        // Wave 1 has good lyrics: wave 2 is never asked.
        let second = Mock::new("second", 0, Some(WORD));
        let s = waved(
            vec![(Mock::new("first", 0, Some(LINE)), 1), (second.clone(), 2)],
            LyricsServiceConfig::default(),
        );
        let search = s.search(&q(), &SearchOptions::default());
        assert_eq!(picked(&search), "first");
        thread::sleep(Duration::from_millis(30));
        assert_eq!(second.calls.load(Ordering::SeqCst), 0);
        assert_eq!(status_of(&search, "second"), &AttemptStatus::NotAsked);
        assert_eq!(search.attempts[0].provider, "first");

        // Wave 1 has only plain text (or nothing): wave 2 runs and wins.
        let s = waved(
            vec![
                (Mock::new("plain", 0, Some(PLAIN)), 1),
                (Mock::new("none", 0, None), 1),
                (Mock::new("synced", 0, Some(LINE)), 2),
            ],
            LyricsServiceConfig::default(),
        );
        let search = s.search(&q(), &SearchOptions::default());
        assert_eq!(picked(&search), "synced");
        assert_eq!(search.alternates.len(), 1, "the plain text stays available");
        assert_eq!(status_of(&search, "none"), &AttemptStatus::NotFound);

        // A wave that runs too long: the next one starts alongside it, and the slow
        // provider is stopped once a good result is in.
        let slow = Mock::new("slow", 3000, Some(WORD));
        let s = waved(
            vec![(slow.clone(), 1), (Mock::new("next", 0, Some(LINE)), 2)],
            LyricsServiceConfig {
                wave_timeout: Duration::from_millis(100),
                quality_grace: Duration::from_millis(100),
                ..LyricsServiceConfig::default()
            },
        );
        let t = Instant::now();
        let search = s.search(&q(), &SearchOptions::default());
        assert_eq!(picked(&search), "next");
        assert!(
            t.elapsed() < Duration::from_millis(1500),
            "{:?}",
            t.elapsed()
        );
        assert_eq!(status_of(&search, "slow"), &AttemptStatus::Stopped);
    }

    #[test]
    fn waves_off_asks_everyone_at_once() {
        let second = Mock::new("second", 0, Some(WORD));
        let s = waved(
            vec![(Mock::new("first", 30, Some(LINE)), 1), (second.clone(), 3)],
            LyricsServiceConfig {
                waves: false,
                ..LyricsServiceConfig::default()
            },
        );
        assert_eq!(s.waves(), &[1, 1]);
        assert_eq!(s.fetch(&q()).unwrap().unwrap().provider, "second");
        assert_eq!(second.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn stragglers_are_cancelled() {
        let slow = Mock::new("slow", 200, Some(LINE));
        let s = service(
            vec![Mock::new("fast", 0, Some(WORD)), slow.clone()],
            LyricsServiceConfig::default(),
        );
        let search = s.search(&q(), &SearchOptions::default());
        assert_eq!(picked(&search), "fast");
        assert_eq!(status_of(&search, "slow"), &AttemptStatus::Stopped);
        // The worker sees the flag: its next request would fail at once.
        thread::sleep(Duration::from_millis(400));
        assert!(slow.saw_cancel.load(Ordering::SeqCst));
        // Cancelled requests never reach the network.
        let flag = Arc::new(AtomicBool::new(true));
        set_cancel_flag(Some(flag));
        let err = super::super::providers::http_get("https://example.invalid/", &[], &[], None);
        set_cancel_flag(None);
        assert!(err.is_err_and(|e| e.to_string().contains("cancelled")));
    }

    #[test]
    fn requests_stay_capped() {
        let gauge = Arc::new((AtomicUsize::new(0), AtomicUsize::new(0)));
        let mocks: Vec<(Arc<Mock>, u8)> = ["a", "b", "c", "d", "e"]
            .into_iter()
            .map(|name| {
                let m = Arc::new(Mock {
                    gauge: Some(gauge.clone()),
                    ..Mock::build(name, 40, None)
                });
                (m, 1)
            })
            .collect();
        let s = waved(
            mocks,
            LyricsServiceConfig {
                max_concurrent: 2,
                ..LyricsServiceConfig::default()
            },
        );
        assert!(s.fetch(&q()).unwrap().is_none());
        assert_eq!(gauge.1.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn pinned_source_wins_and_bypasses_other_cached_lyrics() {
        let pin = |p: &str| SearchOptions {
            pinned: Some(p.into()),
            ..SearchOptions::default()
        };
        let cache = Arc::new(MemoryLyricsCache::new());
        let best = Mock::new("best", 0, Some(WORD));
        let chosen = Mock::new("chosen", 50, Some(LINE));
        // `chosen` sits in a later wave; pinning it brings it into the first.
        let s = waved(
            vec![(best.clone(), 1), (chosen.clone(), 2)],
            LyricsServiceConfig::default(),
        )
        .with_cache(cache.clone());
        assert_eq!(s.fetch(&q()).unwrap().unwrap().provider, "best");
        // The cache holds `best`: a pin on `chosen` asks the providers again…
        let search = s.search(&q(), &pin("chosen"));
        assert!(!search.from_cache);
        assert_eq!(picked(&search), "chosen");
        assert_eq!(search.alternates[0].provider, "best");
        // …and stores the choice, which later searches answer from the cache.
        let again = s.search(&q(), &pin("chosen"));
        assert!(again.from_cache);
        assert_eq!(again.result.unwrap().unwrap().provider, "chosen");
        assert_eq!(chosen.calls.load(Ordering::SeqCst), 1);
        // A pinned source without lyrics: the usual pick.
        let s = service(
            vec![
                Mock::new("best", 0, Some(WORD)),
                Mock::new("empty", 0, None),
            ],
            LyricsServiceConfig::default(),
        );
        let search = s.search(&q(), &pin("empty"));
        assert_eq!(picked(&search), "best");
        // One source on request.
        let (r, attempt) = s.fetch_from(&q(), "best");
        assert_eq!(r.unwrap().unwrap().provider, "best");
        assert!(matches!(
            attempt.status,
            AttemptStatus::Found {
                sync: SyncKind::Word,
                ..
            }
        ));
        assert_eq!(s.fetch_from(&q(), "nope").1.status, AttemptStatus::NotAsked);
    }

    #[test]
    fn default_waves_put_fast_providers_first() {
        let s = LyricsService::new(LyricsServiceConfig::default());
        let wave_of = |name: &str| {
            let i = s.providers().iter().position(|p| p.name() == name).unwrap();
            s.waves()[i]
        };
        assert_eq!(wave_of("BetterLyrics"), 1);
        assert_eq!(wave_of("LrcLib"), 1);
        assert_eq!(wave_of("SimpMusic"), 1);
        assert_eq!(wave_of("Kugou"), 2);
        assert_eq!(wave_of("YouLyPlus"), 3);
        // The provider at the top of the user's order is always in the first wave.
        let s = LyricsService::new(LyricsServiceConfig {
            preferred: Some(ProviderId::Megalobiz),
            ..LyricsServiceConfig::default()
        });
        assert_eq!(s.providers()[0].name(), "Megalobiz");
        assert_eq!(s.waves()[0], 1);
    }
}
