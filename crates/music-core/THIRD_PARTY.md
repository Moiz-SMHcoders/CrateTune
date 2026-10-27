# Third-party code in music-core

Parts of music-core are ported from **Hummingbird**, a native Rust music player
(<https://git.mailliw.org/hummingbird/hummingbird>, GitHub mirror
<https://github.com/hummingbird-player/hummingbird>), licensed under the
Apache License 2.0. Upstream commit: `e15e8ba0ef6ff5445294030d373e3c8ccc7d780c`.

Copyright Hummingbird contributors, Apache-2.0. All ported files are modified;
each carries a header naming its upstream source. A copy of the license is at
<https://www.apache.org/licenses/LICENSE-2.0>.

| music-core file | Upstream file(s) | Changes |
|---|---|---|
| `src/equalizer.rs` | `src/playback/dsp/equalizer.rs`, `src/settings/equalizer.rs` | Settings types merged in without serde; presets + 10-band graphic layout; interleaved-`f32` live stage (`EqControl`/`EqStage`) for the decoder; deterministic test noise instead of `rand`. DSP unchanged. |
| `src/media_controls/mod.rs` | `src/services/controllers.rs` | GPUI/tokio/async-trait removed; one `Backend` trait driven by `PlaybackState` snapshots (`MediaControls::update` diffs and only publishes changes); events go to a callback, with a `Player` router. |
| `src/media_controls/macos.rs` | `src/services/controllers/macos.rs` | Callback instead of the playback bridge; playback rate published; artwork from URL (fetched on a thread) or bytes without leaking the `NSImage`; stop command; handlers removed on drop. |
| `src/media_controls/windows.rs` | `src/services/controllers/windows.rs` | Callback; blocking `join()` instead of `.await`; artwork from URL; no shuffle/repeat; error instead of panic without an `HWND`. |
| `src/media_controls/mpris.rs` | `src/services/controllers/mpris.rs` | No tokio (zbus' own executor + `block_on`); std `Mutex`; callback; track ids; artwork URLs passed through; no shuffle/repeat; configurable bus name/identity. |
| `src/scrobble/mod.rs` | `src/services/mmb.rs`, play-time bookkeeping in `src/services/mmb/{lastfm,listenbrainz}.rs` | Async trait replaced by a pure `ScrobbleTracker` over `PlaybackState` snapshots (wall-clock-bounded play time, replays, now-playing on resume) and a `Scrobbler` worker thread. |
| `src/scrobble/lastfm.rs` | `src/services/mmb/lastfm.rs`, `lastfm/client.rs`, `lastfm/types.rs` | Blocking `ureq`; `serde_json::Value` instead of serde derives; API errors surfaced; runtime credentials instead of env vars; documented auth flow. |
| `src/scrobble/listenbrainz.rs` | `src/services/mmb/listenbrainz.rs`, `listenbrainz/client.rs`, `listenbrainz/types.rs` | Blocking `ureq`; `json!` payloads; YouTube Music `origin_url`/`music_service`; configurable client name and API root. |
| `src/discord.rs` | `src/services/mmb/discord.rs` | Snapshot-driven `PresenceTracker`; IPC on a worker thread with update coalescing (≤1 per 4 s) instead of async + `watch`; caller-supplied application id/name; YouTube Music artwork URL. |

## ArchiveTune (feat/lyrics)

The lyrics stack is ported from **ArchiveTune** (<https://github.com/rukamori/ArchiveTune>),
© Rukamori and ArchiveTune contributors, licensed under the GNU General Public
License v3.0 (this crate is GPL-3.0-only as a result). Ported files carry a
`// Ported from ArchiveTune <path>` header. Paths are relative to the ArchiveTune
repository root.

| music-core file | Upstream file(s) | Changes |
|---|---|---|
| `src/lyrics/ttml.rs` | `lyrics/betterlyrics/.../betterlyrics/TTMLParser.kt` | `roxmltree` instead of the JDK DOM (DTDs rejected); undeclared prefixes declared generically; output mapped to `Lyrics` + `LineExtras` (background vocals as their own timed line, untimed spans slotted between timed neighbours); Apple's syllable-spaced Korean transliteration rejoined. |
| `src/lyrics/qrc.rs` | `lyrics/betterlyrics/.../betterlyrics/QRCParser.kt` | `LyricContent` ends at the element's closing quote (unescaped quotes in the wild); more credit-line keywords (English "… by", engineers, CJK credits). |
| `src/lyrics/format.rs`, `src/lyrics/lrc.rs` (parts) | `app/.../lyrics/LyricsUtils.kt` (normalization, format detection, translation merge of duplicated LRC stamps) | Folded into the existing LRC parser; blank stamps end lines. |
| `src/lyrics/service.rs` | `app/.../lyrics/LyricsHelper.kt`, `LyricsProviderSettings.kt`, `constants/PreferenceKeys.kt` (`DefaultLyricsProviderOrder`) | Threads + channel instead of coroutines; main-thread dispatch so a sequential run can stop early; per-provider timeouts; caching delegated to a `LyricsCache` hook; `priority_grace` default 2 s (upstream 350 ms). |
| `src/lyrics/providers/lrclib.rs` | `lyrics/lrclib/.../lrclib/LrcLib.kt` | Exact `/api/get` before `/api/search`. |
| `src/lyrics/providers/kugou.rs` | `lyrics/kugou/.../kugou/KuGou.kt` (+ models) | String or numeric candidate ids; own base64 decoder. |
| `src/lyrics/providers/simpmusic.rs` | `lyrics/simpmusic/.../simpmusic/SimpMusicLyrics.kt` | Prefers the word-synced `richSyncLyrics`; HTML entities decoded. |
| `src/lyrics/providers/betterlyrics.rs` | `lyrics/betterlyrics/.../betterlyrics/BetterLyrics.kt`, `app/.../lyrics/BetterLyrics*Provider.kt` | Optional `X-API-Key`; nested JSON unwrapping up to depth 3. |
| `src/lyrics/providers/youlyplus.rs` | `lyrics/youlyplus/.../youlyplus/YouLyPlus.kt` | Background syllables excluded from the main line; last syllable closed with an end tag. |
| `src/lyrics/providers/unison.rs` | `lyrics/unison/.../unison/Unison.kt` | — |
| `src/lyrics/providers/paxsenix.rs` | `lyrics/paxsenix/.../paxsenix/PaxsenixLyrics.kt`, `app/.../lyrics/Paxsenix*Provider.kt` | Word timestamps kept when present (enhanced LRC). |
| `src/lyrics/providers/megalobiz.rs` | `app/.../lyrics/MegalobizLyricsProvider.kt` | — |
| `src/lyrics/providers/binimum.rs` | `app/.../lyrics/BinimumLyricsProvider.kt` | — |
| `src/lyrics/providers/youtube.rs` | `app/.../lyrics/YouTubeLyricsProvider.kt`, `core/.../innertube/YouTube.kt` (`next` lyrics tab, `lyrics`) | Self-contained WEB_REMIX `next` + `browse`; tab found by page type. |
| `src/lyrics/translate.rs` | `app/.../lyrics/translation/StandardLyricsTranslationRepository.kt`, `TranslateLyricsUseCase.kt`, `app/.../ai/AiTextService.kt`, `AiLyricsTranslator.kt`, `AiJsonUtils.kt`, `AiRequestParameters.kt` | Line-aligned API instead of rebuilding LRC/TTML documents; repeated lines sent once; Google batches joined by newlines with halving on mismatch. |
| `src/lyrics/romanize/japanese.rs` | `app/.../lyrics/LyricsUtils.kt` (`KANA_ROMAJI_MAP`, `katakanaToRomaji`, `romanizeJapanese`) | Hiragana handled directly; extra digraphs; ー repeats the vowel; in-token sokuon; lindera instead of Kuromoji (feature `japanese-dict`). |
| `src/lyrics/romanize/korean.rs` | `app/.../lyrics/LyricsUtils.kt` (`HANGUL_ROMAJA_MAP`, `romanizeKorean`) | Index tables; context forms replace final + next initial; silent ㅎ before vowels; ㅢ → ui. |
| `src/lyrics/romanize/mod.rs` | `app/.../lyrics/LyricsUtils.kt` (script detection, provided-romanization preference) | `pinyin`/`deunicode` and own tables instead of ICU `Any-Latin`. |
