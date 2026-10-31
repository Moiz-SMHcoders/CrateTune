//! Lyrics for the Now Playing screen: fetching through music-core's
//! `LyricsService` (provider order / switches / keys from Settings → Lyrics,
//! results cached in memory), preparing them for the renderer (word offsets,
//! background vocals, duet sides, romanization) and on-demand translation,
//! cached per track and translator settings (for the session, and in the library
//! database across sessions); a rate-limited translation retries by itself once the
//! service's back-off has passed.
//!
//! Persistent caching: [`MusicApp::set_lyrics_cache`] swaps the in-memory
//! `LyricsCache` for another one (e.g. the library database's table); the
//! service is rebuilt around it.
//!
//! Per track, [`TrackLyricsStore`] remembers the source picked by hand (asked first
//! and preferred by later searches) and a timing nudge, in `lyrics-tracks.json`
//! next to the settings; [`SourcesInfo`] keeps what the last searches did (the
//! "sources tried" list) and the other sources' lyrics, to switch without asking
//! again. Per-track nudges follow Zuno's `lyricsOffset` (© noFAYZ, Apache-2.0) and
//! Limusic's pinned choices (© SimoHypers, GPL-3.0).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{Context, SharedString};
use music_core::Track;
use music_core::lyrics::romanize::dictionary::{self, DictionarySource};
use music_core::lyrics::romanize::{Script, detect_script, romanize_line};
use music_core::lyrics::translate::{CachedTranslator, LyricsTranslator, is_rate_limited};
use music_core::lyrics::{
    AttemptStatus, CachedLyrics, LyricLine, Lyrics, LyricsCache, LyricsQuery, LyricsResult,
    LyricsService, MemoryLyricsCache, RawLyrics, SearchOptions, SourceAttempt, SyncKind,
};
use serde::{Deserialize, Serialize};

use crate::app::MusicApp;
use crate::karaoke;
use crate::util::off_thread;

pub enum LyricsState {
    Loading,
    /// No provider had lyrics (with the provider settings of `generation`).
    Missing {
        generation: u64,
    },
    Failed(String),
    Ready(Arc<PreparedLyrics>),
}

/// One synced line, ready for `KaraokeLine`.
pub struct PreparedLine {
    pub text: SharedString,
    pub word_starts: Arc<[usize]>,
    /// Background vocals sung alongside (TTML `x-bg` spans, QRC).
    pub background: Option<(LyricLine, SharedString, Arc<[usize]>)>,
    /// Sung by another singer than the first one (duets): drawn on the other side.
    pub secondary_singer: bool,
    /// Translation shipped with the lyrics (TTML translation track).
    pub provided_translation: Option<SharedString>,
}

pub struct PreparedLyrics {
    pub provider: SharedString,
    pub kind: SyncKind,
    /// `Some` for line- or word-synced lyrics.
    pub synced: Option<Lyrics>,
    pub lines: Vec<PreparedLine>,
    /// Unsynced text, one entry per line (when `synced` is `None`).
    pub plain: Vec<SharedString>,
    /// Per line (synced or plain): the line's script and its romanization.
    pub romanized: Vec<Option<(Script, SharedString)>>,
    /// Romanization shipped by the provider, per line (kept for re-romanizing).
    provided_romanization: Vec<Option<String>>,
}

impl PreparedLyrics {
    pub fn new(result: LyricsResult) -> Self {
        let kind = result.sync_kind();
        let provider: SharedString = result.provider.clone().into();
        let first_agent = result.extras.iter().find_map(|e| e.agent.clone());
        let group_agents: Vec<&str> = result
            .agents
            .iter()
            .filter(|a| a.kind.as_deref() == Some("group"))
            .map(|a| a.id.as_str())
            .collect();
        let (lines, texts): (Vec<PreparedLine>, Vec<String>) = match &result.synced {
            Some(lyrics) => lyrics
                .lines
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    let extras = result.extras_for(i);
                    let background = extras.and_then(|e| e.background.clone()).map(|bg| {
                        let starts = karaoke::word_starts(&bg.text, &bg.words);
                        let text: SharedString = bg.text.clone().into();
                        (bg, text, starts)
                    });
                    let agent = extras.and_then(|e| e.agent.as_deref());
                    let secondary_singer = agent.is_some_and(|a| {
                        Some(a) != first_agent.as_deref() && !group_agents.contains(&a)
                    });
                    (
                        PreparedLine {
                            text: l.text.clone().into(),
                            word_starts: karaoke::word_starts(&l.text, &l.words),
                            background,
                            secondary_singer,
                            provided_translation: extras
                                .and_then(|e| e.translation.clone())
                                .map(Into::into),
                        },
                        l.text.clone(),
                    )
                })
                .unzip(),
            None => (
                Vec::new(),
                result
                    .plain
                    .as_deref()
                    .unwrap_or_default()
                    .lines()
                    .map(str::to_owned)
                    .collect(),
            ),
        };
        let provided: Vec<Option<String>> = (0..texts.len())
            .map(|i| result.extras_for(i).and_then(|e| e.romanization.clone()))
            .collect();
        let romanized = romanize_all(&texts, &provided);
        let plain = if result.synced.is_none() {
            texts.into_iter().map(Into::into).collect()
        } else {
            Vec::new()
        };
        Self {
            provider,
            kind,
            synced: result.synced,
            lines,
            plain,
            romanized,
            provided_romanization: provided,
        }
    }

    /// The same lyrics with romanization computed again (a dictionary was loaded).
    fn reromanized(&self) -> Self {
        let texts = self.texts();
        let provided: Vec<Option<String>> = self.provided_romanization.clone();
        Self {
            provider: self.provider.clone(),
            kind: self.kind,
            synced: self.synced.clone(),
            lines: self
                .lines
                .iter()
                .map(|l| PreparedLine {
                    text: l.text.clone(),
                    word_starts: l.word_starts.clone(),
                    background: l.background.clone(),
                    secondary_singer: l.secondary_singer,
                    provided_translation: l.provided_translation.clone(),
                })
                .collect(),
            plain: self.plain.clone(),
            romanized: romanize_all(&texts, &provided),
            provided_romanization: provided,
        }
    }

    /// The text lines translation works on (synced lines, else plain lines).
    pub fn texts(&self) -> Vec<String> {
        match &self.synced {
            Some(l) => l.lines.iter().map(|l| l.text.clone()).collect(),
            None => self.plain.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// Romanization of every line, tagged with its script. Kanji-only lines in a song
/// with kana are Japanese (like music-core's `romanize_lines`). Romanization the
/// provider shipped wins.
fn romanize_all(
    lines: &[String],
    provided: &[Option<String>],
) -> Vec<Option<(Script, SharedString)>> {
    let japanese_song = lines
        .iter()
        .any(|l| detect_script(l) == Some(Script::Japanese));
    lines
        .iter()
        .zip(provided)
        .map(|(line, provided)| {
            let script = match detect_script(line)? {
                Script::Chinese if japanese_song => Script::Japanese,
                s => s,
            };
            let text = provided
                .clone()
                .filter(|p| !p.trim().is_empty() && !p.trim().eq_ignore_ascii_case(line.trim()))
                .or_else(|| romanize_line(line, script))?;
            Some((script, text.into()))
        })
        .collect()
}

pub enum TranslationState {
    Loading,
    Ready(Arc<Vec<Option<SharedString>>>),
    /// `retry_at`: the service rate-limited us; ask again by itself after this.
    Failed {
        message: String,
        retry_at: Option<Instant>,
    },
}

/// The downloadable kanji dictionary (Japanese romanization), see
/// `music_core::lyrics::romanize::dictionary`.
#[derive(Clone)]
pub enum KanjiDictionary {
    /// Not looked at yet (startup).
    Unknown,
    /// This build embeds IPADIC (`japanese-dict`): nothing to download.
    Embedded,
    Missing,
    Downloading {
        done: u64,
        total: u64,
        cancel: Arc<AtomicBool>,
    },
    /// Installed and loaded; bytes on disk.
    Ready(u64),
    Failed(String),
}

/// Where the dictionary is unpacked: `$MUSIC_CONFIG_DIR/cache/ja-dict`, else the
/// platform cache dir (macOS `~/Library/Caches/music-macos/ja-dict`).
pub fn kanji_dictionary_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("MUSIC_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir).join("cache").join("ja-dict"));
    }
    crate::identity::project_dirs().map(|d| d.cache_dir().join("ja-dict"))
}

/// What the last search for a track did, and every usable result it found (the
/// shown one first), kept as raw payloads so switching costs a parse, not a request.
#[derive(Debug, Default)]
pub struct SourcesInfo {
    pub attempts: Vec<SourceAttempt>,
    pub found: Vec<RawLyrics>,
    /// Answered from the lyrics cache (no provider asked).
    pub from_cache: bool,
    pub elapsed: Duration,
    /// A provider being asked on request ("try this source").
    pub trying: Option<String>,
    /// Changes with every update (the toolbar rebuilds its menu only then).
    pub revision: u64,
}

/// How many tracks' [`SourcesInfo`] are kept (the playing one and a few before).
const SOURCES_KEPT: usize = 3;

/// Per-track lyrics choices: the source picked by hand and a timing nudge.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TrackLyrics {
    /// [`music_core::lyrics::LyricsProvider::name`] of the source picked for the track.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Timing nudge in milliseconds; positive shows the lyrics later.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub offset_ms: i32,
    /// Last change (a counter), to forget the oldest tracks first.
    #[serde(default)]
    pub touched: u64,
}

fn is_zero(v: &i32) -> bool {
    *v == 0
}

impl TrackLyrics {
    fn is_default(&self) -> bool {
        self.source.is_none() && self.offset_ms == 0
    }
}

/// One nudge step and the largest nudge (a bigger error is the wrong lyrics).
pub const OFFSET_STEP_MS: i32 = 100;
pub const MAX_OFFSET_MS: i32 = 10_000;
/// Tracks remembered; the least recently changed are forgotten beyond this.
const MAX_TRACKS: usize = 2000;

/// [`TrackLyrics`] per video id, saved as JSON (`lyrics-tracks.json`).
#[derive(Debug, Default)]
pub struct TrackLyricsStore {
    path: Option<PathBuf>,
    entries: HashMap<String, TrackLyrics>,
    counter: u64,
    /// The newest JSON not yet written (writes happen off the UI thread).
    pending: Arc<Mutex<Option<String>>>,
}

impl TrackLyricsStore {
    pub const FILE_NAME: &str = "lyrics-tracks.json";

    /// Loads `path` (an unreadable or missing file is an empty store).
    pub fn load(path: Option<PathBuf>) -> Self {
        let entries: HashMap<String, TrackLyrics> = path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        let counter = entries.values().map(|t| t.touched).max().unwrap_or(0);
        Self {
            path,
            entries,
            counter,
            pending: Arc::default(),
        }
    }

    pub fn get(&self, video_id: &str) -> Option<&TrackLyrics> {
        self.entries.get(video_id)
    }

    pub fn source(&self, video_id: &str) -> Option<&str> {
        self.get(video_id)?.source.as_deref()
    }

    pub fn offset_ms(&self, video_id: &str) -> i32 {
        self.get(video_id).map_or(0, |t| t.offset_ms)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }

    /// Remembers (or forgets, `None`) the source picked for a track. True if changed.
    pub fn set_source(&mut self, video_id: &str, source: Option<String>) -> bool {
        self.change(video_id, |t| {
            let changed = t.source != source;
            t.source = source;
            changed
        })
    }

    /// Moves the track's nudge by `delta_ms` (snapped to [`OFFSET_STEP_MS`], at most
    /// ±[`MAX_OFFSET_MS`]); returns the new nudge.
    pub fn nudge(&mut self, video_id: &str, delta_ms: i32) -> i32 {
        let mut now = 0;
        self.change(video_id, |t| {
            let raw = (t.offset_ms + delta_ms).clamp(-MAX_OFFSET_MS, MAX_OFFSET_MS);
            let snapped = ((raw as f32 / OFFSET_STEP_MS as f32).round() as i32) * OFFSET_STEP_MS;
            now = snapped;
            std::mem::replace(&mut t.offset_ms, snapped) != snapped
        });
        now
    }

    pub fn reset_offset(&mut self, video_id: &str) -> bool {
        self.change(video_id, |t| std::mem::take(&mut t.offset_ms) != 0)
    }

    fn change(&mut self, video_id: &str, f: impl FnOnce(&mut TrackLyrics) -> bool) -> bool {
        let entry = self.entries.entry(video_id.to_owned()).or_default();
        if !f(entry) {
            if entry.is_default() {
                self.entries.remove(video_id);
            }
            return false;
        }
        self.counter += 1;
        entry.touched = self.counter;
        if entry.is_default() {
            self.entries.remove(video_id);
        }
        if self.entries.len() > MAX_TRACKS {
            let mut touched: Vec<u64> = self.entries.values().map(|t| t.touched).collect();
            touched.sort_unstable();
            let cut = touched[self.entries.len() - MAX_TRACKS];
            self.entries.retain(|_, t| t.touched >= cut);
        }
        self.save();
        true
    }

    /// Writes the store off the UI thread; the newest state always lands last.
    fn save(&self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let Ok(json) = serde_json::to_string(&self.entries) else {
            return;
        };
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(json);
        let pending = self.pending.clone();
        std::thread::spawn(move || {
            let _guard = TRACKS_WRITE.lock().unwrap_or_else(|e| e.into_inner());
            let next = pending.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(json) = next
                && let Err(e) = write_atomically(&path, &json)
            {
                eprintln!("music-gpui: {}: {e}", path.display());
            }
        });
    }

    /// Waits for pending writes (tests).
    #[cfg(test)]
    fn flush(&self) {
        while self.pending.lock().unwrap().is_some() {
            std::thread::sleep(Duration::from_millis(2));
        }
        // The writer takes the JSON while holding the lock: wait for it to finish.
        drop(TRACKS_WRITE.lock());
    }
}

/// Serializes writes of `lyrics-tracks.json`.
static TRACKS_WRITE: Mutex<()> = Mutex::new(());

fn write_atomically(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

/// Lyrics service + cache and the per-track state the app keeps.
pub struct LyricsHub {
    pub service: Arc<LyricsService>,
    pub cache: Arc<dyn LyricsCache>,
    /// Bumped whenever the provider settings change (refetch "missing" tracks).
    pub generation: u64,
    /// Per video id: the last [`STATES_KEPT`] tracks (a track played again comes
    /// back from the lyrics cache, without a request).
    pub states: HashMap<String, LyricsState>,
    /// `states`' ids, least recently set first.
    recent: std::collections::VecDeque<String>,
    /// Per (video id, translator key).
    pub translations: HashMap<(String, String), TranslationState>,
    pub kanji: KanjiDictionary,
    /// Sources picked by hand and timing nudges, per track.
    pub tracks: TrackLyricsStore,
    /// The last few searches (most recent last), see [`SourcesInfo`].
    pub sources: Vec<(String, SourcesInfo)>,
    sources_revision: u64,
}

impl Default for LyricsHub {
    fn default() -> Self {
        let cache: Arc<dyn LyricsCache> = Arc::new(MemoryLyricsCache::new());
        Self {
            service: Arc::new(LyricsService::default().with_cache(cache.clone())),
            cache,
            generation: 0,
            states: HashMap::new(),
            recent: std::collections::VecDeque::new(),
            translations: HashMap::new(),
            kanji: KanjiDictionary::Unknown,
            // Unit tests never touch the real settings folder.
            tracks: TrackLyricsStore::load(
                crate::settings::file::config_dir()
                    .filter(|_| !cfg!(test))
                    .map(|d| d.join(TrackLyricsStore::FILE_NAME)),
            ),
            sources: Vec::new(),
            sources_revision: 0,
        }
    }
}

/// Prepared lyrics kept in memory (each holds every line, word timing and
/// romanization of a song; a long session would otherwise keep them all).
const STATES_KEPT: usize = 12;

impl LyricsHub {
    /// Sets a track's state and forgets the oldest ones beyond [`STATES_KEPT`]
    /// (never `keep`, the playing track, or one still loading).
    pub fn put_state(&mut self, video_id: String, state: LyricsState, keep: Option<&str>) {
        self.recent.retain(|id| *id != video_id);
        self.recent.push_back(video_id.clone());
        self.states.insert(video_id, state);
        let mut i = 0;
        while self.states.len() > STATES_KEPT && i < self.recent.len() {
            let id = &self.recent[i];
            if Some(id.as_str()) == keep
                || matches!(self.states.get(id), Some(LyricsState::Loading))
            {
                i += 1;
                continue;
            }
            if let Some(id) = self.recent.remove(i) {
                self.states.remove(&id);
                self.translations.retain(|(track, _), _| *track != id);
            }
        }
    }

    pub fn sources_for(&self, video_id: &str) -> Option<&SourcesInfo> {
        self.sources
            .iter()
            .rev()
            .find(|(id, _)| id == video_id)
            .map(|(_, s)| s)
    }

    /// The track's sources, to change (bumps their revision).
    fn sources_mut(&mut self, video_id: &str) -> Option<&mut SourcesInfo> {
        self.sources_revision += 1;
        let revision = self.sources_revision;
        let info = self
            .sources
            .iter_mut()
            .rev()
            .find(|(id, _)| id == video_id)
            .map(|(_, s)| s)?;
        info.revision = revision;
        Some(info)
    }

    fn set_sources(&mut self, video_id: String, mut info: SourcesInfo) {
        self.sources_revision += 1;
        info.revision = self.sources_revision;
        self.sources.retain(|(id, _)| *id != video_id);
        self.sources.push((video_id, info));
        if self.sources.len() > SOURCES_KEPT {
            self.sources.remove(0);
        }
    }

    /// Where synced lyrics are at playback `position` for `video_id` (its timing
    /// nudge applied: a positive nudge shows them later).
    pub fn lyrics_position(&self, video_id: &str, position: Duration) -> Duration {
        shifted(position, self.tracks.offset_ms(video_id))
    }

    /// The playback position for lyrics time `at` (inverse of `lyrics_position`).
    pub fn playback_position(&self, video_id: &str, at: Duration) -> Duration {
        shifted(at, -self.tracks.offset_ms(video_id))
    }
}

/// `position - offset_ms`, clamped at zero.
fn shifted(position: Duration, offset_ms: i32) -> Duration {
    let offset = Duration::from_millis(u64::from(offset_ms.unsigned_abs()));
    if offset_ms >= 0 {
        position.saturating_sub(offset)
    } else {
        position + offset
    }
}

impl MusicApp {
    /// Rebuilds the service from Settings → Lyrics (providers, keys).
    pub(crate) fn rebuild_lyrics_service(&mut self, cx: &mut Context<Self>) {
        if matches!(self.lyrics.kanji, KanjiDictionary::Unknown) {
            self.load_kanji_dictionary(cx);
        }
        let config = self.settings.read(cx).lyrics_service_config();
        if *self.lyrics.service.config() == config {
            return;
        }
        let hub = &mut self.lyrics;
        hub.service = Arc::new(LyricsService::new(config).with_cache(hub.cache.clone()));
        hub.generation += 1;
        cx.notify();
    }

    /// Hook for a persistent lyrics cache (e.g. the library database): later
    /// fetches read and write it.
    pub fn set_lyrics_cache(&mut self, cache: Arc<dyn LyricsCache>, cx: &mut Context<Self>) {
        let config = self.lyrics.service.config().clone();
        self.lyrics.cache = cache.clone();
        self.lyrics.service = Arc::new(LyricsService::new(config).with_cache(cache));
        cx.notify();
    }

    /// Fetches lyrics for `track` once (all providers, off the UI thread).
    /// Failures retry the next time; "missing" retries after a provider change.
    pub fn ensure_lyrics(&mut self, track: &Track, cx: &mut Context<Self>) {
        let generation = self.lyrics.generation;
        let refresh = match self.lyrics.states.get(&track.video_id) {
            Some(LyricsState::Loading | LyricsState::Ready(_)) => return,
            Some(LyricsState::Missing { generation: g }) if *g == generation => return,
            // The cache remembers "not found"; skip it after a settings change.
            Some(LyricsState::Missing { .. }) => true,
            Some(LyricsState::Failed(_)) | None => false,
        };
        self.fetch_lyrics(track, refresh, cx);
    }

    /// Fetches again, skipping the cache ("reload" in Now Playing).
    pub fn reload_lyrics(&mut self, cx: &mut Context<Self>) {
        if let Some(track) = self.playback.track.clone() {
            self.lyrics
                .translations
                .retain(|(id, _), _| *id != track.video_id);
            self.fetch_lyrics(&track, true, cx);
        }
    }

    fn fetch_lyrics(&mut self, track: &Track, refresh: bool, cx: &mut Context<Self>) {
        let playing = self.playback.track.as_ref().map(|t| t.video_id.clone());
        self.lyrics.put_state(
            track.video_id.clone(),
            LyricsState::Loading,
            playing.as_deref(),
        );
        let service = self.lyrics.service.clone();
        let generation = self.lyrics.generation;
        let mut query = LyricsQuery::from_track(track);
        let id = track.video_id.clone();
        let options = SearchOptions {
            refresh,
            pinned: self.lyrics.tracks.source(&id).map(str::to_owned),
        };
        // Tracks from some search cards and queues carry no length; the match on the
        // exact cut needs it (as Limusic does, from the watch page's own item).
        let lookup = (query.duration_secs.is_none() && query.video_id.is_some())
            .then(|| self.innertube.clone());
        let trace = self.trace;
        cx.spawn(async move |this, cx| {
            let (result, info) = off_thread(move || {
                if let Some(client) = lookup
                    && let Some(video_id) = query.video_id.clone()
                {
                    let target = music_core::innertube::WatchTarget::video(video_id.as_str());
                    match client.next(&target) {
                        Ok(queue) => {
                            if let Some(t) = queue.tracks.iter().find(|t| t.video_id == video_id) {
                                query.duration_secs = t.duration_secs;
                                if query.album.is_none() {
                                    query.album = t.album.clone();
                                }
                            }
                        }
                        Err(e) => eprintln!("music-gpui: lyrics: no length for {video_id}: {e}"),
                    }
                }
                let search = service.search(&query, &options);
                let mut found: Vec<RawLyrics> = Vec::new();
                if let Ok(Some(r)) = &search.result {
                    found.push(r.to_raw());
                }
                found.extend(search.alternates.iter().map(LyricsResult::to_raw));
                let info = SourcesInfo {
                    attempts: search.attempts,
                    found,
                    from_cache: search.from_cache,
                    elapsed: search.elapsed,
                    trying: None,
                    revision: 0,
                };
                let result = search
                    .result
                    .map(|r| r.map(|r| Arc::new(PreparedLyrics::new(r))));
                (result, info)
            })
            .await;
            this.update(cx, |this, cx| {
                if let Some(started) = trace {
                    let what = match &result {
                        Ok(Some(p)) => format!(
                            "{} ({:?}, {} lines)",
                            p.provider,
                            p.kind,
                            p.lines.len().max(p.plain.len())
                        ),
                        Ok(None) => "none".into(), // i18n-ignore
                        Err(e) => format!("failed: {e}"),
                    };
                    let asked: Vec<String> = info
                        .attempts
                        .iter()
                        .filter(|a| a.status != AttemptStatus::NotAsked)
                        .map(|a| {
                            format!("{}:{}", a.provider, a.elapsed.map_or(0, |d| d.as_millis()))
                        })
                        .collect();
                    eprintln!(
                        "music-gpui: [{:6.2}s] lyrics {id}: {what} in {} ms{} [{}]",
                        started.elapsed().as_secs_f64(),
                        info.elapsed.as_millis(),
                        if info.from_cache { " (cache)" } else { "" },
                        asked.join(", "),
                    );
                }
                let state = match result {
                    Ok(Some(prepared)) => LyricsState::Ready(prepared),
                    Ok(None) => LyricsState::Missing { generation },
                    Err(e) => LyricsState::Failed(e.to_string()),
                };
                let playing = this.playback.track.as_ref().map(|t| t.video_id.clone());
                this.lyrics.put_state(id.clone(), state, playing.as_deref());
                this.lyrics.set_sources(id, info);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Shows `provider`'s lyrics for the playing track and remembers the choice
    /// (`None`: forget it and search again). Lyrics a search already found are
    /// switched to at once; another source is asked first, and only remembered
    /// when it has lyrics.
    pub fn choose_lyrics_source(&mut self, provider: Option<String>, cx: &mut Context<Self>) {
        let Some(track) = self.playback.track.clone() else {
            return;
        };
        let id = track.video_id.clone();
        let Some(provider) = provider else {
            self.lyrics.tracks.set_source(&id, None);
            self.reload_lyrics(cx);
            return;
        };
        let query = LyricsQuery::from_track(&track);
        let found = self
            .lyrics
            .sources_for(&id)
            .and_then(|s| s.found.iter().find(|r| r.provider == provider).cloned());
        let service = self.lyrics.service.clone();
        let cache = self.lyrics.cache.clone();
        if found.is_none() {
            if let Some(info) = self.lyrics.sources_mut(&id) {
                info.trying = Some(provider.clone());
            }
            cx.notify();
        }
        cx.spawn(async move |this, cx| {
            let outcome = off_thread(move || {
                let (result, attempt) = match found {
                    Some(raw) => (
                        raw.parse(query.duration_secs)
                            .and_then(|r| r.without_credits(&query)),
                        None,
                    ),
                    None => {
                        let (result, attempt) = service.fetch_from(&query, &provider);
                        (result.ok().flatten(), Some(attempt))
                    }
                };
                let raw = result.as_ref().map(LyricsResult::to_raw);
                if let Some(raw) = &raw {
                    // The choice is what this track shows from now on.
                    cache.store(&query, &CachedLyrics::Found(raw.clone()));
                }
                let prepared = result.map(|r| Arc::new(PreparedLyrics::new(r)));
                (provider, prepared, raw, attempt)
            })
            .await;
            this.update(cx, |this, cx| {
                let (provider, prepared, raw, attempt) = outcome;
                if let Some(info) = this.lyrics.sources_mut(&id) {
                    info.trying = None;
                    if let Some(attempt) = attempt
                        && let Some(slot) = info
                            .attempts
                            .iter_mut()
                            .find(|a| a.provider == attempt.provider)
                    {
                        *slot = attempt;
                    }
                    if let Some(raw) = raw
                        && !info.found.iter().any(|r| r.provider == raw.provider)
                    {
                        info.found.push(raw);
                    }
                }
                if let Some(prepared) = prepared {
                    this.lyrics.tracks.set_source(&id, Some(provider));
                    this.lyrics
                        .translations
                        .retain(|(track, _), _| *track != id);
                    this.lyrics
                        .put_state(id.clone(), LyricsState::Ready(prepared), Some(&id));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Prints the playing track's "sources tried" list (`@lyrics-sources`).
    pub(crate) fn print_lyrics_sources(&self) {
        let Some(track) = &self.playback.track else {
            return;
        };
        let id = &track.video_id;
        let Some(info) = self.lyrics.sources_for(id) else {
            eprintln!("music-gpui: lyrics sources {id}: no search yet");
            return;
        };
        eprintln!(
            "music-gpui: lyrics sources {id}: {} ms{}, pinned {:?}, nudge {} ms",
            info.elapsed.as_millis(),
            if info.from_cache { " (cache)" } else { "" },
            self.lyrics.tracks.source(id),
            self.lyrics.tracks.offset_ms(id),
        );
        for a in &info.attempts {
            eprintln!(
                "music-gpui:   wave {} {:<22} {:>5} ms {:?}",
                a.wave,
                a.provider,
                a.elapsed.map_or(0, |d| d.as_millis()),
                a.status
            );
        }
    }

    /// Nudges the playing track's lyrics timing by `delta_ms` (positive: later).
    pub fn nudge_lyrics(&mut self, delta_ms: i32, cx: &mut Context<Self>) {
        if let Some(track) = &self.playback.track {
            self.lyrics.tracks.nudge(&track.video_id, delta_ms);
            cx.notify();
        }
    }

    /// The playing track's lyrics as published again.
    pub fn reset_lyrics_nudge(&mut self, cx: &mut Context<Self>) {
        if let Some(track) = &self.playback.track
            && self.lyrics.tracks.reset_offset(&track.video_id)
        {
            cx.notify();
        }
    }

    /// Translation of the current track's lyrics with the configured translator,
    /// requested once per track and translator settings (`None` while loading).
    pub(crate) fn translation_for(
        &mut self,
        video_id: &str,
        prepared: &Arc<PreparedLyrics>,
        cx: &mut Context<Self>,
    ) -> Option<Result<Arc<Vec<Option<SharedString>>>, String>> {
        let key = self.settings.read(cx).translator_key();
        let slot = (video_id.to_owned(), key.clone());
        match self.lyrics.translations.get(&slot) {
            Some(TranslationState::Ready(lines)) => return Some(Ok(lines.clone())),
            Some(TranslationState::Failed { message, retry_at }) => {
                // A rate limit ran out: ask again (the Retry button does it earlier).
                if !retry_at.is_some_and(|at| Instant::now() >= at) {
                    return Some(Err(message.clone()));
                }
            }
            Some(TranslationState::Loading) => return None,
            None => {}
        }
        let translator = match self.settings.read(cx).translator() {
            Ok(t) => t,
            Err(e) => return Some(Err(e)),
        };
        // Finished translations persist in the library (per lyrics, language, translator).
        let translator: Box<dyn LyricsTranslator> = match self.store.library.clone() {
            Some(library) => Box::new(CachedTranslator::new(translator, Arc::new(library), key)),
            None => translator,
        };
        let language = self
            .settings
            .read(cx)
            .settings()
            .lyrics
            .target_language
            .clone();
        self.lyrics
            .translations
            .insert(slot.clone(), TranslationState::Loading);
        let texts = prepared.texts();
        cx.spawn(async move |this, cx| {
            let result = off_thread(move || {
                let lines: Vec<&str> = texts.iter().map(String::as_str).collect();
                translator
                    .translate_lines(&lines, &language)
                    .map_err(|e| (e, translator.retry_after()))
            })
            .await;
            this.update(cx, |this, cx| {
                if let Some(started) = this.trace {
                    let what = match &result {
                        Ok(lines) => format!("{} lines", lines.iter().flatten().count()),
                        Err((e, _)) => format!("failed: {e}"),
                    };
                    eprintln!(
                        "music-gpui: [{:6.2}s] translation {}: {what}",
                        started.elapsed().as_secs_f64(),
                        slot.0
                    );
                }
                let state = match result {
                    Ok(lines) => TranslationState::Ready(Arc::new(
                        lines.into_iter().map(|l| l.map(Into::into)).collect(),
                    )),
                    Err((e, retry_after)) => TranslationState::Failed {
                        retry_at: is_rate_limited(&e).then(|| {
                            Instant::now() + retry_after.unwrap_or(Duration::from_secs(120))
                        }),
                        // Without the generic "unexpected response:" prefix.
                        message: match e {
                            music_core::Error::Parse(m) => m,
                            e => e.to_string(),
                        },
                    },
                };
                this.lyrics.translations.insert(slot, state);
                cx.notify();
            })
            .ok();
        })
        .detach();
        None
    }

    /// At startup: loads an installed kanji dictionary (memory-mapped, off the UI thread).
    fn load_kanji_dictionary(&mut self, cx: &mut Context<Self>) {
        if music_core::lyrics::romanize::HAS_KANJI_READINGS {
            self.lyrics.kanji = KanjiDictionary::Embedded;
            return;
        }
        self.lyrics.kanji = KanjiDictionary::Missing;
        let Some(dir) = kanji_dictionary_dir() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let state = off_thread(move || {
                let source = DictionarySource::ipadic();
                let path = dictionary::installed(&source, &dir)?;
                Some(match dictionary::load(&path) {
                    Ok(()) => KanjiDictionary::Ready(dictionary::installed_size(&source, &dir)),
                    Err(e) => KanjiDictionary::Failed(e.to_string()),
                })
            })
            .await;
            if let Some(state) = state {
                this.update(cx, |this, cx| this.kanji_dictionary_changed(state, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// Downloads, verifies and loads the kanji dictionary (Settings → Lyrics, or
    /// turning Japanese romanization on). No-op when it is there or downloading.
    pub fn download_kanji_dictionary(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.lyrics.kanji,
            KanjiDictionary::Embedded
                | KanjiDictionary::Ready(_)
                | KanjiDictionary::Downloading { .. }
        ) {
            return;
        }
        let Some(dir) = kanji_dictionary_dir() else {
            self.lyrics.kanji = KanjiDictionary::Failed(t!("lyrics.no_cache_dir").into());
            cx.notify();
            return;
        };
        let source = DictionarySource::ipadic();
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(std::sync::Mutex::new((0u64, source.size)));
        self.lyrics.kanji = KanjiDictionary::Downloading {
            done: 0,
            total: source.size,
            cancel: cancel.clone(),
        };
        cx.notify();
        let executor = cx.background_executor().clone();
        let shared = progress.clone();
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(250)).await;
                let (done, total) = *progress.lock().unwrap_or_else(|e| e.into_inner());
                let keep = this
                    .update(cx, |this, cx| match &mut this.lyrics.kanji {
                        KanjiDictionary::Downloading {
                            done: d, total: t, ..
                        } => {
                            (*d, *t) = (done, total);
                            cx.notify();
                            true
                        }
                        _ => false,
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let state = off_thread(move || {
                let path = dictionary::install(&source, &dir, &cancel, |done, total| {
                    *shared.lock().unwrap_or_else(|e| e.into_inner()) = (done, total);
                })?;
                dictionary::load(&path)?;
                Ok::<_, music_core::Error>(dictionary::installed_size(&source, &dir))
            })
            .await;
            drop(ticker);
            let state = match state {
                Ok(bytes) => KanjiDictionary::Ready(bytes),
                Err(e) if e.to_string().contains("cancelled") => KanjiDictionary::Missing,
                Err(e) => KanjiDictionary::Failed(e.to_string()),
            };
            this.update(cx, |this, cx| this.kanji_dictionary_changed(state, cx))
                .ok();
        })
        .detach();
    }

    pub fn cancel_kanji_dictionary(&mut self, cx: &mut Context<Self>) {
        if let KanjiDictionary::Downloading { cancel, .. } = &self.lyrics.kanji {
            cancel.store(true, Ordering::Relaxed);
        }
        cx.notify();
    }

    /// Deletes the downloaded dictionary (kana-only romanization again).
    pub fn remove_kanji_dictionary(&mut self, cx: &mut Context<Self>) {
        if let Some(dir) = kanji_dictionary_dir()
            && let Err(e) = dictionary::remove(&DictionarySource::ipadic(), &dir)
        {
            self.kanji_dictionary_changed(KanjiDictionary::Failed(e.to_string()), cx);
            return;
        }
        self.kanji_dictionary_changed(KanjiDictionary::Missing, cx);
    }

    /// New dictionary state; lyrics already on screen are romanized again.
    fn kanji_dictionary_changed(&mut self, state: KanjiDictionary, cx: &mut Context<Self>) {
        if let Some(started) = self.trace {
            let what = match &state {
                KanjiDictionary::Ready(bytes) => format!("ready ({bytes} bytes)"),
                KanjiDictionary::Failed(e) => format!("failed: {e}"),
                KanjiDictionary::Missing => "not installed".into(), // i18n-ignore
                _ => "…".into(),
            };
            eprintln!(
                "music-gpui: [{:6.2}s] kanji dictionary {what}",
                started.elapsed().as_secs_f64()
            );
        }
        let reload = matches!(state, KanjiDictionary::Ready(_))
            || matches!(self.lyrics.kanji, KanjiDictionary::Ready(_));
        self.lyrics.kanji = state;
        if reload {
            // Prepared lyrics carry their romanization: prepare them again.
            for state in self.lyrics.states.values_mut() {
                if let LyricsState::Ready(prepared) = state {
                    *prepared = Arc::new(prepared.reromanized());
                }
            }
        }
        cx.notify();
    }

    /// Forgets a failed translation so the next render asks again.
    pub fn retry_translation(&mut self, cx: &mut Context<Self>) {
        self.lyrics
            .translations
            .retain(|_, s| !matches!(s, TranslationState::Failed { .. }));
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use music_core::lyrics::RawLyrics;

    #[test]
    fn prepares_duets_background_vocals_and_romanization() {
        let ttml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttm="http://www.w3.org/ns/ttml#metadata" xmlns:itunes="http://music.apple.com/lyric-ttml-internal">
<head><metadata><ttm:agent type="person" xml:id="v1"/><ttm:agent type="person" xml:id="v2"/></metadata></head>
<body><div>
<p begin="00:01.000" end="00:03.000" ttm:agent="v1"><span begin="00:01.000" end="00:02.000">こんにちは</span> <span ttm:role="x-bg"><span begin="00:02.000" end="00:03.000">(yeah)</span></span></p>
<p begin="00:03.000" end="00:05.000" ttm:agent="v2"><span begin="00:03.000" end="00:05.000">hello</span></p>
</div></body></tt>"#;
        let result = RawLyrics::new("Test", ttml).parse(None).expect("parses");
        let prepared = PreparedLyrics::new(result);
        assert_eq!(prepared.kind, SyncKind::Word);
        assert_eq!(prepared.lines.len(), 2);
        assert!(!prepared.lines[0].secondary_singer);
        assert!(prepared.lines[1].secondary_singer);
        assert!(prepared.lines[0].background.is_some());
        let (script, roman) = prepared.romanized[0].clone().expect("kana romanized");
        assert_eq!(script, Script::Japanese);
        assert!(
            roman.contains("konnichiha") || roman.contains("konnichiwa"),
            "{roman}"
        );
        assert!(prepared.romanized[1].is_none(), "latin stays as is");
    }

    #[test]
    fn reromanizing_keeps_lines_and_provider_romanization() {
        let ttml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttm="http://www.w3.org/ns/ttml#metadata"><body>
<p begin="1" end="2"><span begin="1" end="2">사랑</span><span ttm:role="x-roman">sa-rang</span></p>
<p begin="2" end="3"><span begin="2" end="3">君の名は</span></p></body></tt>"#;
        let prepared = PreparedLyrics::new(RawLyrics::new("T", ttml).parse(None).unwrap());
        let again = prepared.reromanized();
        assert_eq!(again.lines.len(), 2);
        assert_eq!(again.texts(), prepared.texts());
        assert_eq!(again.romanized[0].as_ref().unwrap().1, "sa-rang");
        assert_eq!(again.romanized[1].as_ref().unwrap().0, Script::Japanese);
    }

    #[test]
    fn plain_lyrics_keep_their_lines() {
        let result = RawLyrics::new("Plain", "first line\nzweite Zeile")
            .parse(None)
            .expect("plain parses");
        let prepared = PreparedLyrics::new(result);
        assert_eq!(prepared.kind, SyncKind::Plain);
        assert!(prepared.synced.is_none());
        assert_eq!(prepared.plain.len(), 2);
        assert_eq!(prepared.texts()[1], "zweite Zeile");
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "music-gpui-lyrics-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn remembers_source_choice_and_nudge_per_track() {
        let dir = temp_dir("tracks");
        let path = dir.join(TrackLyricsStore::FILE_NAME);
        let mut store = TrackLyricsStore::load(Some(path.clone()));
        assert!(store.set_source("vid1", Some("LrcLib".into())));
        assert!(
            !store.set_source("vid1", Some("LrcLib".into())),
            "unchanged"
        );
        assert_eq!(store.nudge("vid2", 100), 100);
        assert_eq!(store.nudge("vid2", 100), 200);
        assert_eq!(store.nudge("vid2", -350), -200, "snapped to 0.1 s");
        assert_eq!(store.nudge("vid3", 60_000), MAX_OFFSET_MS);
        store.flush();

        // A new session reads the choices back.
        let mut again = TrackLyricsStore::load(Some(path.clone()));
        assert_eq!(again.source("vid1"), Some("LrcLib"));
        assert_eq!(again.offset_ms("vid2"), -200);
        assert_eq!(again.offset_ms("unknown"), 0);
        // Forgetting both leaves nothing behind for the track.
        assert!(again.set_source("vid1", None));
        assert!(again.reset_offset("vid2"));
        assert!(again.get("vid1").is_none() && again.get("vid2").is_none());
        again.flush();
        let json = std::fs::read_to_string(&path).unwrap();
        assert!(!json.contains("vid1") && json.contains("vid3"), "{json}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn track_store_is_capped_and_survives_a_bad_file() {
        let dir = temp_dir("cap");
        let path = dir.join(TrackLyricsStore::FILE_NAME);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        let mut store = TrackLyricsStore::load(Some(path.clone()));
        assert_eq!(store.len(), 0);
        for i in 0..MAX_TRACKS + 5 {
            store.nudge(&format!("v{i}"), 100);
        }
        assert_eq!(store.len(), MAX_TRACKS);
        // The oldest changes go first.
        assert!(store.get("v0").is_none());
        assert!(store.get(&format!("v{}", MAX_TRACKS + 4)).is_some());
        store.flush();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn nudge_shifts_lyrics_time() {
        let mut hub = LyricsHub::default();
        let at = Duration::from_secs(10);
        assert_eq!(hub.lyrics_position("v", at), at);
        hub.tracks.nudge("v", 300); // lyrics 0.3 s later
        assert_eq!(hub.lyrics_position("v", at), Duration::from_millis(9700));
        assert_eq!(hub.playback_position("v", Duration::from_millis(9700)), at);
        hub.tracks.nudge("v", -500); // now 0.2 s earlier
        assert_eq!(hub.lyrics_position("v", at), Duration::from_millis(10_200));
        assert_eq!(
            hub.lyrics_position("v", Duration::from_millis(100)),
            Duration::from_millis(300)
        );
        hub.tracks.nudge("v", 2000);
        assert_eq!(hub.lyrics_position("v", Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn keeps_sources_of_the_last_few_tracks() {
        let mut hub = LyricsHub::default();
        for i in 0..5 {
            hub.set_sources(format!("v{i}"), SourcesInfo::default());
        }
        assert_eq!(hub.sources.len(), SOURCES_KEPT);
        assert!(hub.sources_for("v0").is_none());
        assert!(hub.sources_for("v4").is_some());
    }

    #[test]
    fn keeps_the_last_tracks_lyrics_only() {
        let mut hub = LyricsHub::default();
        hub.put_state(
            "playing".into(),
            LyricsState::Failed("x".into()),
            Some("playing"),
        );
        hub.put_state("loading".into(), LyricsState::Loading, Some("playing"));
        for i in 0..20 {
            hub.put_state(
                format!("v{i}"),
                LyricsState::Missing { generation: 0 },
                Some("playing"),
            );
        }
        assert_eq!(hub.states.len(), STATES_KEPT);
        assert!(hub.states.contains_key("playing") && hub.states.contains_key("loading"));
        assert!(hub.states.contains_key("v19") && !hub.states.contains_key("v0"));
        assert_eq!(hub.recent.len(), STATES_KEPT);
    }
}
