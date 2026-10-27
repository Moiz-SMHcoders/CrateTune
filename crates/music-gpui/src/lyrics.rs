//! Lyrics for the Now Playing screen: fetching through music-core's
//! `LyricsService` (provider order / switches / keys from Settings → Lyrics,
//! results cached in memory), preparing them for the renderer (word offsets,
//! background vocals, duet sides, romanization) and on-demand translation,
//! cached per track and translator settings.
//!
//! Persistent caching: [`MusicApp::set_lyrics_cache`] swaps the in-memory
//! `LyricsCache` for another one (e.g. the library database's table); the
//! service is rebuilt around it.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{Context, SharedString};
use music_core::Track;
use music_core::lyrics::romanize::{Script, detect_script, romanize_line};
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
    Failed(String),
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
        }
    }
}

impl MusicApp {
    /// Rebuilds the service from Settings → Lyrics (providers, keys).
    pub(crate) fn rebuild_lyrics_service(&mut self, cx: &mut Context<Self>) {
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
        let slot = (video_id.to_owned(), key);
        match self.lyrics.translations.get(&slot) {
            Some(TranslationState::Ready(lines)) => return Some(Ok(lines.clone())),
            Some(TranslationState::Failed(e)) => return Some(Err(e.clone())),
            Some(TranslationState::Loading) => return None,
            None => {}
        }
        let translator = match self.settings.read(cx).translator() {
            Ok(t) => t,
            Err(e) => return Some(Err(e)),
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
                translator.translate_lines(&lines, &language)
            })
            .await;
            this.update(cx, |this, cx| {
                let state = match result {
                    Ok(lines) => TranslationState::Ready(Arc::new(
                        lines.into_iter().map(|l| l.map(Into::into)).collect(),
                    )),
                    Err(e) => TranslationState::Failed(e.to_string()),
                };
                this.lyrics.translations.insert(slot, state);
                cx.notify();
            })
            .ok();
        })
        .detach();
        None
    }

    /// Forgets a failed translation so the next render asks again.
    pub fn retry_translation(&mut self, cx: &mut Context<Self>) {
        self.lyrics
            .translations
            .retain(|_, s| !matches!(s, TranslationState::Failed(_)));
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
