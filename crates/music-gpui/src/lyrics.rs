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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{Context, SharedString};
use music_core::Track;
use music_core::lyrics::romanize::dictionary::{self, DictionarySource};
use music_core::lyrics::romanize::{Script, detect_script, romanize_line};
use music_core::lyrics::translate::{CachedTranslator, LyricsTranslator, is_rate_limited};
use music_core::lyrics::{
    LyricLine, Lyrics, LyricsCache, LyricsQuery, LyricsResult, LyricsService, MemoryLyricsCache,
    SyncKind,
};

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
    directories::ProjectDirs::from("", "", crate::identity::APP_ID)
        .map(|d| d.cache_dir().join("ja-dict"))
}

/// Lyrics service + cache and the per-track state the app keeps.
pub struct LyricsHub {
    pub service: Arc<LyricsService>,
    pub cache: Arc<dyn LyricsCache>,
    /// Bumped whenever the provider settings change (refetch "missing" tracks).
    pub generation: u64,
    /// Per video id; small, never evicted.
    pub states: HashMap<String, LyricsState>,
    /// Per (video id, translator key).
    pub translations: HashMap<(String, String), TranslationState>,
    pub kanji: KanjiDictionary,
}

impl Default for LyricsHub {
    fn default() -> Self {
        let cache: Arc<dyn LyricsCache> = Arc::new(MemoryLyricsCache::new());
        Self {
            service: Arc::new(LyricsService::default().with_cache(cache.clone())),
            cache,
            generation: 0,
            states: HashMap::new(),
            translations: HashMap::new(),
            kanji: KanjiDictionary::Unknown,
        }
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
        self.lyrics
            .states
            .insert(track.video_id.clone(), LyricsState::Loading);
        let service = self.lyrics.service.clone();
        let generation = self.lyrics.generation;
        let query = LyricsQuery::from_track(track);
        let id = track.video_id.clone();
        let trace = self.trace;
        cx.spawn(async move |this, cx| {
            let result = off_thread(move || {
                let result = if refresh {
                    service.refresh(&query)
                } else {
                    service.fetch(&query)
                };
                result.map(|r| r.map(|r| Arc::new(PreparedLyrics::new(r))))
            })
            .await;
            this.update(cx, |this, cx| {
                let state = match result {
                    Ok(Some(prepared)) => {
                        if let Some(started) = trace {
                            eprintln!(
                                "music-gpui: [{:6.2}s] lyrics {id}: {} ({:?}, {} lines)",
                                started.elapsed().as_secs_f64(),
                                prepared.provider,
                                prepared.kind,
                                prepared.lines.len().max(prepared.plain.len()),
                            );
                        }
                        LyricsState::Ready(prepared)
                    }
                    Ok(None) => LyricsState::Missing { generation },
                    Err(e) => LyricsState::Failed(e.to_string()),
                };
                this.lyrics.states.insert(id, state);
                cx.notify();
            })
            .ok();
        })
        .detach();
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
            self.lyrics.kanji = KanjiDictionary::Failed("no cache directory".into());
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
                KanjiDictionary::Missing => "not installed".into(),
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
}
