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
| `src/spectrum.rs` | `src/playback/dsp/spectrum.rs` (mono tap), `src/ui/equalizer/spectrum.rs` (analyzer) | Approach adapted, code rewritten: one tap after the whole mix (pre-volume) as a fixed ring of atomics read for the newest window on demand instead of an `rtrb` queue drained by a timer task; 2048-point `rustfft` instead of 4096-point `realfft`; the same Hann coherent-gain normalization (full-scale sine = 0 dBFS), tilt around 1 kHz and decay-to-floor once the tap runs dry, but log-spaced bars (max bin per band, interpolated when narrower than a bin) normalized to 0..1 with time-based attack/decay, peak hold and an RMS level. |
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
| `src/innertube/account.rs` | `YouTube.kt` / `InnerTube.kt` (account menu, accounts list, library, history, like, subscribe, playlist create/edit/delete, `startPlaylistCoverUpload` / `uploadPlaylistCover` / `setPlaylistCustomCover`, `uploadCustomPlaylistCover`), `pages/{LibraryPage,HistoryPage}.kt`, `models/body/*.kt` (incl. `EditPlaylistBody.kt` custom-thumbnail actions), `models/response/{AccountMenuResponse,ImageUploadResponse}.kt` | Library feedback tokens exposed per item; playlist edits as a `PlaylistAction` enum; cover upload as one blocking call (request building unit-tested, live-untested). |
| `src/innertube/pages.rs` (podcasts, long albums) | `pages/PodcastPage.kt` (`fromResponse`, `continuationFromResponse`, `toEpisodeItem`, `episodeDateText`), `YouTube.kt` (`podcast`, `podcastContinuation`, `albumSongs`), `pages/AlbumPage.kt` (song continuations) | Episodes as `Episode` (track + date + description + `MPED` page id); episode pages parsed too; long albums continue through the album playlist page by page (`album_tracks`) or all at once (`album_complete`, 20 pages max). |
| `src/model.rs` (`MusicVideoType`, `Track::is_video`) | `models/Endpoint.kt` (`MUSIC_VIDEO_TYPE_*`), `models/YTItem.kt` (`filterVideo`) | Video type kept on every track; OMV / UGC count as videos, as in ArchiveTune. |

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
| `src/lyrics/service.rs` | `app/.../lyrics/LyricsHelper.kt`, `LyricsProviderSettings.kt`, `constants/PreferenceKeys.kt` (`DefaultLyricsProviderOrder`) | Threads + channel instead of coroutines; main-thread dispatch so a sequential run can stop early; per-provider timeouts; caching delegated to a `LyricsCache` hook; `priority_grace` default 300 ms (upstream 350 ms) now that providers are asked in waves (see below). |
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

## Zuno and Limusic (feat/lyrics-race)

Two lyrics ideas come from other desktop YouTube Music clients; the code was written for
this project in Rust, following their approach (no file was copied):

* **Zuno** (<https://github.com/noFAYZ/zuno>, © noFAYZ, Apache License 2.0, reference commit
  `1fa9a96`): asking lyric sources in waves (fast, trusted sources raced first, the rest only
  without a usable result), the per-source attempt log shown in the UI, a preferred source that
  joins the first wave, and the per-track timing nudge. Apache-2.0 is compatible with this
  project's GPL-3.0.
* **Limusic** (<https://github.com/SimoHypers/limusic>, © SimoHypers, GPL-3.0, reference commit
  `d82372c`): matching search hits on the playing cut's length (closest within a tolerance,
  unknown lengths last but kept, `best_by_duration`), cancelling providers still in flight
  once a hit is in, and per-song choices that survive a change of the provider order
  (`choose_source`, `set_offset`).

| music-core file | Upstream file(s) | What was adapted |
|---|---|---|
| `src/lyrics/service.rs` (waves, `SourceAttempt`, `SearchOptions::pinned`, cancellation) | Zuno `src/datasource/youtube/lyricsSources.ts` (`LYRICS_SOURCES` waves, `planLyricsWaves`, `pickBestLyrics`, `skippedAttempt`, `sortAttempts`), `src/datasource/youtube/YouTubeMusicDataSource.ts` (`fetchSyncedLyrics`, `runLyricsSource`); Limusic `src-tauri/src/lyrics.rs` (`fetch`: dropping the in-flight answers on a hit) | Waves per provider (`ProviderId::default_wave`, timed on real tracks) instead of a fixed table; a wave also ends after `wave_timeout`, its slow providers keep running beside the next; results ranked (pinned, synced, length fit, names, word timing, priority) rather than by table rank alone; cancellation through a per-thread flag checked before each HTTP request. |
| `src/lyrics/types.rs` (`MatchInfo`, `DurationFit`, `EXACT_DURATION_SECS`), `src/lyrics/format.rs` (`declared_length`) | Limusic `src-tauri/src/lyrics.rs` (`best_by_duration`, `MATCH_TOLERANCE_SECS`, `kpoe_to_lyrics` length check) | Providers report the matched recording's length (or a video-id lookup); payload lengths (TTML `<body dur>`, LRC `[length:]`) fill the gaps; ±2 s counts as the same cut; lyrics singing past the track's end count as a longer cut. |
| `src/lyrics/providers/binimum.rs` (`ranked_lyrics_urls`), `lrclib.rs`, `kugou.rs`, `simpmusic.rs`, `unison.rs`, `youtube.rs` (match info) | Limusic `src-tauri/src/lyrics.rs` (`best_by_duration`, `lrclib_search_by`) | The same cut first, then word timing; each provider passes on the length / names of the candidate it picked. |
| `src/lyrics/lrc.rs` (`[offset:]`) | — (LRC convention) | Positive offsets show lyrics sooner. |

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
pure-Rust Opus; vendored with two fixes in `vendor/opus-decoder`, see its `VENDORED.md`), `libsonic` (Apache-2.0, Rust port of Bill Cox's Sonic, used for speed/pitch).

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
| `src/together/messages.rs` | `together/TogetherMessages.kt`, `TogetherModels.kt`, `TogetherJson.kt` | serde enums with the same `type` discriminators and camelCase fields, including the host-transfer messages. |
| `src/together/sync.rs` | `together/TogetherClock.kt`, `TogetherPlaybackSync.kt` | Clock offset applied with the correct sign when converting the host's timestamp; `GuestSync` bundles stale-state dropping, drift thresholds and echo suppression into a pure planner. |
| `src/together/server.rs` | `together/TogetherServer.kt` | std threads + `tungstenite` instead of Ktor; room-settings checks (add / control) on the server; ban list per session. |
| `src/together/client.rs` | `together/TogetherClient.kt` | One worker thread polling the socket; heartbeats and the clock estimate live in the client. |
| `src/together/link.rs` | `together/TogetherLink.kt` | Own `music-together://join?…` scheme; IPv6 hosts; LAN address via the default route. |

`src/spotify.rs` (official Web API, Authorization Code + PKCE with the user's own Client
ID) contains no ported code; the playlist / liked-songs model follows
`spotifycore/src/.../spotify/models/*.kt`.

## ArchiveTune (feat/release: song recognition)

The song-recognition module is ported from **ArchiveTune**'s `shazamkit` module and
recognition repository (<https://github.com/rukamori/ArchiveTune>), © Rukamori and
ArchiveTune contributors, GPL-3.0. ArchiveTune's generator follows the Shazam signature
format reverse-engineered by SongRec (GPL-3.0). Files carry a header naming their source.

| music-core file | Upstream file(s) | Changes |
|---|---|---|
| `src/recognition/signature.rs` | `shazamkit/src/main/kotlin/moe/koiverse/rukamori/shazamkit/ShazamSignatureGenerator.kt` | Same arithmetic order (bit-identical output, checked against `tests/fixtures/recognition/ReferenceSignature.java`, a Java transliteration of the Kotlin file); `crc32fast`/`base64` instead of `java.util`; a decoder for the binary format. |
| `src/recognition/shazam.rs` | `shazamkit/.../Shazam.kt`, `shazamkit/.../models/ShazamModels.kt` | Blocking `ureq` + `serde_json::Value`; one fixed user agent, neutral geolocation and the system time zone instead of random ones; no queue/cache/automatic retries; Android deep links mapped to web URLs. |
| `src/recognition/history.rs` | `app/.../musicrecognition/MusicRecognitionRepository.kt` (history) | JSON list in the library's key/value table (50 entries, newest first, de-duplicated by stable key). |
| `src/recognition/capture.rs`, `audio.rs` | `MusicRecognitionRepository.kt` (`captureAudio`) | cpal input at the device's rate, down-mixed and resampled (windowed sinc) to 16 kHz instead of Android's `AudioRecord`; cancellable, with a level callback. |

`tests/fixtures/recognition/ReferenceSignature.java` is itself a transliteration of
`ShazamSignatureGenerator.kt` (GPL-3.0). `synth_16k_mono.s16le` is synthetic audio
generated for the test (tones + deterministic noise).

## ArchiveTune (feat/modules)

The `.arch` provider-module system follows ArchiveTune's `MODULES.md` (package format and
JavaScript API v1) and ports its implementation (© Rukamori and ArchiveTune contributors,
GPL-3.0) from Kotlin + Android (QuickJS via `dokar/quickjs-kt`, OkHttp, Android Keystore)
to blocking Rust with `rquickjs`, `ureq` and a caller-supplied secret hook. Each file
carries a header naming its upstream source. The manifest `format` string
`archivetune.module` is kept verbatim for package compatibility; it is not shown in the UI.

| music-core file | Upstream file(s) | Changes |
|---|---|---|
| `src/modules/package.rs` | `modules/.../ModulePackageReader.kt`, `scripts/package_module.py` (packaging rules) | `zip` crate in memory; duplicate entries detected from the end-of-central-directory count; the `extension` runtime and `index.js` packages are rejected (`ARCH_UNSUPPORTED_runtime`); PNG checked from its IHDR instead of `BitmapFactory`; `pack_dir` is a minimal deterministic packager (no interactive studio / reports). |
| `src/modules/manifest.rs` | `modules/.../ModuleModels.kt` (`ModuleManifest.parse`, `ModuleSetting.acceptsValue`, `requireCompatible`, `moduleIcon`) | serde types; settings keep their typed JSON values instead of JSON text; app version is the host's. No `authentication` / signed sessions. |
| `src/modules/runtime.rs`, `src/modules/host.js` | `modules/.../ModuleRuntime.kt`, `modules/src/main/assets/module-host.js` | One engine thread per module instead of coroutines; promises, timers and requests settle through a Rust event loop (`__moduleSettle` / `__moduleTimerFire`); interrupt handler bound to the call deadline; console output kept in a per-module log; extra `storage` / `utils` / `crypto` helpers; HLS / DASH results rejected (no segmented playback); no extension-compatibility host. |
| `src/modules/http.rs` | `modules/.../ModuleHttpClient.kt`, `app/.../modules/ModuleNetworkPolicy.kt` | Blocking `ureq` with the app proxy; redirects followed manually with credential headers stripped across origins; per-call deadline; no cookie jar. |
| `src/modules/catalog.rs` | `modules/.../ModuleCatalogReader.kt` | Regex link discovery instead of Jsoup; same depth / document / entry limits. |
| `src/modules/registry.rs` | `app/.../modules/ModuleRepository.kt` (encode/decode), `modules/.../ModuleRuntimeStorage.kt` | JSON registry file (0600) instead of AtomicFile + Keystore encryption; secret settings go to a `SecretHook` (the app's keychain) and are never written to the registry; runtime storage is a plain 0600 JSON file. No portable backups yet. |
| `src/modules/matcher.rs` | `app/.../modules/ModuleTrackMatcher.kt`, `app/.../utils/FuzzyMatcher.kt` | Trailing-feature lookahead regex rewritten as a scan (Rust `regex` has no lookaround); NFKD via `icu_normalizer`. |
| `src/modules/mod.rs` | `app/.../modules/ModuleRepository.kt`, `ModulePlaybackUseCases.kt` (`ResolveModuleAudioUseCase`, diagnostics), `ModuleSettingsUseCases.kt` | Blocking API with deadlines instead of coroutines/flows; `ModuleResolver` adapts it to `StreamResolver` with a YouTube fallback switch; no local metadata edits or authorization flows. |
| `src/stream.rs` (`Downloader::download_progressive`), `src/voice.rs` (source reporting) | `playback/ModuleMediaSource.kt` (role only) | Module streams are fetched with one progressive GET + request headers instead of googlevideo's 1 MiB ranges; the decoder's codec / rate / bit depth is reported in `PlaybackState::source`. |

Libraries used by the module system (not ported code): `rquickjs` (MIT; bundles QuickJS-ng,
MIT), `url` (MIT OR Apache-2.0), `sha2` (MIT OR Apache-2.0), `icu_normalizer` (Unicode-3.0).

## Full-length YouTube streams (`youtube` module)

| music-core file | Source | Notes |
|---|---|---|
| `js/ejs/lib.min.js`, `js/ejs/core.min.js` | [yt-dlp/ejs](https://github.com/yt-dlp/ejs) 0.8.0 (as shipped in the `yt-dlp-ejs` package) | Unmodified. The Unlicense (public domain), see `js/ejs/LICENSE`; `lib.min.js` bundles meriyah (ISC, © KFlash and others) and astring (MIT, © David Bonnet), whose license texts are kept in its header comment. |
| `assets/po_token.html` | ArchiveTune `app/src/main/assets/po_token.html` | Unmodified BotGuard client page (GPL-3.0, © Rukamori and ArchiveTune contributors). |
| `src/youtube/potoken.rs` | ArchiveTune `app/.../utils/potoken/{BotGuardTokenGenerator.kt, ChallengeParser.kt}` | Same flow and page parsing; blocking `ureq`; the browser part behind the `BotGuardVm` trait; a Safari identity matching the WebKit engine it runs in. |
| `src/youtube/webkit.rs` | ArchiveTune `BotGuardTokenGenerator.kt` (WebView engine) | A hidden WKWebView (`callAsyncJavaScript`) instead of Android's WebView + JS bridge. |
| `src/youtube/resolver.rs`, `session.rs` | yt-dlp `yt_dlp/extractor/youtube/{_base.py,_video.py}` (The Unlicense) | Client definitions (`tv_samsung`, `mweb`), the player request shape, and the "wait for the pre-roll ad" rule (`_get_available_at_timestamp`); reimplemented in Rust. |

## refryt and Limusic (stream resolution robustness)

Ideas and small parts of the stream chain are ported from two GPL-3.0 YouTube Music desktop
clients: **refryt** (<https://github.com/gridoe/refryt>, © refryt contributors, commit
`e9685b0b49ff2fd094d60abc565a57494963755d`) and **Limusic** (<https://github.com/SimoHypers/limusic>, © Limusic
contributors, commit `d82372cdd3016e3c5e28ea8022596b7fefdf681a`). Each ported file carries a header naming its source.

| music-core file | Upstream file(s) | Changes |
|---|---|---|
| `src/youtube/breaker.rs` | refryt `crates/refryt-resolve/src/chain.rs` (`Breaker`, `is_video_specific`) | Same rule (3 failures in a row → skipped 10 min, half-open retry, video-specific failures not counted, the last route never skipped), per `WebClient` instead of per resolver; synchronous; network errors not counted either; a `ResolverStatus` snapshot for the UI. |
| `src/youtube/client.rs` | refryt `crates/refryt-resolve/src/guest.rs` (`VISIONOS` client), Limusic `crates/innertube/clients.json` (`TVHTML5_SIMPLY`) | Client identity strings and the visionOS request body; identities can be replaced by the signed player-fixes manifest. |
| `src/youtube/resolver.rs` (visionOS path, self-heal) | refryt `guest.rs` (visitor adopted from a `LOGIN_REQUIRED` refusal, first-byte probe); Limusic `src-tauri/src/orchestrator.rs` (`self_heal`), `src-tauri/src/cipher/` (refetch `player.js` on failure) | Rewritten for blocking `ureq` and our QuickJS solver: one retry with the player dropped from the solver and session caches. Limusic's community cipher registries are **not** used; the app reads only its own signed manifest (`src/youtube/fixes.rs`, original). |

## ArchiveTune (fix/content: playlist covers)

`crates/music-gpui/src/playlist_cover.rs` ports the cover preparation of ArchiveTune's
`app/src/main/kotlin/moe/rukamori/archivetune/repository/PlaylistCoverRepository.kt`
(`decodeUploadImage`, `compressWithinLimit`: centre-square crop, 1080 px, JPEG quality 92
→ 60 in steps of 8 until ≤ 2 MiB), © Rukamori and ArchiveTune contributors, GPL-3.0,
with the `image` crate instead of Android's `Bitmap`.

## ArchiveTune (feat/home-parity: Home)

Home's sections, their order, data and options are ported from ArchiveTune (© Rukamori and
ArchiveTune contributors, GPL-3.0); each file names its upstream source in its header.

| File | Upstream file(s) | Changes |
|---|---|---|
| `crates/music-core/src/library/home.rs` | `db/DatabaseDao.kt` (`forgottenFavorites`, `mostPlayedSongs`, `recentSongs`, `librarySongIds`) | Forgotten favourites as a `ForgottenRule` (30 days, < 20 % of the earlier listening time, as upstream) that also admits songs not played at all lately and needs 3 earlier plays; recent songs by the time of the last play; no artist blocking. |
| `crates/music-gpui/src/pages/home/model.rs` | `home/HomeUseCases.kt` (`LoadPersonalizedQuickPicksUseCase`, `PrepareCommunityHomePageUseCase`, `QuickPickSeed`), `home/HomeRepository.kt` (`loadQuickPickSeeds`, `loadRemoteQuickPickSeeds`, `validRecommendationsFor`), `viewmodels/HomeViewModel.kt` (`extractQuickPicks`), `ui/screens/HomeScreen.kt` (section order) | Same seed choice, scoring (seed weight 100, position, artist affinity 80), rotation pool and community-shelf matching; 4 seeds instead of 6; a structural fallback for YouTube's quick picks shelf in other languages; the section order as a testable list. |
| `crates/music-gpui/src/pages/home/load.rs` | `viewmodels/HomeViewModel.kt` (`loadInternal`, `loadSimilarRecommendations`, `refreshAccountPlaylistsInternal`), `home/HomeRepository.kt` (`loadRelatedSongs`) | Related songs from the seed's radio (`next`) only (no `related` + search + cached-related fallbacks); 2 artists + 1 song for "Similar to" (upstream 3 + 2); requests on a pool of 3 threads; no playlist previews for the community section. |
| `crates/music-gpui/src/pages/home/mod.rs` | `ui/screens/HomeScreen.kt`, `viewmodels/HomeViewModel.kt` | Shelves, cards and quick-picks grids of this app instead of Compose decks; a refresh button instead of pull-to-refresh; a 20-minute cache per account (refreshed when Home is shown after it expired). |
| `crates/music-gpui/src/pages/home/speed_dial.rs` | `utils/SpeedDialPins.kt`, `viewmodels/HomeViewModel.kt` (`loadSpeedDialItems`) | Pins stored as items in the library's key/value table (24, newest first) instead of a preferences string resolved against the database; a Shuffle button instead of the random tile. |
| `crates/music-gpui/src/settings/home_prefs.rs` | `constants/PreferenceKeys.kt` (`QuickPicks`, `QuickPicksDisplayMode`, `ShowHomeCategoryChipsKey`, `DisableBlurKey`), `home/HomeRepository.kt` | Same defaults; the tonal backdrop is its own switch (skipped while the app-wide artwork look is on); hidden sections added. |

## Accounts (feat/accounts-plus): several accounts, browser sign-in, YouTube history

Ported with attribution in each file's header. refryt and Limusic are GPL-3.0 (the license
of this project); Zuno is Apache-2.0, compatible with GPL-3.0 (it has no NOTICE file; its
license text is at <https://github.com/noFAYZ/zuno/blob/main/LICENSE>).

| File | Upstream | Changes |
|---|---|---|
| `crates/music-core/src/browser_cookies/mod.rs` | [refryt](https://github.com/gridoe/refryt) `crates/refryt/src/browser_cookies.rs` (`KNOWN`, `readable_on`, `cookies_used`, `detected_in`), © gridoe and refryt contributors, GPL-3.0 | The browser table split per OS, with Arc and Safari added; the Windows app-bound check (`app_bound_encrypted_key` in `Local State`) kept but the profile is listed as blocked instead of hidden; profiles listed one by one with their names (`Local State`, `profiles.ini`); the stores are read natively (`chromium.rs`, `firefox.rs`, `safari.rs`) instead of through yt-dlp, and only `youtube.com` rows are queried. |
| `crates/music-core/src/browser_cookies/{chromium,firefox,safari}.rs` | — (own code; the store formats are the ones yt-dlp's `cookies.py` reads, The Unlicense) | Chromium `v10`/`v11` AES-128-CBC (PBKDF2-HMAC-SHA1, `saltysalt`), Windows `v10` AES-256-GCM, `v20` detected and refused, the SHA-256 host prefix of database version 24; Firefox `moz_cookies`; Safari `Cookies.binarycookies`. |
| `crates/music-core/src/innertube/playback_report.rs` | ArchiveTune `innertube/InnerTube.kt` (`registerPlayback`, `ytPlaybackTrackingClient`), `innertube/YouTube.kt` (`registerPlayback`), © Rukamori and ArchiveTune contributors, GPL-3.0; [Zuno](https://github.com/noFAYZ/zuno) `src/datasource/youtube/YouTubeMusicDataSource.ts` (`createPlaybackNonce`, `beginPlayReport`, `updatePlayReport`, `pingPlaybackStats`), © noFAYZ and Zuno contributors, Apache-2.0 | The `player` request body and the ping parameters of both; watch-time ranges (`st`/`et`) of the real played ranges, clamped to the wall time; the cookie is only sent to `*.youtube.com`. |
| `crates/music-core/src/innertube/auth.rs` (`login_key`) | [Limusic](https://github.com/SimoHypers/limusic) `src-tauri/src/db.rs` (`account_key`), © SimoHypers and Limusic contributors, GPL-3.0 | The idea (key a login by its `SAPISID`); SHA-256 instead of MD5, 12 bytes. |
| `crates/music-gpui/src/account_list.rs` | Zuno `src/ui/components/AccountSwitcher.tsx` (`GoogleAccountSwitcher`, `AddGoogleAccountButton`), Apache-2.0; Limusic `src-tauri/src/db.rs` (the `accounts` table, the active account as a projection), `src-tauri/src/session.rs` (`open_login` with `add_account`), GPL-3.0 | Saved accounts in the settings with one keychain entry each; sign-in decides renew / switch / add; ratings owed to YouTube kept per account; one sign-in web view profile per account instead of clearing the web view's cookies. |
| `crates/music-gpui/src/play_report.rs` | ArchiveTune `playback/MusicService.kt` (`registerRemotePlaybackHistory`), GPL-3.0; Zuno `src/player/PlayerController.ts` (`beginPlayReport`, `finishPlayReport`, `syncScrobbleTicker`), Apache-2.0 | Played ranges followed from the 250 ms poll; reported 5 s in, watch time every 30 s and at the end. |
| `crates/music-gpui/src/browser_import.rs` | refryt `crates/refryt/src/browser_cookies.rs` (`detected`, `label_for`, `import`), GPL-3.0 | A dialog of browser profiles; the OS key per Chromium browser (Keychain "Safe Storage", DPAPI, libsecret / KWallet) instead of yt-dlp. |

## Zuno and Limusic (feat/queue-ux)

Ideas and small pieces of logic ported from two other desktop YouTube Music clients; each
file names its source in its header or next to the ported code.

* **Zuno** (<https://github.com/noFAYZ/zuno>, commit
  `1fa9a9669e4a526ebe53c0275153feda0c4fcefa`), © Zuno contributors, Apache License 2.0
  (<https://www.apache.org/licenses/LICENSE-2.0>).
* **Limusic** (<https://github.com/SimoHypers/limusic>, commit
  `d82372cdd3016e3c5e28ea8022596b7fefdf681a`), © Limusic contributors, GPL-3.0.

| File | Upstream | Changes |
|---|---|---|
| `crates/music-core/src/output.rs` (`tidy`) | Zuno `src-tauri/src/audio.rs` (`is_alsa_plugin_noise`, its list of ALSA PCM types) | Same list plus a few more types; instead of dropping `hw:` / `plughw:` outright, one entry per card is kept by rank (`sysdefault` > `hdmi` > `dmix` > `front` > `plughw` > `hw`), so a card with only raw entries stays selectable; friendly names for bare PCMs, de-duplication by id and name on every platform, the default first, a "show all" switch and the picked device always kept. |
| `crates/music-core/src/together/{messages,server,client}.rs` (suggestion approval) | Limusic `crates/listen-protocol/src/lib.rs` (`Suggestion`, `Suggest`, `ApproveSuggestion` / `RejectSuggestion`, `SuggestionReceived` / `SuggestionApproved` / `SuggestionRejected`), `src-tauri/src/listentogether/mod.rs` | Kept within ArchiveTune's protocol: the guest's request is the existing `add_track_request`; the host keeps the waiting list (`TogetherServer::suggestions` / `decide_suggestion`) and answers with one new `suggestion_status` message (pending / approved / rejected) that older peers skip; a room setting, off by default; a cap of 20 waiting suggestions per guest. |
| `crates/music-gpui/src/queue.rs` (your queue vs the context) | Zuno `src/player/Queue.ts` (`manualQueueLength`), `src/ui/components/player/QueuePanel.tsx` (sections) | A source tag per item instead of a count, so shuffle, moves, jumps, history and saving keep the two parts apart; a new list keeps your pending items (as Spotify does); each part can be cleared on its own; the context is named ("Next from …"). |
| `crates/music-gpui/src/queue_tabs.rs` | Zuno `src/player/TabManager.ts` | Only the queue is per tab (one engine; Zuno gives each UI tab its own player and page stack): tabs are saved queues, one of them playing; the others are previewed and "continued" where they were left. |
| `crates/music-gpui/src/selection.rs`, `src/components/selection_bar.rs` | Zuno `src/ui/hooks/useTrackSelection.ts`, `src/ui/components/SelectionBar.tsx` | Same click rules (⌘ / Ctrl toggles, Shift extends from the last row touched, selected songs in list order); one app-wide selection keyed by list id, index and video id; a plain click still plays (Zuno's collapses the selection); ⌘A / Esc; more actions (play, play next, like, dislike, the list's own "Remove"). |
| `crates/music-gpui/src/logs.rs` | Zuno's log viewer (Settings) | Own implementation: stderr through a pipe into a ring buffer and a rotated file, levels guessed from the text, secrets redacted before anything is kept. |

## Freeverb, Limusic, Zuno and Deviload (feat/np-extras)

| File | Upstream | Changes |
|---|---|---|
| `crates/music-core/src/reverb.rs` | Freeverb by Jezar at Dreampoint (2000, public domain): the comb / allpass structure and its tuning constants (delay lengths, stereo spread, gains). The preset idea is [Limusic](https://github.com/SimoHypers/limusic)'s rooms (`crates/player/src/lib.rs`, `reverb_filter`; commit `d82372cdd3016e3c5e28ea8022596b7fefdf681a`, GPL-3.0) | Written for this project, no code copied. Limusic convolves bundled impulse responses with FFmpeg's `afir`; this is an algorithmic reverb on the deck's mix: four presets of our own (room size, damping, width, pre-delay), a pre-delay line, delay lengths scaled to the output rate, smoothed wet / dry gains, denormal flushing, a tail that rings out after a pause and then stops, dropped entirely when off. |
| `crates/music-core/src/equalizer.rs` (`EqPreset::Warm`, `EqPreset::Night`) | [Zuno](https://github.com/noFAYZ/zuno) `src/ui/settings/equalizer.ts` ("Warm", commit `1fa9a9669e4a526ebe53c0275153feda0c4fcefa`, Apache-2.0); [Deviload](https://github.com/Pronexsteam/deviload) `src/devil-music.js` / `src/music-sound.js` ("night", commit `70d1d951147d8882996c544f4061faea9100752f`, © Samvel Avetisyan, MIT) | Warm's ten gains as Zuno has them; Deviload's six-band night curve (60 Hz – 10 kHz) spread over our ten bands. |

## Limusic (feat/music-video): music videos and theater mode

Ideas and data shapes from **Limusic** (<https://github.com/SimoHypers/limusic>, © SimoHypers
and Limusic contributors, GPL-3.0, commit `d82372cdd3016e3c5e28ea8022596b7fefdf681a`). No code
was copied: Limusic plays the video in a `<video>` element or mpv, CrateTune decodes it itself
(`src/video/`, original). Each file names its source in its header.

| File | Upstream file(s) | Changes |
|---|---|---|
| `crates/music-core/src/innertube/pages.rs` (`counterpart`), `models.rs` (`Counterpart`) | `crates/innertube/src/models/metadata.rs` (`panel_rows`, `set_twin`, `Segment`) | The wrapper row's `counterpart` arm and its `segmentMap` read the same way (milliseconds as strings or numbers); kept as a list beside the queue instead of on each row; `Counterpart::video_for` also turns a video-primary pair around. |
| `crates/music-core/src/video/timeline.rs` | `crates/innertube/src/models/metadata.rs` (`counterpart_segments`, issue #535 there) | Limusic maps a Song / Video *switch* onto the other version's time; here the song's position drives the muted video continuously, with gaps under 1.5 s bridged and the artwork shown in longer ones. |
| `crates/music-gpui/src/theater.rs` | `ui/src/lib/components/TheaterMode.svelte` | The idea (picture filling the window, lyric line over it, controls that fade after a few still seconds but not while paused, Esc leaves); written for GPUI, without Limusic's queue / lyrics columns or OS full screen. |
| `crates/music-gpui/src/video.rs` (`session_off`) | `ui/src/lib/video.svelte.ts` (`video.want`) | "Show artwork" lasts for the session; the setting is the lasting off switch. |
