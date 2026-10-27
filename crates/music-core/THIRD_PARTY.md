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

## ArchiveTune (feat/library)

Parts of music-core are ported from **ArchiveTune**, an Android YouTube Music client
(© Rukamori and ArchiveTune contributors), licensed under the GNU General Public License
v3.0 — the reason this project is GPL-3.0. Reference checkout: `archivetune-ref`
(`app/src/main/kotlin/moe/rukamori/archivetune/…`, Room schemas in `app/schemas/`).
Each ported file carries a header naming its upstream source; all were rewritten from
Kotlin/Room/Android APIs into blocking Rust over SQLite (`rusqlite`) and `lofty`.

| music-core file | Upstream file(s) | Changes |
|---|---|---|
| `src/library/schema.rs` | `db/entities/*.kt`, `app/schemas/` | Condensed table set (song, artist, album, maps, playlist + entries with `setVideoId`, play events, search history, lyrics, key/value, local files) as one `user_version` migration; no formats/podcasts/top-mix/aliases tables. |
| `src/library/mod.rs` | `db/MusicDatabase.kt`, `db/LocalMusicIdentity.kt` | Single mutex-guarded connection, `user_version` migrations, name normalization without NFKC, key/value store, saved queue. |
| `src/library/songs.rs` | `db/DatabaseDao.kt`, `db/entities/{SongEntity,ArtistEntity,AlbumEntity}.kt` | Like/library semantics of `toggleLike`/`toggleLibrary`; upserts from `Track`; sorted/filtered listings and local search as dynamic SQL; synthetic `LA:`/`LB:` ids for name-only artists/albums. |
| `src/library/playlists.rs` | `db/entities/{PlaylistEntity,PlaylistSongMap}.kt`, `db/DatabaseDao.kt` | Local + mirrored playlists, contiguous positions with insert/move/remove. |
| `src/library/history.rs` | `db/DatabaseDao.kt`, `db/entities/{Event,SearchHistory,LyricsEntity}.kt` | Play events with per-song aggregates recomputed on delete; day grouping in SQL with a caller-chosen time zone. |
| `src/library/stats.rs` | `db/DatabaseDao.kt` (`mostPlayed*`, `listeningBy*`, `listeningTotals`), `viewmodels/YearInMusicViewModel.kt`, `ui/screens/{StatsScreen,YearInMusicScreen}.kt` | Half-open ranges, explicit `TimeZone` (local or fixed offset), month/day breakdowns, top hour/busiest day; no artist blocking. |
| `src/library/backup.rs` | `backup/BackupArchiveRepository.kt` | SQLite online backup API instead of copying DB/WAL files; JSON manifest + caller's settings JSON instead of preferences XML; validated restore into the live connection. |
| `src/local_files.rs` | `localmedia/{LocalSongScanner,LocalAudioMetadataReader,SupportedLocalAudio}.kt` | Directory walk instead of MediaStore; `lofty` instead of `MediaMetadataRetriever`; artwork deduplicated by content hash (not resized); ReplayGain + embedded lyrics; mtime/size incremental rescan. |

`src/downloads.rs` follows the split between user downloads and the player cache in
ArchiveTune's `playback/DownloadUtil.kt` but contains no ported code.

## ArchiveTune (feat/innertube)

The InnerTube (YouTube Music) layer is ported from **ArchiveTune**
(<https://github.com/rukamori/ArchiveTune>, Kotlin), © Rukamori and ArchiveTune
contributors, licensed under GPL-3.0 (this project is GPL-3.0 as well). Logic, endpoint
names, request bodies and renderer paths were ported; the code is a rewrite over
`serde_json::Value` with blocking `ureq` instead of Ktor/kotlinx-serialization. Each
file carries a header naming its upstream source.

| music-core file | Upstream file(s) (`core/src/main/kotlin/moe/rukamori/archivetune/innertube/`) | Changes |
|---|---|---|
| `src/innertube/mod.rs` | `YouTube.kt` | Blocking methods on a cloneable `InnerTube` config; results are UI-agnostic serde models; charts take a country via `formData`; no automix auto-follow (exposed as `WatchQueue::automix`). |
| `src/innertube/client.rs` | `InnerTube.kt` (`ytClient`, request bodies), `models/Context.kt`, `models/YouTubeClient.kt` | WEB_REMIX only; anonymous `visitorData` remembered per process so continuations work; no retries, PoTokens or player clients. |
| `src/innertube/parse.rs` | `pages/PageHelper.kt`, `pages/SearchPage.kt`, `pages/HomePage.kt`, `pages/RelatedPage.kt`, `pages/NextPage.kt`, `models/{Runs,Thumbnails,Continuation}.kt` | One generic item parser keyed on endpoint page/video types (not localized labels); subtitle runs split into artists/album/year/duration groups. |
| `src/innertube/pages.rs` | `YouTube.kt` (`home`, `explore`, `moodAndGenres`, `getChartsPage`, `album`, `artist`, `artistItems`, `playlist`, `search*`, `next`, `lyrics`, `related`), `pages/*Page.kt` | Generic `Shelf` model instead of per-page section types; chart countries decoded from the menu's entity keys. |
| `src/innertube/auth.rs` | `InnerTube.kt` (SAPISIDHASH header), `PlaybackAuthState.kt`, `utils/Utils.kt` (`sha1`, cookie parsing) | `Auth` struct with configurable `X-Goog-AuthUser`; secrets redacted from `Debug`. |
| `src/innertube/account.rs` | `YouTube.kt` / `InnerTube.kt` (account menu, accounts list, library, history, like, subscribe, playlist create/edit/delete), `pages/{LibraryPage,HistoryPage}.kt`, `models/body/*.kt`, `models/response/AccountMenuResponse.kt` | Library feedback tokens exposed per item; playlist edits as a `PlaylistAction` enum; no playlist cover upload. |

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

## ArchiveTune (feat/playback)

Playback-engine behaviour is ported from **ArchiveTune**
(<https://github.com/rukamori/ArchiveTune>, commit `c7f0e243c637e039bce4b92fecdf7179804114fb`), © Rukamori and ArchiveTune contributors,
GPL-3.0. Files carry a header naming their upstream source.

| music-core file | Upstream file(s) | What was ported |
|---|---|---|
| `src/sleep.rs` | `playback/SleepTimer.kt` | Sleep timer model: a duration timer or "pause when the song ends", re-arming replaces the previous mode, clearing after it fires; driven by engine ticks instead of coroutines. |
| `src/deck.rs` | `playback/CrossfadeHandoffPolicy.kt`, `playback/MusicService.kt` (`scheduleCrossfade`, `effectiveCrossfadeDuration`, `applyCrossfadeVolumes`) | Equal-power crossfade gains (cos/sin quarter circle); crossfade timing (minimum 500 ms, 150 ms end guard, shortened when started late); mixed sample-accurately in one source instead of two ExoPlayers. |
| `src/player.rs` | `playback/MusicService.kt` (`skipOnError`, `CROSSFADE_PREPARE_AHEAD_MS`), `playback/preload/NextStreamPreloader.kt` | Auto-skip to the next item on load errors with a cap on consecutive skips; preparing the next stream ahead of the end (30 s) and replacing the preload target when the queue changes. |

Paths are relative to `app/src/main/kotlin/moe/rukamori/archivetune/`. All Kotlin/Android
specifics (Media3, coroutines, DataStore) are replaced by the blocking engine thread.

Libraries used by the engine (not ported code): `opus-decoder` (MIT OR Apache-2.0,
pure-Rust Opus), `libsonic` (Apache-2.0, Rust port of Bill Cox's Sonic, used for speed/pitch).

## ArchiveTune (feat/integrations)

SponsorBlock, playlist import and Music Together are ported from **ArchiveTune**
(<https://github.com/rukamori/ArchiveTune>), © Rukamori and ArchiveTune contributors,
GPL-3.0. Files carry a header naming their upstream source. Paths are relative to
`app/src/main/kotlin/moe/rukamori/archivetune/` unless noted. ArchiveTune's online
Together relay and its Spotify web-player login (`sp_dc` cookie + TOTP secret from a
third-party gist) are **not** ported.

| music-core file | Upstream file(s) | Changes |
|---|---|---|
| `src/sponsorblock.rs` | `sponsorblock/SponsorBlockModels.kt`, `SponsorBlockRepository.kt`, `SponsorBlockUseCases.kt`, `SponsorBlockPlaybackController.kt` | Blocking `ureq`; LRU cache keyed by URL/video/categories; category kept on merged segments; `SkipTracker` (once-per-segment, end tolerance, minimum seek distance) is pollable instead of a coroutine loop; `music_offtopic` is the default category. |
| `src/playlist_import/matcher.rs` | `utils/FuzzyMatcher.kt`, `modules/ModuleTrackMatcher.kt` (title clean-up, recording markers), `spotifycore/src/.../spotify/SpotifyMapper.kt` (duration tiers) | One score: FuzzyMatcher's 0.6 title / 0.4 artist Jaro–Winkler on NFKD-folded text, recording-marker mismatch as a penalty instead of a rejection, duration and explicit penalties. |
| `src/playlist_import/mod.rs` | `playlistimport/ImportSongResolver.kt`, `spotify/SpotifyPlaybackResolver.kt` | Scoped worker threads instead of a coroutine semaphore; direct video ids; second query (`artist title`) when the first finds nothing good; cancel flag. CSV / M3U / JSON readers and writers are new. |
| `src/together/messages.rs` | `together/TogetherMessages.kt`, `TogetherModels.kt`, `TogetherJson.kt` | serde enums with the same `type` discriminators and camelCase fields; host transfer not implemented. |
| `src/together/sync.rs` | `together/TogetherClock.kt`, `TogetherPlaybackSync.kt` | Clock offset applied with the correct sign when converting the host's timestamp; `GuestSync` bundles stale-state dropping, drift thresholds and echo suppression into a pure planner. |
| `src/together/server.rs` | `together/TogetherServer.kt` | std threads + `tungstenite` instead of Ktor; room-settings checks (add / control) on the server; ban list per session. |
| `src/together/client.rs` | `together/TogetherClient.kt` | One worker thread polling the socket; heartbeats and the clock estimate live in the client. |
| `src/together/link.rs` | `together/TogetherLink.kt` | Own `music-together://join?…` scheme; IPv6 hosts; LAN address via the default route. |

`src/spotify.rs` (official Web API, Authorization Code + PKCE with the user's own Client
ID) contains no ported code; the playlist / liked-songs model follows
`spotifycore/src/.../spotify/models/*.kt`.
