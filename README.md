# TuneDesk — native desktop YouTube Music client

A lightweight Rust desktop client (macOS, Windows, Linux) in the spirit of the
Android app ArchiveTune: a UI-agnostic core (`crates/music-core`) and a GPUI
app (`crates/music-gpui`, see [music-gpui](#music-gpui-gpui--gpui-component)).

**License:** GPL-3.0 (see `LICENSE`), because features are ported from
ArchiveTune (GPL-3.0, © Rukamori and contributors). Code ported from
Hummingbird is Apache-2.0 and listed in `crates/music-core/THIRD_PARTY.md`.
This is an independent project: it is not ArchiveTune and uses none of its
name or branding.

## Install / build

**Users:** download the DMG (macOS), the AppImage / tarball (Linux) or the installer / zip
(Windows) from the releases, see [docs/USER_GUIDE.md](docs/USER_GUIDE.md) (features,
shortcuts, settings, sign-in, limitations) and [CHANGELOG.md](CHANGELOG.md). Testers: see
[docs/TESTING.md](docs/TESTING.md) (Windows build, .exe / installer, manual checklist).

**From source** (Rust 1.99 via `rust-toolchain.toml`; no Xcode needed on macOS — the
Command Line Tools suffice, GPUI's shaders are compiled at runtime):

```sh
cargo build --release -p music-gpui      # → target/release/music-gpui
cargo run --release -p music-gpui
```

Linux needs the development packages GPUI and the audio stack link against (Debian /
Ubuntu names): `build-essential pkg-config clang libasound2-dev libfontconfig-dev
libfreetype-dev libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev libx11-xcb-dev
libxcb1-dev libvulkan-dev libdbus-1-dev` (the list CI installs, `.github/workflows/ci.yml`).
For PoTokens on Linux (optional, untested) add `libwebkit2gtk-4.1-dev` and build with
`--features music-core/webkitgtk-potoken`. The relay server is `cargo build --release -p
music-relay` ([Relay server](#relay-server-music-relay)).

**Packages** (`target/bundle/`):

| Platform | Command | Output |
|---|---|---|
| macOS | `scripts/bundle-macos.sh [--no-build] [--no-dmg]` | `TuneDesk.app` (Info.plist with bundle id, version from Cargo, `LSMinimumSystemVersion` 11.0, `NSMicrophoneUsageDescription`, high-res, music category; ad-hoc signed, or Developer ID + hardened runtime with `MACOS_SIGN_IDENTITY`) and `tunedesk-<ver>-macos-<arch>.dmg` |
| Linux | `scripts/bundle-linux.sh [--no-build] [--appimage]` | `.tar.gz` with `bin/`, `.desktop`, hicolor icons, AppStream metainfo, `install.sh`; AppImage via `appimagetool` |
| Windows | `pwsh scripts/bundle-windows.ps1` | `.zip` and an Inno Setup 6 per-user installer (`packaging/windows/installer.iss`); the `.exe` gets its icon + version resource from `build.rs` (`embed-resource`, native Windows builds). An MSI could be made with `cargo-wix` instead. |

CI (`.github/workflows/ci.yml`) runs fmt, clippy (also with `--features screenshot`) and
tests on macOS, Ubuntu and Windows; `release.yml` builds every package on a `v*` tag and
attaches them to a draft release.

**App identity.** The app is **TuneDesk** (app id `tunedesk`; the bundle id
`io.github.tunedesk` is a placeholder). Name and ids live in **one file, `packaging/app.env`** (`APP_NAME`, `APP_ID`,
`BUNDLE_ID`, `APP_SUMMARY`, `UPDATE_REPO`), read by `crates/music-gpui/build.rs`
(→ `identity.rs`: window title, menus, tray, About, media-controls / MPRIS name, settings
directory and keychain service) and by the packaging scripts. Renaming `APP_ID` moves the
settings directory and keychain entries (move them by hand to keep them). The app icon is
original artwork: `packaging/icons/app-icon.svg`, rendered by `scripts/make-icons.sh`
(rsvg-convert, iconutil, ImageMagick) into the committed `packaging/macos/AppIcon.icns`,
`packaging/windows/app.ico` and `packaging/linux/icons/*.png`.

## Decision: GPUI + gpui-component

The UI framework was picked by building the same spikes in three stacks. `main`
now carries the winner; the other branches are kept for reference.

| Branch | UI stack | Outcome |
|---|---|---|
| `main` (= `ui/gpui-component`) | GPUI (`gpui-pre` 0.3.10) + gpui-component 0.8.1 | **Chosen** |
| `ui/slint` | Slint 1.18 (FemtoVG) | Built everything incl. karaoke lyrics, but word layout relies on Slint's internal text engine (Parley) and the accessibility tree is rebuilt every animated frame |
| `ui/hummingbird` | Fork of Hummingbird (GPUI, `gpui-unofficial`) | Works by converting YouTube tracks to tagged local files; 38.5 MB binary, fork upkeep. Used as a donor instead (EQ, media controls, scrobbling ported into music-core) |

Deciding test — Now Playing with word-by-word karaoke lyrics (release, macOS arm64, 60 Hz):

| | Slint | GPUI |
|---|---|---|
| CPU while lyrics animate | 79 % avg (19 % with accessibility compiled out) | 17 % avg |
| Frame rate | 53–61 fps (steady 60 without accessibility) | ~61 fps, worst gap 17.8 ms |
| RSS while animating | ~139 MB | ~104 MB |
| Idle, screen closed | 0 % | ~0 % |
| Binary | 17.7 MB | 21.8 MB |

Accepted risks of GPUI: a pinned pre-release snapshot (Zed deprioritises
non-Zed users), thin accessibility, no transforms/blur/masks on arbitrary
elements (worked around with custom painting and `anim.rs`). The core stays
UI-agnostic so the UI could be swapped again.

## Shared core (`crates/music-core`)

Blocking, thread-safe, no async runtime, so every UI toolkit can drive it from
its own executor.

* `InnerTube` — YouTube Music (`WEB_REMIX`) metadata: home (+ mood chips, continuation),
  explore, charts (per country), moods & genres, new releases, album / artist / playlist
  pages (long albums: `album_tracks` / `album_complete` load past the page's 200 tracks
  through the album playlist; `playlist_complete`), podcasts (`podcast` show pages with
  `Episode`s, `podcast_continuation`, `episode` pages), search (summary, per-category
  filters, suggestions, continuations, spelling `correction` + `search_summary_exact`),
  play queues (`next`, `RDAMVM<id>` radio), related and lyrics tabs; locale via `hl`/`gl`.
  Every parsed `Track` carries `music_video_type` (`MusicVideoType::{Atv, Omv, Ugc,
  PodcastEpisode, Uploaded, Other}`; `Track::is_video` = OMV or UGC). A cookie-based
  account layer (`innertube::Auth`, SAPISIDHASH) adds account info, library (incl.
  podcasts), history, likes, subscriptions, playlist editing and custom playlist covers
  (`upload_playlist_cover`; request bodies unit-tested; not yet verified live). Ported from
  ArchiveTune, see `THIRD_PARTY.md`.
* `http::configure(HttpConfig { proxy, user_agent, timeout, doh })` — process-wide HTTP agent
  (http/https/socks4/socks5 proxies) with optional DNS-over-HTTPS (`doh` module: RFC 8484
  wire format over ureq's resolver hook, `DohProvider::{Cloudflare, Google, Quad9,
  Custom(url)}` reached through bootstrap IPs, A + AAAA in parallel, TTL cache 30 s – 1 h,
  `fallback_to_system`); `http::lookup(host)` reports how a name resolves. The modules'
  agent shares the resolver. ureq speaks HTTP/1.1 only, so Quad9 uses its port-5053 endpoint.
* `StreamResolver` trait + `IosClientResolver` — video id → audio URL; `Quality`
  (`Low`/`Normal`/`High`) picks among the itags it returns (AAC 139/140, Opus 249/250/251).
* `Player` — audio thread with a pollable `PlaybackState`, `pause`/`resume`/`toggle_pause`/`seek`.
  Decoding is Symphonia 0.6 plus the pure-Rust `opus-decoder` (`TrackDecoder`, a seekable
  `rodio::Source`): MP4/M4A (AAC, ALAC), MP3, FLAC, Ogg Vorbis/Opus, WAV, AIFF, WebM/MKV.
  rodio/cpal only mix and output. Gapless details the libraries leave out:
  * AAC in MP4 (files, YouTube's fragmented DASH `.m4a`): the encoder delay ("priming",
    ~1–2k frames) and the last frame's padding are trimmed from the edit list
    (`moov/trak/edts/elst`; iTunes' `iTunSMPB` when there is none) by a small box parser
    (`mp4_gapless`), since Symphonia ignores edit lists. Durations, positions and seeks
    use the trimmed timeline; AAC seeks pre-roll two frames, so audio after a seek is
    identical to sequential decoding. Two halves of one signal play back without a gap
    (`player::tests::gapless_aac_tracks_join_without_priming_or_padding`).
  * Opus: mono / stereo (mapping family 0, everything YouTube serves) and multichannel
    (family 1 up to 7.1, family 255): multistream packets are split per RFC 6716 App. B
    (`opus-decoder` 0.1's own multistream splitter misreads them) and mixed down to
    stereo (ITU-style matrix normalized like opusfile's; family 255 keeps its first two
    channels). Ambisonics (families 2 / 3) are rejected.

  Engine features:
  * plays `Track::local_path` files (ReplayGain / R128 track gain from tags) or streams
    YouTube progressively (`StreamBuffer`: playback starts with the first bytes, seeks
    into the undownloaded part wait up to 10 s). Expiring URLs (googlevideo's `expire`,
    a module's `expiresAt` → `ResolvedStream::valid_until`) are re-resolved through
    `StreamResolver::refresh_track` 30 s before they expire (also before first use, e.g.
    a preloaded next track) and when rejected (401/403/410), and the download resumes at
    the same byte — only if it is provably the same file (format, size, module + variant;
    a server-reported size change fails instead of splicing two files); up to 3 times per
    download, each after progress. Downloads do the same (`download_refreshing`);
    loudness normalization from YouTube's `loudnessDb` (`set_normalization(false)` turns
    it off, ReplayGain included);
  * `set_next` → preload ~30 s ahead, gapless switch, optional crossfade
    (`set_crossfade`, ≤ 12 s, also before a preview's early end); engine-initiated
    switches bump `PlaybackState::advance_count`;
  * `set_skip_silence`, `set_speed` (0.5–2×) and `set_pitch` (±12 semitones, Sonic),
    `set_sleep_timer` (duration or end of track), `set_auto_skip` (errors →
    `error_count`/`failed_track_id`), `output_devices` / `set_output_device` (falls back
    to the default and back when a device disappears/reappears);
  * `Player::headless` renders without a device (tests use it end to end).
* `equalizer` — parametric EQ (port of Hummingbird's), applied live:
  `player.set_equalizer(EqPreset::Bass.settings())`, `player.clear_equalizer()`.
* `spectrum` — real-time spectrum for visualizers. The deck taps its final mix (after EQ,
  normalization, crossfades and the pause ramp; *before* the volume, so muted playback still
  moves) into a fixed lock-free ring of mono samples (relaxed atomic stores, no allocation or
  lock in the audio callback; skipped entirely while off). On demand — from the UI thread at
  ≤ 60 Hz — `player.spectrum(bands)` / `spectrum_into(bands, &mut Spectrum)` (no per-frame
  allocation) runs a Hann-windowed 2048-point FFT (`rustfft`) over the newest samples and
  returns `Spectrum { bands, peaks, level }`, all 0..1: log-spaced bands (1–256, default
  range 40 Hz–16 kHz, `SpectrumConfig::band_edges`), dB mapped from −70..−12 dBFS with a
  +3 dB/octave tilt around 1 kHz, fast attack (15 ms) / slower decay (300 ms), peak hold
  (600 ms, then 1.2/s), and the RMS level (−60..0 dBFS). Paused, stopped, silent or not
  pulled for 150 ms, it decays to exact zeros (`Spectrum::is_silent` → stop redrawing).
  `set_spectrum_enabled(true)` while a visualizer is visible; `spectrum_analyzer()` gives
  each view its own band count and smoothing (`set_config`). Cost ≈ 11 µs per call (64 bands) in
  release on an M-series Mac (`spectrum::tests::analyze_is_cheap`). Low bands narrower than
  the 23 Hz bin spacing (below ~200 Hz with 32+ bands) share the window's main lobe and
  read alike.
* `media_controls` — macOS Now Playing / media keys, Windows SMTC, Linux MPRIS
  (feature `media-controls`, default on).
* `scrobble` — Last.fm + ListenBrainz (`ScrobbleTracker` applies `ScrobbleRules` — by
  default Last.fm's 50 % / 4 min / > 30 s — to `PlaybackState` snapshots; `Scrobbler`
  sends on a worker thread; `set_rules` changes them live).
* `discord` — Discord Rich Presence (feature `discord`, default on). `DiscordConfig`
  customizes the activity: type (listening / playing / watching / competing), name,
  details / state / image-text templates with `{title}` `{artist}` `{album}`, large and
  small image (artwork, asset key / URL, none), up to two buttons (the song on YouTube
  Music or a custom URL), show while paused; `DiscordPresence::set_config` applies it live.
* `lyrics` — lyrics stack ported from ArchiveTune (details below); `fetch_lyrics` stays the
  simple LRCLIB-only call.
* `library` — SQLite library (`rusqlite`, bundled): likes, library, artists/albums, playlists
  (local + YouTube mirrors), play history, listening stats and "year in music", search
  history, lyrics cache, saved queue, zip backup/restore. `Library` is `Clone + Send + Sync`.
* `downloads` — `DownloadManager`: user downloads (never evicted) and an LRU play cache, two
  workers on the `StreamResolver`, pollable `states()`.
* `modules` — `.arch` provider modules (details [below](#provider-modules-arch)): JavaScript
  providers (QuickJS via `rquickjs`, sandboxed) that supply alternative audio for a song;
  `ModuleResolver` (a `StreamResolver`) tries the enabled modules before YouTube. Module streams
  download progressively with their request headers (any size, no 1 MiB range chunks), also
  for downloads. `StreamResolver::resolve_track` resolves a whole `Track` (default: its video id);
  `PlaybackState::source` says where the audio comes from and what it is ("FLAC 16/44.1").
* `local_files` — scans music folders with `lofty` (tags, cover art, lyrics, ReplayGain) into
  the library as `local:<hash>` tracks, incrementally (`scan_with_progress` reports
  `(done, total)` per file).
* `sponsorblock` — SponsorBlock segments through the privacy-preserving hash-prefix API
  (`/api/skipSegments/<first 4 hex of sha256(id)>`), category filter + merge, per-video LRU,
  configurable server, `SkipTracker` (skip each segment once per pass).
* `playlist_import` — Exportify CSV / our CSV, M3U, JSON readers and writers; `matcher`
  (ArchiveTune's FuzzyMatcher: Jaro–Winkler title / artist, recording markers, duration)
  and a threaded `resolve` (direct video ids, up to two InnerTube searches per song, progress,
  cancel).
* `spotify` — Spotify Web API with the user's own Client ID: Authorization Code + PKCE with
  a loopback redirect, playlists, liked songs, playlist items (read-only scopes).
* `together` — Music Together on the LAN or through a self-hosted relay: ArchiveTune's JSON
  protocol over WebSockets (`tungstenite` on std threads, `wss://` via rustls),
  `TogetherServer` (`start` on a port, `start_relay` through a `music-relay` server) /
  `TogetherClient`, heartbeat clock sync, `GuestSync` (drift / play-state planner), join
  links, host transfer (`transfer_host`: the guest starts its own server with the room's
  state, everyone reconnects). See [Relay server](#relay-server-music-relay).
* `cast` — Chromecast / Google Cast: mDNS discovery (`discover`, `_googlecast._tcp`),
  `CastSession` (CASTV2 over TLS: Default Media Receiver, LOAD / PLAY / PAUSE / SEEK /
  STOP / volume, status), `MediaProxy` (serves a `ResolvedStream` or a local file to the
  receiver over HTTP with ranges).
* `ai_filter` — AI content filter: classifies songs / albums into categories the user
  hides with any OpenAI-compatible chat-completions API (user-supplied endpoint / key /
  model), verdicts cached per id (memory + JSON file), cache-only `hidden(id)`.
* `recognition` — Shazam-compatible song recognition ported from ArchiveTune's `shazamkit`:
  `capture::record_microphone(len, &cancel, |progress| ..)` (cpal input, any rate → 16 kHz
  mono, cancellable, level callback), `audio::decode_file` / `to_signature_pcm`,
  `signature_from_pcm` / `SignatureGenerator` (2048-point FFT every 128 samples, spread
  peaks in four bands, Shazam's binary format as a `data:` URI; bit-identical to the
  Kotlin generator on a fixture, see `tests/fixtures/recognition/ReferenceSignature.java`),
  `ShazamClient::recognize` → `Recognition` (title, artist, album, art, genre, label, release,
  ISRC, Shazam / Apple Music / Spotify links, YouTube id), and the history on `Library`
  (`recognition_history`, `add_recognition`, `remove_recognition`,
  `clear_recognition_history`; JSON in the key/value table, 50 entries).

```sh
cargo run --release -p music-core --example library -- ~/Music   # scan a folder, print stats
```

Media controls, scrobbling and Discord all hang off the poll the UI already does:

```rust
// once, on the UI thread after the event loop/main window exists
// (macOS: main thread; Windows: set `config.hwnd` to the main window's HWND)
let mut controls = MediaControls::for_player(
    MediaControlsConfig::new("tunedesk", "TuneDesk"),
    player.clone(),
    |event| { /* MediaControlEvent::Next / Previous → UI queue */ },
)?;
let mut scrobbler = Scrobbler::new(vec![
    Box::new(LastFm::new(api_key, api_secret).with_session(session_key)),
    Box::new(ListenBrainz::new(user_token)),
]);
let mut discord = DiscordPresence::new(DiscordConfig::new(discord_app_id));

// every ~250 ms tick
let state = player.state();
controls.update(&state);   // only talks to the OS when something changed
scrobbler.update(&state);  // never blocks; requests run on a worker thread
discord.update(&state);
```

Credentials are always supplied at runtime; `examples/scrobble.rs` runs the Last.fm
auth flow (`get_token` → `auth_url` → `get_session`). Threading details are in the
`media_controls` module docs. Ported code is listed in
[`crates/music-core/THIRD_PARTY.md`](crates/music-core/THIRD_PARTY.md) (Apache-2.0).

```sh
cargo run --release -p music-core --example resolve -- "daft punk"  # headless: search → resolve → stream + decode → seek to 30 s (MUSIC_QUALITY=high: Opus)
cargo run --release -p music-core --example devices                 # list output devices (+ a file path: muted real-device playback check)
cargo run --release -p music-core --example cli -- "daft punk"      # plays ~8 s through the speakers (seeks, Bass EQ)
cargo run --release -p music-core --example browse -- "daft punk"   # every anonymous InnerTube endpoint, live
cargo run -p music-core --example recognize -- --youtube "a-ha take on me"  # decode 12 s of a stream → signature → Shazam
cargo run -p music-core --example recognize -- song.flac 30         # a local file from 30 s (`--mic 8`: the microphone)
```

### Lyrics (`music_core::lyrics`)

Providers (ported from ArchiveTune, default priority order): BetterLyrics (Apple Music TTML,
word-synced with background vocals; uncached songs need an `X-API-Key`), BetterLyrics Portato
(QQ Music QRC), YouLyPlus, Binimum (lrc.red TTML), LRCLIB, KuGou, Megalobiz, SimpMusic (by
video id), Unison, Paxsenix Apple Music / Spotify / Musixmatch (API key required, skipped
without one) and YouTube Music's own plain-text lyrics (InnerTube `next` → lyrics `browse`).

```rust
use music_core::lyrics::{LyricsQuery, LyricsService, LyricsServiceConfig, MemoryLyricsCache};
use music_core::lyrics::romanize::{Script, romanize_result};
use music_core::lyrics::translate::{GoogleTranslator, LyricsTranslator};

let service = LyricsService::new(LyricsServiceConfig::default())   // order, disabled, preferred,
    .with_cache(std::sync::Arc::new(MemoryLyricsCache::new()));     // timeouts, keys, …
if let Some(result) = service.fetch(&LyricsQuery::from_track(&track))? {
    let lyrics = result.synced.as_ref();          // Option<Lyrics>: what the renderer draws
    let extras = &result.extras;                  // per line: agent, background vocals, translation, romanization
    let romaji = romanize_result(&result, Script::Auto);
    let english = GoogleTranslator::default().translate_lyrics(lyrics.unwrap(), "en")?;
}
```

* `LyricsResult` keeps the raw payload (`to_raw()` / `RawLyrics::parse`) so a library database
  can implement `LyricsCache` and store what was fetched.
* Credit lines (`lyrics/credits.rs`, one heuristic for TTML, QRC, LRC and plain text): the
  leading (and a short trailing) block is checked for credit labels in English, Romance
  languages, Chinese, Japanese and Korean (`Written by:`, `Keyboard by：`, `作词 :`, `작곡 :`,
  `Letra:` …), "Written by X" lines, songwriter lists that name one of the track's artists
  (`Julian Casablancas/Thomas Bangalter/…`), a `Title - Artist` header, copyright notices.
  Weak signals (an ambiguous `Music:` label, a plain list of names, a lone Chinese name) only
  count with support: the line spans 0 s, its words are evenly timed (synthesized by the
  provider), or the line next to it is a credit. Names are compared transliterated
  (`ROSALÍA` = `Rosalia`, `周杰倫` = `周杰伦`). `parse` uses the payload's own `[ti:]`/`[ar:]`;
  `LyricsResult::without_credits(&query)` adds the track's title and artists (the service
  applies it to every result and cache hit). Lines in the middle of a song are never touched.
* "Instrumental" answers (`纯音乐，请欣赏`, `Instrumental`, `♪`) are not lyrics
  (`LyricsResult::is_instrumental`); one such answer vetoes line-synced / plain results from
  other providers (for an instrumental they are another song or a fan-written poem), while
  word-synced lyrics still win.
* Selection: providers run concurrently (`max_concurrent`, 1 = one after another); with
  `prefer_word_synced` a word-synced result beats a line-synced one from a higher-priority
  provider; otherwise the highest-priority synced result wins; plain text is the fallback.
* Translation: `GoogleTranslator` (free endpoint, batched; requests of one translation are
  spaced 400 ms and capped at 12) or `AiTranslator` (any OpenAI-compatible chat-completions
  endpoint; key, model and custom prompt from the caller). A 429 / 503 blocks that endpoint
  for its `Retry-After`, else 2 min doubling up to 30 min; calls meanwhile fail at once with
  "… rate-limited this connection; try again in N min" (`translate::is_rate_limited`,
  `LyricsTranslator::retry_after`); a `Retry-After` ≤ 10 s is waited out once. Google answers
  a burst with a redirect to its "sorry" page (a 429 without `Retry-After`).
  `CachedTranslator` + `TranslationCache` (`MemoryTranslationCache`, or `Library`: rows in the
  lyrics table) keep finished translations per translator settings, language and lyrics text.
* Romanization: Japanese (kana table; kanji readings with lindera + IPADIC), Korean (Revised
  Romanization), Chinese (pinyin with tones), Hindi/Devanagari, Cyrillic, other scripts via
  `deunicode`. Kanji: `romanize::dictionary` downloads lindera's prebuilt IPADIC on demand
  (`DictionarySource::ipadic()`: 10.5 MB zip from lindera's v6.2.0 release, SHA-256 pinned,
  45 MB unpacked — UniDic would be 46 MB zipped), unpacks it into a cache directory and
  `load`s it memory-mapped (~9 ms); without it only kana are romanized. The off-by-default
  `japanese-dict` feature embeds the same dictionary instead (+~48 MB binary, downloaded at
  build time). lindera without a dictionary adds ~0.23 MB to the release binary. The pinned
  zip must match the lindera version in `Cargo.toml` (its dictionary format); update both
  together.

```rust
use music_core::lyrics::romanize::dictionary::{self, DictionarySource};
let source = DictionarySource::ipadic();
let path = match dictionary::installed(&source, &cache_dir) {
    Some(path) => path,
    None => dictionary::install(&source, &cache_dir, &cancel, |done, total| { /* progress */ })?,
};
dictionary::load(&path)?;   // romanize_line / romanize_result read kanji from now on
```

```sh
cargo run -p music-core --example lyrics -- "daft punk instant crush"     # every provider, then the service pick
cargo run -p music-core --example lyrics -- --translate en "yoasobi idol" # + romanization and translation
cargo run -p music-core --example lyrics -- --ja-dict /tmp/ja "yoasobi idol" # download IPADIC: kanji readings
cargo run -p music-core --example lyrics -- --raw /tmp/raw "rosalia malamente" # + every provider's raw payload
```

### Full-length YouTube streams (`youtube` module)

`youtube::WebResolver` gets complete streams; `IosClientResolver` (the first 1 MiB only,
`PlaybackState::preview_only`) is kept as the last fallback. What googlevideo requires
(verified Oct 2026, with controls for each point):

* **TV client** (`WebClient::Tv`, Samsung Tizen `TVHTML5`): the player request carries the
  visitor id from a watch page (`X-Goog-Visitor-Id`) and the player script's
  `signatureTimestamp`; the URL's `n` parameter and signature are transformed with that
  player's own functions. No PoToken.
* **Mobile web client** (`WebClient::MobileWeb`): the same, plus a **PoToken bound to the
  video id**, both in the player request (`serviceIntegrityDimensions.poToken`) and as
  `pot=` on the stream URL. Without it: 403.
* **Both:** googlevideo answers 403 until the pre-roll ad could have been skipped (5–15 s,
  from `adPlacements` / `adSlots`). `ResolvedStream::not_before` carries that time and the
  downloader waits; meanwhile it plays the head of the file from the iOS client's URL when
  that is provably the same file (`ResolvedStream::prefix`: same itag, size and `lmt`), so
  playback starts long before that (see first audio below).

Pieces:

* `youtube::jsc` — the JavaScript challenges solved by yt-dlp's solver
  ([yt-dlp/ejs](https://github.com/yt-dlp/ejs) 0.8.0, public domain; bundles meriyah (ISC)
  and astring (MIT); vendored in `crates/music-core/js/ejs/`) in the embedded QuickJS. The
  player script is preprocessed once (a few seconds) and cached per player id in
  `<config>/cache/youtube/`.
* `youtube::potoken` — BotGuard attestation, ported from ArchiveTune: the challenge from
  youtube.com's page, the interpreter, `GenerateIT`, then a minter that makes per-video
  tokens (cached; the integrity token lasts ~12 h). The browser part is the `BotGuardVm`
  trait: `WebKitBotGuard` runs `assets/po_token.html` in a hidden WKWebView on macOS (work
  is dispatched to the main thread, so never call the resolver from it). On **Windows** a
  hidden WebView2 on its own thread (feature `webview2-potoken`, default; the WebView2
  runtime ships with Windows 10/11, the loader is linked statically with MSVC), on
  **Linux** WebKitGTK on its own GTK thread (feature `webkitgtk-potoken`, **off by
  default**: build with `--features music-core/webkitgtk-potoken` and
  `libwebkit2gtk-4.1-dev` ≥ 2.40). Both run the same page and calls (`webview_host.rs`) and
  fetch the challenge with their engine's User-Agent (`BotGuardVm::user_agent`). **They are
  untested**: written and type-checked / clippy-clean for `x86_64-pc-windows-gnu` and
  `x86_64-unknown-linux-gnu` from macOS only. Without a provider the TV client is used.
* `WebResolver::prewarm()` does the slow one-time work at launch; the app calls it.
* **First audio** (time from `play` to audible output) after `prewarm`: one player request
  plus ~70 ms. The iOS client's head starts downloading as soon as that client answers
  (`PrefetchedHead`, in parallel with the web client's player request), so the decoder
  opens on bytes that are already there once the web URL confirms the same file; the
  solver loads the ~4 MB preprocessed player while the player request is in flight
  (`ChallengeSolver::prepare`), so solving `n` takes ~15 ms instead of 200–500 ms; a
  session older than 40 min (TTL 45) is renewed on a background thread. Measured with
  `youtube_play --first-audio` (muted real device, Oct 2026, other builds running):

  | | before | after |
  |---|---|---|
  | TV, 4 songs × 2 searches | 1478 1358 2286 1464 / 1573 1158 2005 1161 ms (mean 1.56 s) | 729 841 841 835 / 626 532 528 526 ms (mean 0.68 s) |
  | Web + PoToken, 3 songs | 2806 2170 1055 ms | 726 728 636 ms |
  | TV, cold (no prewarm), first / second song | 3329 / 1439 ms | 2604 / 838 ms |

* The PoToken attestation is renewed when its integrity token runs out (~12 h, minus
  10 min): `PoTokenProvider::expire_attestation` forces that (unit test without network;
  `youtube_play --first-audio 3 --expire-potoken web` live: ~1 s BotGuard renewal, then
  full streams again). Age-restricted and unavailable videos fail with a clean error after
  the client chain and the fallback (~0.4 s warm), and the player auto-skips them.
* Settings → Playback → **YouTube streams**: Automatic (TV, then web + PoToken, then
  preview), TV client, Web + PoToken, Previews only.

```sh
cargo run --release -p music-core --example youtube -- tv  "daft punk instant crush"  # resolve + full download + decode
cargo run --release -p music-core --example youtube -- web khnokW3Mw24                # same with a PoToken (macOS)
cargo run --release -p music-core --example youtube_play -- tv "daft punk"            # muted real-device playback
cargo run --release -p music-core --example youtube_play -- --first-audio 4 tv "daft punk"  # time to first audio, 4 songs (MUSIC_LOG=debug: timings)
```

This uses YouTube in ways its terms of service don't allow, as ArchiveTune and yt-dlp do,
and can break whenever YouTube changes something (then the preview fallback takes over).

## Spike spec (identical in every UI branch)

1. Window ~1100×720, dark theme.
2. Search field → `InnerTube::search_songs` on a background thread.
3. Virtualized results list: artwork (`Track::artwork_url(120)`), title,
   artists · album, duration. Click a row to play it.
4. Now-playing bar: artwork, title/artists, play/pause, progress bar with
   elapsed/total time and click-to-seek, "preview" badge when `preview_only`.
5. UI polls `Player::state()` every ~250 ms; no other cross-thread plumbing.

## How to compare

Build each branch with `cargo build --release` and record:

* Release binary size (and bundled app size where applicable).
* Idle RSS after launch, RSS with 20 results and artwork loaded, RSS while playing
  (`ps -o rss= -p <pid>` or Activity Monitor "Memory").
* CPU while playing with the window visible vs. hidden.
* Cold start to first frame, clean build time, scroll smoothness.

## music-gpui (GPUI + gpui-component)

The GPUI client (`crates/music-gpui`), picked over Slint after the benchmark rounds below.

```sh
cargo run --release -p music-gpui
# no audio device / sandbox: fake playback from 20 s that "ends" at 80 s like a preview
MUSIC_DEMO_CLOCK=1 cargo run --release -p music-gpui
# dev-only offscreen screenshots (GPUI test-support; separate, slower build)
MUSIC_QUERY="daft punk" MUSIC_GPUI_SCREENSHOT="9:/tmp/search.png,9.5:@settings=eq,11:/tmp/eq.png" \
  cargo run --release -p music-gpui --features screenshot
```

| ![Home](docs/screenshots/shell-home.png) | ![Search](docs/screenshots/shell-search.png) |
|---|---|
| ![Queue panel](docs/screenshots/shell-queue.png) | ![Settings](docs/screenshots/shell-settings.png) |
| ![Dynamic accent](docs/screenshots/shell-dynamic-accent.png) | ![Pure black](docs/screenshots/shell-pure-black.png) |
| ![Equalizer section](docs/screenshots/shell-settings-eq.png) | ![Mini player](docs/screenshots/shell-mini-player.png) |

Browse & search (live YouTube Music data, `screenshot` build, 1180 pt wide window):

| ![Home feed](docs/screenshots/browse-home.png) | ![Search suggestions](docs/screenshots/browse-suggestions.png) |
|---|---|
| ![Search: all](docs/screenshots/browse-search.png) | ![Search: songs tab](docs/screenshots/browse-search-songs.png) |
| ![Artist](docs/screenshots/browse-artist.png) | ![Artist shelves](docs/screenshots/browse-artist-shelves.png) |
| ![Artist items](docs/screenshots/browse-artist-items.png) | ![Album](docs/screenshots/browse-album.png) |
| ![Playlist](docs/screenshots/browse-playlist.png) | ![Radio queue](docs/screenshots/browse-radio-queue.png) |
| ![Explore](docs/screenshots/browse-explore.png) | ![Charts](docs/screenshots/browse-charts.png) |
| ![Moods & genres](docs/screenshots/browse-moods.png) | ![Mood category](docs/screenshots/browse-category.png) |
| ![New releases](docs/screenshots/browse-new-releases.png) | ![Error with retry](docs/screenshots/browse-proxy-error.png) |
| ![Content & region](docs/screenshots/browse-settings-content.png) | ![Network](docs/screenshots/browse-settings-network.png) |

### Structure

```
src/
  main.rs            app init, window (restored frame), mod list
  app.rs             MusicApp: root layout (sidebar | header + page | queue panel, bar,
                     Now Playing overlay), navigation, search field, settings/theme glue,
                     update_app / read_app (how pages + components reach the app)
  playback.rs        play / queue edits / transport / volume / 250 ms poll / media keys
  actions.rs         shortcuts + macOS menu bar (global action handlers)
  router.rs          Route enum + Router (back/forward history, per-entry scroll offsets)
  pages/             one file per page; mod.rs: PageView, NAV (sidebar), build()
  components/        track_row (hover actions + context menu), card, shelf/grid/section
                     header/empty state (mod.rs), items (InnerTube items: card / row /
                     menu / shelf / quick picks / chips / header / loading + retry),
                     artwork (shared cache), sidebar, player_bar (+ progress.rs: the
                     progress / volume track element, bar_fx.rs: its animation state and
                     the play / pause background, np_visualizer.rs), queue_panel
  browse.rs          online pages: Load state, fetch (worker thread, app's InnerTube),
                     PageCache, Paging (infinite scroll), content filters, locale
  search_box.rs      header search: suggestions + recent searches dropdown;
                     search_history.rs: ordered, flushable history writes
  playlist_cover.rs  "Change cover…": image → square JPEG ≤ 2 MiB → upload
  radio.rs           radio / autoplay / play-an-album-or-playlist (InnerTube `next`)
  settings/          file.rs (settings.json model), panel.rs (SettingsPanel entity: state,
                     persistence, secrets, section list), one file per section, mod.rs
                     (SectionId + SECTIONS registry), widgets.rs
  queue.rs           queue model (pure, tested)
  theme.rs           dark / pure black / dynamic, Customize-theme tokens (Look → Resolved →
                     gpui-component Theme + AppColors / AppPalette), radius / density metrics;
                     palette.rs (Oklab, tones, quantizer, scorer, schemes), theme_state.rs
                     (applied look, artwork colours, 400 ms cross-fade), theme_script.rs
  settings/theme_prefs.rs, theme_ui.rs   Customize theme model + group
  settings/portable.rs, portable_ui.rs   settings / theme export-import, presets, Backup & sync
  settings/profiles.rs, profiles_ui.rs   appearance / behaviour profiles
  components/download_header.rs          "Download all" header (HeaderDownloadState)
  mini_player.rs     always-on-top mini player window
  tray.rs            menu-bar item (macOS)
  now_playing.rs     Now Playing: screen, header, backgrounds, transport, the first three
                     designs, lyrics pane; np_designs.rs (the other five designs),
                     np_lyrics.rs (synced lyrics list: own layout, frame pacing, card
                     selection), np_region.rs / snapshot.rs (cached sub-views)
  card_render.rs     lyrics card image (tiny-skia + resvg, pure Rust); lyrics_card.rs its
                     dialog (preview, options, save / copy)
  lyrics.rs          LyricsService wrapper: fetch, prepare (duets, background vocals,
                     romanization), translation cache, cache hook
  karaoke.rs, anim.rs, backdrop.rs (blur, palette, glow lights), clock.rs (engine +
                     demo clock + smooth position)
  settings/prefs.rs  audio / lyrics / Now Playing / Discord activity / scrobble-rule
                     model; settings/prefs_ui.rs: their panel widgets + methods
  module_service.rs  provider modules: the shared `Modules` store, the module-first resolver
                     (player + downloads), keychain hook for secret module settings;
                     settings/modules.rs: the Modules section
  integrations/      SponsorBlock, Music Together (+ relay, host transfer), import / export,
                     AI content filter (ai_filter.rs), Chromecast (cast.rs: Engine::Cast)
                     state + script actions (config.rs: their settings;
                     settings/integrations_ui.rs: sections; pages/together.rs, pages/import.rs)
  services.rs, secrets.rs, script.rs, util.rs, icons.rs
  recognize.rs       recognition session (Recognizer global entity: permission → capture →
                     signature → request → history → YouTube Music search);
                     pages/recognition.rs renders it; mic_permission.rs (macOS AVFoundation)
  settings/onboarding.rs  first-run guide overlay (reuses the settings sections)
  update_check.rs    optional GitHub release check; identity.rs (name / ids from
                     packaging/app.env via build.rs); release_script.rs (script actions)
  i18n/              translations: t! / tp! / k! (mod.rs), CLDR plurals (plural.rs),
                     numbers / dates / relative times (format.rs), system language
                     (system.rs), live input placeholders (inputs.rs), catalog tests and
                     the English-literal lint (tests.rs); settings/language.rs: the picker
locales/<locale>/*.lang  the catalogs (English = source), embedded by build.rs via
                     i18n_parse.rs
```

### Extension points

* **Add a page:** add a `Route` variant (`router.rs`: `title`, `parse`), create
  `pages/<name>.rs` with a struct implementing `Render` and a `build(..) -> PageView`
  (pass the page's main `ScrollHandle` so back/forward restores the scroll position),
  add one arm to `pages::build`, and optionally one `NavItem` line to `pages::NAV`.
  Pages call into the app with `update_app(cx, |app, cx| app.play_list(..))`,
  `app.navigate(route, window, cx)`, `app.queue_add(..)`, and read shared state with
  `read_app(cx, |app, _| ..)`. Build UI from `components::{TrackRow, Card, shelf, grid,
  section_header, empty_state, page_scroll}`; images go through `components::artwork`.
  Online pages fetch with `browse::fetch(cx, |client, filter| .., |page, result, cx| ..)`
  (blocking InnerTube call on a worker thread with the configured locale), keep a
  `browse::Load<T>`, store their data in `browse::PageCache` (so back/forward and revisits
  don't refetch) and render `components::items::{shelf_view, item_card, ItemRow,
  load_state, more_footer}`.
* **Add a settings section:** add a `SectionId` variant (+ `key`), create
  `settings/<name>.rs` with `pub(super) fn render(panel, window, cx) -> AnyElement`, and
  register it in `settings::SECTIONS`. Persisted values go into `settings/file.rs`
  (every field has a default); change them through a `SettingsPanel` method that calls
  `changed()` and emits a `SettingsEvent` if the app must react.
* **Search history:** `MusicApp::record_search(query)` is called for every submitted
  search (in-memory list of 30 feeding the "Recent searches" dropdown) and also saves it in
  the library (`Library::add_search_query`, unless Privacy → "Pause search history");
  `set_recent_searches(..)` is seeded from the library at launch, `remove_recent_search`
  deletes one from both. Database writes go through one ordered worker
  (`search_history.rs`), flushed on quit, before reloading and before clearing; entries
  differing only in letter case are one entry in both lists. `Track::artist_refs` / `album_id` drive "Go to artist / album".
* **Persistent lyrics cache:** `MusicApp::set_lyrics_cache(Arc<dyn LyricsCache>)` swaps
  the in-memory cache (e.g. for the library database's lyrics table); the service is
  rebuilt around it and every later fetch reads / writes it.
* **What the engine plays:** `MusicApp::engine_track(track)` is applied both to `play`
  and to the preloaded next track (`sync_engine_next`); it swaps in a downloaded copy's
  `local_path` (`MusicApp::playable`, `store.rs`).
* **Library services** (`store.rs`, see [Library, account, downloads](#library-account-downloads-local-files)):
  `app.library()` (the SQLite `Library`, `None` if it couldn't open), `app.store.downloads`,
  `app.account_client()` / `app.youtube_auth()` (signing in also puts the account on
  `app.innertube` and the pages' `BrowseClient`, so browse pages are fetched signed-in),
  `toggle_like` / `set_liked`, `add_tracks_to_playlist` (dialog) / `append_to_playlist` /
  `create_playlist`, `download_tracks` / `delete_download`, `set_subscribed` (artist page
  button). Pages load library data with `pages::loader::Loaded` (re-queried when
  `store.revision` moves) and list songs with `pages::song_list::SongList` (virtualized,
  multi-select + action bar). The library database also backs the lyrics cache.

* **Recognition:** `crate::recognize::Recognizer::global(cx)` is the session (`listen`,
  `recognize_file(path)`, `cancel`, `show(entry)`, `phase`, `history`); `app.recognize_song`
  is the ⇧⌘R entry point.
* **Onboarding:** add a `Step` to `settings/onboarding.rs` (`STEPS`, `title`, `subtitle`,
  `icon`, a body arm — any `settings/<section>::render` can be reused via the panel).
  `app.open_onboarding(step, ..)` / `close_onboarding` (sets `settings.onboarded`).
* **Add a Now Playing design:** add a `NowPlayingLayout` variant (`settings/prefs.rs`:
  `ALL`, `title`, `key`, and `own_background` if it brings its colours), a
  `render_<design>` in `np_designs.rs` (reuse `artwork_shaped`, `progress_bar`,
  `times_row`, `transport_view`, `lyric_snippet`, `like_icon_button`), one arm in
  `render_now_playing_screen` (and `design_backdrop` for its own background), and a
  schematic preview in `settings/appearance.rs`. Keep it free of continuous frames: ask
  for timed renders with `Frame::wake_at` / `wake_in` (the time label already wakes once
  a second); a part that has to change at display rate belongs in a cached region
  (`np_region.rs`) or must be cheap.
* **Theme tokens:** colour our views with `cx.theme()` (gpui-component tokens, all
  derived from the look) or `theme::AppColors::get(cx)` / `theme::AppPalette::get(cx)`
  (accent, on-accent, secondary, tertiary, surface tint, scrim, text on artwork) — never
  literal colours outside artwork-coloured Now Playing designs. Corner radii go through
  `theme::radius(px)`, paddings through `theme::space(px)`, track rows are
  `theme::row_height()` tall. A new look option belongs in `CustomTheme`
  (`settings/theme_prefs.rs`, with a default that changes nothing) and `themed_colors`.
* **Profiles / theme files:** profiles carry whole top-level sections
  (`settings/profiles.rs::PROFILE_SECTIONS`), theme files `custom_theme`, `now_playing`
  and `appearance.theme` / `font_scale`; both also carry the player bar's sections listed
  in `settings/portable.rs::BAR_STYLE_KEYS`. A new field in one of these structs is picked
  up automatically; a new *section* is one entry in those lists. Minimal / Fast values:
  `profiles::minimal_settings`.
* **Player bar** (see [Player bar](#player-bar-redesign-progress-styles-backgrounds)):
  a progress style is a `ProgressStyle` variant (`settings/bar_prefs.rs`: `ALL`, `title`,
  `key`) plus an arm in `components/progress.rs::paint_track` (quads / one path per
  frame; positions in, nothing stored); a background is a `BarBackground` variant plus
  an arm in `bar_fx::BackgroundFx::layers`. Static by default: anything that moves on
  its own must ask for frames with `Frame::wake_in` at a capped interval only while
  playing, and say so in `PlayerBarConfig::animates` (and `least_resources` for the
  Minimal / Fast profile). A new bar control goes into `bar_layout` with a priority.
  Never request frames from the zones (`AppRegion`s): they are cached while the bar
  animates.
* **Renaming the app:** edit `packaging/app.env` only (see Install / build).
* **UI text** (see [Translations](#translations-ui-languages)): add the English string to
  `locales/en/<area>.lang` and use `t!("key")` / `t!("key", name = value)` /
  `tp!("key", count)`; an unknown key is a compile error. Never hard-code the app's name
  (`{app}` in the catalog). **Add a language:** create `locales/<code>/` with the English
  file names, translate, check with `cargo test -p music-gpui i18n::` (keys,
  placeholders, plural forms) and add its plural rule to `i18n/plural.rs` if the
  language isn't listed there.

### Features

* **Languages:** 18 UI languages, live switching, system default; see
  [Translations](#translations-ui-languages).
* **Shell:** sidebar (Home, Explore, Charts, Moods & genres, New releases, Library,
  History, Stats, Downloads, Local files, Import & export, Listen together, Settings), header with back/forward and the
  search field, routed content, player bar, queue panel, Now Playing overlay. Window
  title follows the page. Back/forward keep up to 64 entries; the 8 nearest keep
  their page alive (state + scroll), older ones are rebuilt and get their scroll offset
  back. Typing in the search field replaces the current search entry (no history spam).
* **Search field:** typing shows YouTube Music suggestions (`search_suggestions`,
  220 ms debounce, cached per query): completed queries plus direct hits (artists,
  songs, albums with artwork); with an empty field it lists recent searches (× removes
  one). ↑ / ↓ move through the list, Enter picks the highlighted entry or searches the
  text, Esc closes it. Picking a song suggestion starts its radio, an artist / album /
  playlist opens its page.
* **Search** page: **All** (top result card with Play / Shuffle / Radio and its songs,
  then one block per category with "See all") and tabs for Songs, Videos, Albums,
  Artists, Community playlists, Featured playlists, Podcasts and Episodes (fetched when
  first opened; virtualized rows; more results load as you scroll). Each query is its
  own history entry (replaced while you stay on search). A spelling correction shows as
  "Showing results for *X* · Search instead for *Y*" (the latter reloads All without
  correction) or "Did you mean *X*?"; the history keeps the typed query.
  Song rows: click plays the list from that row; hover shows play next / add to queue /
  more; right click (or "…") opens play, play next, add to queue, start radio, add to
  playlist, like, go to artist (one entry per artist with an id), go to album.
  Album / playlist / artist / podcast rows and cards open their page; their menu has
  Open, Play, Shuffle, Play next, Add to queue, Start radio and Go to artist.
* **Home:** the YouTube Music home feed: mood chips (Energize, Relax, …; click again for
  the full feed), song shelves as a quick-picks grid (four rows, scrolling sideways),
  albums / playlists / artists as cards, "See all" where YouTube offers it, more shelves
  as you scroll. "Played this session" stays on top until a persistent history exists.
* **Explore** (new releases / charts / moods shortcuts, a two-row moods carousel, the
  explore shelves), **Charts** (country picker with search; song and video charts as
  numbered lists of 20, artists as cards), **Moods & genres** (tiles per category →
  category page of playlists), **New releases** (album / single grid).
* **Artist page:** round art, name, monthly audience / subscribers, subscribed state,
  Play (top songs), Shuffle and Radio, description, top songs as a track list, albums /
  singles / videos / playlists / related artists as cards; "See all" opens the artist's
  items page (discography grid or song list, more as you scroll) or the playlist.
  **Album page:** art, type · year, linked artists, track count, Play / Shuffle / Radio /
  Add to queue, numbered tracks (albums over 200 tracks load the rest right away, with a
  Retry footer), other versions. **Playlist page**: author, track count, description,
  Play / Shuffle / Radio / Add to queue (/ Change cover… for your own playlists when
  signed in), tracks in a virtualized list that loads more rows as you scroll.
  **Podcast page** (`Route::Podcast`, `MPSP…`): art, publisher, episode count,
  description, Play / Shuffle / Add to queue / Save to library (signed in), episodes
  (art, title, date · length, two lines of description) virtualized with infinite scroll;
  click plays from that episode, the menu adds "Open episode page" (`Route::Episode`:
  Play, Add to queue, Go to podcast, full description). Episodes are videos and play
  through the normal resolver; "Go to podcast" replaces "Go to artist" in their menus.
* **Radio:** "Start radio" (track menus, song suggestions, top result, header buttons of
  artists / albums / playlists) replaces the queue with the radio mix (`InnerTube::next`
  with `RDAMVM…` / `RDAMPL…` / the artist's radio); while it plays, the next batch loads
  when three songs are left. **Autoplay similar songs** (Content & region, on by
  default): when an ordinary queue's last song starts, songs from its radio are added.
* **Loading, empty and error states** on every online page (spinner; error with Retry;
  "Load more" / Retry footer if a continuation fails). Fetched page data is cached for
  20 minutes (48 pages), so back/forward and revisits don't refetch; changing the
  content language / country, filters or the proxy clears it and reloads the page.
* **Content & region** settings: content language (41) and country (50) as searchable
  pickers (InnerTube `hl` / `gl`, applied live: pages reload, suggestions too), hide
  explicit (songs, albums, tracks, radio), hide music videos (OMV / UGC by video type:
  shelves, search, the Videos tab, playlists, albums — unless every track of the album is
  a video —, radio, queues, card actions), autoplay similar songs.
* **Network** settings: proxy Off / HTTP / HTTPS / SOCKS4 / SOCKS5 / SOCKS5h, host, port,
  user name, password (masked; kept in the keychain). Apply validates (host without
  scheme, port 1–65535, no credentials for SOCKS4), rebuilds music-core's HTTP agent
  (`http::configure`; an invalid proxy keeps the previous one) and reloads pages; "Test
  connection" fetches `youtube.com/generate_204` through it. Applied at launch before the
  first request (again with the password once the keychain has been read). **DNS**:
  System / Cloudflare / Google / Quad9 / Custom (https URL), "fall back to the system
  resolver", Apply, **Test DNS** (addresses of music.youtube.com and whether DoH, its cache
  or the system answered); `network.dns` in settings.json.
* **Queue** (`queue.rs`): play next, add to queue, remove, move (up/down buttons or drag
  and drop in the panel), clear, shuffle (current track first; turning it off restores
  the original order including tracks added meanwhile), repeat off / all / one (repeat-one
  replays on end, "next" still skips), history of played tracks. Shuffle and repeat are
  saved. "Previous" restarts past 3 s; the end of a track (including a ~60 s preview)
  plays the next one. Queue panel: now playing, up next, history (click to play again).
* **Settings page** with sections: Appearance (theme, text size, Now Playing layout +
  background, mini player style), Playback (queue modes, quality, normalization, skip
  silence, auto-skip, crossfade, speed + pitch, output device, sleep timer), Equalizer,
  Modules (provider modules, playback source), Lyrics (look, providers, translation,
  romanization), Content & region, Network, Privacy,
  Storage (file locations), Account (YouTube sign-in, planned), Scrobbling & Discord,
  SponsorBlock, Music Together, Spotify import, Backup & sync (settings / theme export and import), About (version, GPL-3.0 notice, ArchiveTune and Hummingbird credits). The old sheet
  is gone; the Now Playing gear opens the page.
* **Themes** (`theme.rs`, see [Theming round](#theming-round-customize-theme-dynamic-colours-profiles-downloads)):
  dark (gpui-component's), pure black, dynamic (Material-style scheme from the artwork),
  Customize theme (accent, darkness, surface tint, contrast, corners, density, font), six
  presets, theme files, profiles, text size 88–138 %. Applied to gpui-component's `Theme`
  and our `AppColors` / `AppPalette`.
* **Artwork cache** (`components/artwork.rs`): one cache for every view and both windows,
  LRU over the *decoded* size (width × height × 4 from the image header; 48 MiB, Settings
  → Storage → "Artwork in memory", 16 MiB in Minimal / Fast); evicting drops GPUI's
  decoded copy and its sprite-atlas tiles; images drawn in the last 3 s or held by Now
  Playing are never evicted. 4 dedicated fetch threads taking the newest request first,
  failed URLs retried after 30 s. Now Playing's 544 px art comes from it too (blur +
  accent for the last 8 tracks).
* **Mini player** (⇧⌘M, View menu, bar button): floating 400×124 window, artwork, title,
  previous / play / next, progress; stays visible over other apps (macOS panel with
  `hidesOnDeactivate` off); position saved.
* **Menu-bar item** (macOS, `tray-icon`): current track, play/pause, next, previous,
  mini player, show window, quit. It coexists with GPUI's run loop (the status item is
  just AppKit; menu events are drained by the poll). Not built on Linux/Windows.
* **Window frame** saved (content size, position, maximized) and restored if on screen.
* **Engine wiring** (`playback.rs`): the engine's `set_next` follows the queue on every
  poll and after `play` (shuffle, repeat-all wrap, repeat-one = the same track again,
  edits); when `PlaybackState::advance_count` grows (gapless / crossfade switch,
  auto-skip after a load error) the queue moves to the track the engine now plays
  without calling `play` (catching up if the queue changed after the preload). The bar
  shows "Buffering…" and "Couldn't play “…”, skipped". The demo clock mimics this
  (next track, advance count, speed, sleep timer), so it is testable without audio.
* **Playback options** (Settings → Playback, persisted, applied live): quality
  (low / normal / high), loudness normalization, skip silence, skip failing songs,
  crossfade 0–12 s (0 = gapless), speed 0.5–2× and pitch ±12 st with reset, output
  device (listed off-thread, saved by id, "not connected" when missing).
* **Sleep timer**: 5 / 15 / 30 / 45 / 60 min, end of track, a custom length (slider), off —
  in the bar (moon icon, remaining time as its label), the Now Playing header and the
  Playback section. "End of track" moves on to the next song and waits there paused.
* **Now Playing**: eight designs — split (artwork + controls | lyrics), big artwork
  (centered, the current and next lyric line below the controls), lyrics first (lyrics
  across the screen, artwork / title / progress / transport in a bottom strip), and,
  after ArchiveTune's player designs (`np_designs.rs`): **immersive** (full-bleed artwork
  fading into black, large type, the current lyric line, icon-only transport), **card**
  (a floating card: "playing from", square art, round shuffle / like buttons, flat
  progress with the audio format between the times, volume), **minimal** (type only:
  huge title and time, the window fills with colour from the left as the song plays),
  **editorial** (two tones from the artwork, a round cut-out cover, serif headline, a
  "bento" of PLAY / PAUSE pill, next / previous circles, progress, toggles) and **with
  queue** (artwork and controls beside the next eight songs, click to jump) — and four
  backgrounds: blurred artwork, gradient between two artwork colours, solid colour,
  animated glow (three soft lights in the artwork's colours that drift only while
  playing; pre-rendered images, since GPUI has no radial gradients). Immersive, minimal
  and editorial bring their own colours. Colours cross-fade between tracks. Design and
  background are also in the header's menu. Mini player style: theme or blurred artwork.
* **Lyrics cards** (`card_render.rs`, `lyrics_card.rs`; after ArchiveTune's
  `LyricsImageCard` / `LyricsShareDialog`): the share button in the lyrics toolbar
  switches the synced lyrics to selection (click up to six lines, the toolbar shows the
  count, Cancel / Create card…); the dialog previews the card and offers background
  (blurred artwork, album colour, dark, light), text alignment, size, shape (square
  1080², portrait 1080×1350, story 1080×1920) and the cover on / off, then **Save
  image…** (PNG through the system save dialog) or **Copy image** (PNG on the
  clipboard). The image is drawn offscreen in pure Rust on a worker thread: tiny-skia for
  the background (blurred / gradient), the glass panel and the rounded cover; the text
  laid out by the card renderer (word wrapping, characters of CJK text break anywhere,
  auto-fit from a length-based size down to a minimum, a gap between selected lines) and
  drawn by resvg/usvg, which shapes it with rustybuzz and falls back across the system
  fonts (kana / hanzi / hangul come from the system's CJK font). Both crates were already
  linked through GPUI.
* **Lyrics** (`lyrics.rs`): `LyricsService` with the provider order, switches and
  Paxsenix / BetterLyrics keys from Settings → Lyrics; results cached in memory; the
  pane shows provider · word-/line-synced/unsynced, a translate toggle and a reload
  button (searches again, skipping the cache). Real word timings drive the per-word
  fill; line-synced lyrics light the whole line. Background vocals (smaller, italic,
  their own word timings) and duet singers (other side) when the provider has them.
  Romanization line per script (Japanese — kanji readings once the dictionary is
  downloaded: Settings → Lyrics → Romanization shows its size, a progress bar with Cancel,
  Remove; turning Japanese on fetches it the first time; it lives in the platform cache dir,
  `$MUSIC_CONFIG_DIR/cache/ja-dict` with that variable —, Korean, Chinese pinyin, Devanagari,
  Cyrillic, others). Translation (Google or any OpenAI-compatible endpoint, target
  language, off by default) is requested once per song and translator settings and kept in
  the library database (`CachedTranslator`), so reopening a song or restarting doesn't ask
  again; failures show with a Retry button, and a rate-limited one ("rate-limited this
  connection; try again in 2 min") retries by itself after the back-off. Text size, line spacing, left / center /
  right alignment, and six animation styles (None, Fade, Glow, Slide, Karaoke — the
  original renderer —, Apple) as variants of `KaraokeLine` (sweep vs. per-word fade,
  feather width, lift, scale, glyph halo). Click a line to seek; unsynced lyrics show as
  a plain scrollable list.
* **Scrobbling & Discord**: scrobble rules (percent, at most N minutes, ignore songs
  under N s); Discord activity type, name, details/state templates, large/small image,
  two buttons, show while paused (applied to the running presence without reconnecting).
* **Player bar**: three zones, five progress styles (Standard, Thick, Simple, Wavy,
  Equalizer), a background that reacts to play / pause (artwork glow, dynamic gradient,
  pulse), elapsed / remaining time, hover thumb + time bubble, volume with mute, scroll
  and a percentage bubble, lyrics button; static by default — see
  [Player bar](#player-bar-redesign-progress-styles-backgrounds).
* **OS media controls**, **Equalizer**, **volume**: unchanged (history below).

### Library, account, downloads, local files

| ![Library songs](docs/screenshots/library-songs.png) | ![Multi-select](docs/screenshots/library-select.png) |
|---|---|
| ![Playlist](docs/screenshots/library-playlist.png) | ![Add to playlist](docs/screenshots/library-add-to-playlist.png) |
| ![History](docs/screenshots/library-history.png) | ![Stats](docs/screenshots/library-stats.png) |
| ![Year in Music](docs/screenshots/library-year.png) | ![Year in Music, bars](docs/screenshots/library-year-bars.png) |
| ![Downloads](docs/screenshots/library-downloads.png) | ![Local files](docs/screenshots/library-local-files.png) |
| ![Account](docs/screenshots/library-account.png) | ![Paste cookie](docs/screenshots/library-cookie-dialog.png) |
| ![Storage](docs/screenshots/library-storage.png) | |

* **Services** (`store.rs`): `library.sqlite` and the download store (`media/downloads`,
  `media/cache`) are opened at startup in the settings directory (`MUSIC_CONFIG_DIR`
  respected); without them the app still works, library entries are disabled.
* **Play history**: the 250 ms poll counts wall time while a track plays; leaving it (next,
  end, another track, quit) records it with `record_play` once it played ≥ 30 s
  (ArchiveTune's default), unless Privacy → "Pause listen history". Downloaded copies are
  recorded under their YouTube id.
* **Saved queue**: tracks, current index, play order while shuffled, repeat mode and position
  are saved on every queue change, every 10 s while playing and on quit
  (`Library::save_queue`). On launch the queue comes back paused, showing its track in the
  bar; Play starts it and seeks to the saved position.
* **Likes**: heart on track rows (on hover, always once liked), the player bar and Now
  Playing; "Like / Remove from liked songs" in menus. Local DB, plus `rate_song` on YouTube
  Music when signed in.
* **Library page** (tabs replace the route): Songs (library = added, liked or local), Liked,
  Albums, Artists, Playlists (local and YouTube mirrors), Podcasts (the account's saved
  shows, fetched from YouTube Music when signed in), Downloaded, Local files. Song tabs:
  virtualized list, filter, sort (date added, title, artist, plays, listening time, last
  played; picking the current sort flips the direction), Play / Shuffle, and **Select**:
  checkboxes + action bar (play, play next, add to queue, add to playlist, like, download,
  remove from library / unlike / delete downloads). Albums / Artists: card grids with sort
  and "Saved / Subscribed only"; they open **library views** (`Route::LibraryAlbum` /
  `LibraryArtist`: the collection's songs, save / subscribe, "Open on YouTube Music" for
  real ids).
* **Playlists**: "New playlist" and "Add to playlist…" (dialog: pick an editable playlist or
  create one, optionally also on YouTube Music); `Route::LocalPlaylist` page with rename,
  delete (confirmation), per-row move up / down / remove, multi-select remove, and "Refresh"
  for YouTube mirrors. Edits to the account's own playlists go to YouTube Music as well
  (`edit_playlist` actions with `setVideoId`s); deleting a saved playlist removes it from
  the account's library.
* **History**: grouped by day (Today / Yesterday / date), play time per entry, remove an entry,
  clear (confirmation), "Show more" in pages of 300.
* **Stats**: 7 / 30 / 90 days, this year, all time — listening time, plays, unique songs /
  artists / albums, top songs / artists / albums, listening by hour and weekday; **Year in
  Music** (`Route::YearInMusic`) per year with history: totals, top song / artist, favourite
  hour, busiest day, month-by-month and hourly bars.
* **Download all** (`components/download_header.rs`): Liked / library songs, local
  playlists, YouTube playlists (loading their remaining pages first) and albums; state
  none / partial / downloading ("N of M", Cancel) / all ("Remove downloads"); batches
  go through `DownloadManager::download_many` (one lock, two workers). Storage →
  Downloads: **Download liked songs** and **Remove download when unliked** (both off).
* **Downloads**: "Download" / "Remove download" in every song menu and the selection bar;
  the page polls `DownloadManager::states()` (queued / downloading / done / preview only /
  failed, cancel and retry), lists downloads with size and delete, shows download and cache
  usage. Playback prefers the stored file (`local_file`: downloads always, cached copies when
  complete). Downloads are full files; one made while only previews were available is
  marked "preview".
* **Local files**: music folders and skipped folders (folder picker), Rescan with progress
  (`local_files::scan_with_progress`, new in music-core), the scanned tracks (filter,
  select); local cover art (`file://`) goes through the artwork cache.
* **Account** (Settings → Account): on macOS **"Sign in with Google…"** opens a window with a
  WKWebView child (wry `build_as_child` on the GPUI window's NSView, non-persistent data
  store, Safari user agent) on Google's sign-in page continuing to music.youtube.com; once
  there with a SAPISID cookie, the `.youtube.com` cookies are read from WebKit's cookie store
  (`getAllCookies` + a channel polled by a GPUI timer — wry's `cookies()` spins the run loop
  and would re-enter GPUI) and the window closes. Everywhere: **"Paste cookie…"** with
  step-by-step instructions (copy the `cookie` request header from the browser's dev tools).
  The cookie goes to the keychain (`Secret::YouTubeCookie`); the account card shows avatar,
  name, handle / e-mail; channels (brand accounts, `account_channels` → `data_sync_id`) and
  the Google account index (`auth_user`) can be switched; Sign out forgets cookie and account
  (synced data stays). **Sync now** (also right after signing in / switching) mirrors liked
  songs, library songs, saved albums, subscriptions and playlists (with entries and
  `setVideoId`s) into the database; YouTube wins for likes / saved albums / subscriptions /
  playlist mirrors, local-only items are untouched. The artist page's Subscribe button and the library artist views subscribe /
  unsubscribe (`set_subscribed`).
* **Privacy**: pause / clear listen history, pause / clear search history (submitted searches
  are stored via `record_search`). **Storage**: file locations, play-cache limit (128 MB–8 GB),
  usage and clear, optional "cache played songs", artwork kept in memory (16–192 MB), download folder + usage, **backup export /
  restore** (library database + settings JSON without secrets, window frames or account;
  restoring keeps the current sign-in) followed by a "Restart now" prompt (`cx.restart()`).

### Provider modules (`.arch`)

| ![Modules section](docs/screenshots/modules-installed.png) | ![Install review](docs/screenshots/modules-review.png) |
|---|---|
| ![Settings form](docs/screenshots/modules-settings-form.png) | ![Diagnostics and log](docs/screenshots/modules-diagnostics.png) |
| ![Now Playing from a module](docs/screenshots/modules-now-playing.png) | ![Bar: module source badge](docs/screenshots/modules-bar.png) |
| ![Catalog](docs/screenshots/modules-catalog.png) | |

A module is a JavaScript provider in an `.arch` package (ZIP with `manifest.json`,
`provider.js`, optional `icon.png`) that implements `searchTracks(query, limit, context)` and
`getTrackStreamUrl(trackId, quality, context)`. The format and the JavaScript API v1 are
ArchiveTune's (`MODULES.md` in the ArchiveTune repository, ported in `music_core::modules`);
existing v1 packages install unchanged. The song keeps its identity (title, artists, library
entry, queue position): a module only supplies its audio. **No module, catalog or provider is
bundled or preconfigured.**

* **Playback** (`ModuleResolver`): for a YouTube song, each enabled module in priority order is
  searched with "title + primary artist"; the recording matcher (ArchiveTune's
  `ModuleTrackMatcher`: Jaro–Winkler ≥ 0.9 on normalized title and primary artist, matching
  live / remix / remaster / … markers, explicit flag, duration within max(5 s, 5 %)) picks the
  best acceptable result, and its stream URL plays. 15 s per module, 30 s for the sequence;
  results are cached until a minute before the URL expires (misses 30 s, failures 10 s). Then
  YouTube, unless the source is "Modules only". A module URL that expires (its declared
  `expiresAt`, or 401/403/410 mid-stream) is resolved again through the same module, past
  this cache (`ModuleResolver::refresh_track`), and the download resumes at the same byte.
  Local files never go through modules. Streams
  are any HTTP(S) URL with the module's headers, played while they download (FLAC, ALAC,
  MP3, AAC/M4A, Ogg Vorbis/Opus, WAV, AIFF); HLS / DASH results are rejected. Downloads use
  the same resolver, so a module source is downloaded as the full file.
* **Install**: from a file, a direct package URL, a descriptor (`{"download": …}`, followed up to
  4 deep, cycles rejected) or a catalog (`modules` / `category:*` / `external_sources`, an HTML
  page linking its `index.json`). Packages are validated before review (ZIP entries and sizes,
  4 MiB package / 128 KiB manifest / 3 MiB script / 256 KiB icon, nesting ≤ 32, `format` /
  versions, `sha256` and `iconSha256`, identity, capabilities, typed settings, quality options,
  runtime features, `minAppVersion`) and the script is run once to check that its exports match
  the declared capabilities. Updates revisit the import URL; catalog entries are checked against
  their `sha256`. Codes such as `ARCH_CHECKSUM` follow MODULES.md's diagnostics table.
* **Sandbox** (one QuickJS engine thread per module): ECMAScript built-ins plus the v1 host
  APIs — `fetch` (HTTP(S) only, app proxy, 8 MiB bodies, 10 / 15 / 20 s timeouts, 8 concurrent
  and 64 requests per call, redirects drop credentials across origins), `Headers`, `URL`,
  `URLSearchParams`, `TextEncoder` / `TextDecoder`, `atob` / `btoa`, `Blob`, `FormData`,
  `setTimeout` (≤ 128, ≤ 15 s), `AbortController`, `console` (to the module's log) — and the
  helpers `storage` (1 MiB per installation), `utils` (SHA-256/SHA-1/MD5, UUID, Base64) and
  `crypto.getRandomValues` / `randomUUID`. No files, no `require` / `import()`, no process
  access. 32 MiB heap, 1 MiB stack, interrupted at the deadline; a failed engine is discarded.
* **Settings → Modules**: playback source (YouTube only / modules first, then YouTube /
  modules only), install from file / URL / catalog URL with a review card (icon, identity,
  capabilities, required settings, checksum), installed modules in priority order (icon, name,
  version, author, capabilities, enable switch, move up / down, uninstall with confirmation),
  a settings form generated from the manifest (text, password with reveal, number / integer
  with bounds, toggle, select incl. `qualityOptions`) with Save, Check, Check for update, and
  diagnostics (last check, last song's outcome — resolved / no match + reason / failure code —,
  the module's log, copy diagnostics). Catalogs list their entries with Install / Update.
* **Where the audio comes from**: a badge in the bar and the Now Playing header —
  "YouTube · AAC 128 kbps", "Echo (test provider) · FLAC 16/44.1", "File · …" — from the decoder.
* **Storage**: `modules/registry.json` in the settings directory (0600) holds installed modules,
  order, switches and non-secret settings; secret settings (passwords, token-like keys) go to
  the keychain (`module.<installation>.<key>`; with `MUSIC_SECRET_STORE=file`:
  `modules/secrets.json`, 0600); script storage in `modules/storage/`. Backups do not include
  modules yet.

**Verified in the app** (release `screenshot` build, temporary `MUSIC_CONFIG_DIR`,
`MUSIC_SECRET_STORE=file`, real output device muted with `MUSIC_VOLUME=0.0`, screen locked):
the echo module served by `module_server` was added through a catalog URL, installed from its
URL (review card), configured (server URL, token as a secret), checked ("Ready"); with "Modules
first" the YouTube search hit "Instant Crush (feat. Julian Casablancas)" played as a 338 s
FLAC from the local server (badge "Echo (test provider) · FLAC 16/44.1", lyrics and artwork
from YouTube Music as usual), was seeked to 320 s and played to its real end — not a 60 s
preview —, then the engine advanced gaplessly to the next song, which the module didn't have:
it fell back to YouTube (badge "YouTube · AAC 128 kbps", "Preview"), and the diagnostics show
both outcomes ("Resolved · lossless:…", "No results"). **Not verified:** a real third-party
provider (none is bundled), the macOS file picker for "Install from file…" (the same import
path ran via `@module-install=PATH`), keychain storage of secret settings (the file store was
used), HLS / DASH sources (rejected by design), Windows / Linux.

Authoring: `cargo run -p music-core --example pack_module -- <dir> <out.arch>` packs and validates
a provider directory (adds `sha256` / `iconSha256`), `cargo run -p music-core --example
module_server -- <dir> [port]` serves files locally. `crates/music-core/tests/fixtures/modules/echo`
is a small test provider that serves the files listed in a server's `catalog.json`; the
`modules` integration tests use it against a local server (install validation, sandbox limits,
search → match → stream → full decode through the headless player, chaining / fallback, URL /
descriptor / catalog installs and updates, secrets, downloads).

### Integrations: SponsorBlock, Music Together, playlist import / export

(Relay, host transfer, Chromecast and the AI content filter:
[next section](#integrations-round-2-relay-host-transfer-chromecast-ai-filter-potoken).)

| ![SponsorBlock: Skip chip](docs/screenshots/integrations-sponsorblock-ask.png) | ![Settings: SponsorBlock](docs/screenshots/integrations-settings-sponsorblock.png) |
|---|---|
| ![Together: host](docs/screenshots/integrations-together-host.png) | ![Together: guest](docs/screenshots/integrations-together-guest.png) |
| ![Together: approval](docs/screenshots/integrations-together-approval.png) | ![Settings: Music Together](docs/screenshots/integrations-settings-together.png) |
| ![Import: matching](docs/screenshots/integrations-import-progress.png) | ![Import: results](docs/screenshots/integrations-import-results.png) |
| ![Settings: Spotify import](docs/screenshots/integrations-settings-spotify.png) | ![Import: saved](docs/screenshots/integrations-import-page.png) |
| ![Together: start or join](docs/screenshots/integrations-together-idle.png) | ![Together: waiting for approval](docs/screenshots/integrations-together-waiting.png) |

* **SponsorBlock** (Settings → SponsorBlock; on by default, `music_offtopic` only): when a
  song starts, its segments are looked up on a worker thread (`music_core::sponsorblock`,
  hash prefix only, cached per video / categories / server). While it plays, the poll skips
  a segment once per pass: *Skip automatically* seeks to its end and shows "Skipped:
  Non-music section (22 s)"; *Ask* puts a "Skip non-music section (19s)" chip next to the
  transport buttons until the segment is over. Categories: non-music section, sponsor,
  intro, outro, self-promotion, interaction, preview, filler, hook. Server URL (https only,
  validated), "Forget looked-up segments". Local files and guests of a Together session never
  skip on their own (a host's skip reaches guests through the room state).
* **Listen together** (sidebar; Settings → Music Together for name, port 42117, "guests can
  add songs", "guests can control playback", "approve guests"). **Host:** *Start session*
  listens on the port (all interfaces) and shows a `music-together://join?host=…&port=…&sid=…&key=…`
  link (random session id + key; "Copy code" gives `host|port|sid|key`). The room state —
  current track + up next (YouTube songs only), playing, position from the smooth clock,
  repeat / shuffle — goes out on every change and every 500 ms, stamped with the server
  clock. People list with *Let in* / *Decline* for waiting guests and *Remove* / *Ban*;
  room switches apply live. Guests' requests run through the normal player methods (play /
  pause / seek / next / previous / jump / repeat / shuffle; suggestions as *Play next* /
  *Add to queue* with a notification). **Guest:** *Join* with the link; the guest's queue
  becomes the room's, its player loads the host's track and seeks when it is more than
  0.7 s off (0.15 s while paused), using the host position + the message's age converted
  through the heartbeat clock offset; play / pause follow. Local shuffle is off while in a
  session. *Play next* / *Add to queue* anywhere in the app become suggestions to the host;
  previous / play / next on the page are requests (when the host allows). Kicks, bans,
  declined joins, a host that ends the session or goes silent for 15 s end the guest's
  session with a message. Across networks: a self-hosted relay and **Make host** (host
  transfer), see the next section.
* **Import & export** (sidebar): *Choose a file…* (Exportify CSV of a Spotify playlist,
  M3U / M3U8, or our own CSV / JSON), or — after connecting Spotify — a playlist link, one
  of *Show my playlists*, or *Liked songs*. The job reads the source, then matches every
  song on YouTube Music (4 searches in flight, progress bar, Cancel; tracks that name a
  YouTube video are taken directly). Results: found / not found counts, *Save N songs as a
  playlist* (name from the source, editable) → a local library playlist, a "Not found"
  list with the best guess (*Use guess*) and a search button, and the found songs as normal
  track rows. The job keeps running when you leave the page. *Export a playlist*: every
  library playlist as CSV (video id, title, artists, album, duration, URL), M3U8 (EXTINF +
  YouTube Music links / local paths) or JSON (full metadata; re-imports without matching).
* **Spotify** (Settings → Spotify import): uses the official Web API with *your own* app
  (developer.spotify.com → create an app → add the shown redirect URI
  `http://127.0.0.1:43819/callback` → paste its Client ID → Connect). The browser opens
  Spotify's consent page; a one-shot loopback listener takes the code (PKCE, no client
  secret); the refresh token goes to the keychain, the display name to the settings.
  ArchiveTune's `sp_dc`-cookie login is **not** implemented: Spotify's web-player token
  endpoint needs a TOTP generated from a secret extracted from the web player (ArchiveTune
  fetches it from a third-party gist), which isn't a legitimate client flow. Without an
  account, export the playlist with Exportify and import the CSV.

**Verification** (screen locked; `screenshot` build; temporary `MUSIC_CONFIG_DIR`s with
`MUSIC_SECRET_STORE=file`; `MUSIC_VOLUME=0.0`):

* SponsorBlock, real device (muted), `@play-id=kJQP7kiw5Fk` (a music video with locked
  `music_offtopic` segments): the live lookup returned 0–21.8 s and 249.4–281.5 s; in *Ask*
  mode the chip appeared at 0:06, `@sb-skip` landed at 21.8 s; in *Auto* mode `@seek=2` was
  followed by a skip to 21.8 s 0.35 s later. `music-core` unit tests cover the hash prefix,
  response filtering / merging (fixture), URL validation and the skip tracker; an ignored
  test hits the live API.
* Together, three app instances on one machine (demo clock): host "Hana" started a session
  (port 42117, LAN address in the link), guest "Ann" joined from the link file; the guest
  took the room's 10-song queue, loaded the host's track and seeked to the host position:
  1 s later guest 31.9 s vs host 31.6 s (+ message age), clock offset 2288 ms, both
  playing. Ann's suggestion arrived as *Play next* on the host ("Ann suggested …"), her
  pause request was refused while control was off (server error → notification) and
  applied after `@together-allow=control:on` — the host paused at 50.4 s, both guests
  followed within 0.2 s. With approval on, "Bob" waited (redacted room state, "Waiting"
  tag), `@together-approve` let him in, and he synced (seek to 45.2 s). Bob quitting shows
  "Bob left (Connection lost)". Unit tests: wire format (camelCase / `type` like the Kotlin
  classes), queue hash, links, clock estimate and the offset sign, the guest sync planner,
  and an end-to-end localhost session (wrong key, join, room state, clock, suggestion,
  refused / allowed control, approval, ban + rejoin refused, leave, host ends session).
* Import: an Exportify CSV (6 songs, one made up) → 5 matched with score 1.00 (Instant Crush,
  Get Lucky radio edit, Hello Goodbye, Shape of You, Blinding Lights), the made-up one
  unmatched with a 45 % best guess; saved as "Spotify mix"; exported as M3U8 / CSV / JSON,
  and the JSON re-imported as 6 direct hits without searching. Unit tests: Exportify +
  own CSV, M3U, JSON round trips, YouTube links, the matcher (recording / cover / duration
  cases) and the resolver (direct / matched / unmatched / cancel).
* Spotify: PKCE (RFC 7636 vector), authorize URL, loopback redirect (code, state mismatch),
  playlist-id parsing and the playlist-items JSON (fixture) are unit-tested.

**Not verified:** the Spotify sign-in and API calls against Spotify (no developer Client
ID available here — they are implemented per the documented Web API; Spotify's 2026
development-mode changes renamed `/playlists/{id}/tracks` to `/items`, the client tries
`/items` first and falls back), Together between two different machines / through a
firewall prompt, Together with real audio on both sides (only demo clocks), drag of the
progress bar by a guest (it is overridden by the host's state), physical input.

### Integrations round 2: relay, host transfer, Chromecast, AI filter, PoToken

| ![Together: hosting via relay](docs/screenshots/integrations2-together-relay-host.png) | ![Together: after a host transfer](docs/screenshots/integrations2-together-transferred.png) |
|---|---|
| ![Settings: Music Together (relay)](docs/screenshots/integrations2-settings-together.png) | ![Settings: AI content filter](docs/screenshots/integrations2-settings-ai-filter.png) |
| ![Casting (player bar)](docs/screenshots/integrations2-casting.png) | ![Settings: Chromecast](docs/screenshots/integrations2-settings-cast.png) |
| ![AI filter: "christmas songs" with holiday music hidden](docs/screenshots/integrations2-ai-filter-search.png) | |

* **Host transfer** (Listen together → a guest's **Make host**): the host sends that guest
  `host_transfer` with the room (queue, position stamped with its clock, repeat, room
  settings), the client ids that may rejoin without approval (everyone admitted + the old
  host) and the banned ones. The guest's app starts its own server with the same session id
  and key (`TogetherServer::start_with` + `HostOptions`; LAN: its port, relay: the same
  relay with the room's secret), takes over the queue / position / repeat / settings and
  answers `host_transfer_ready` (its LAN address). The old host broadcasts
  `host_transferred`; every `TogetherClient` reconnects on its own (same client id and
  name, waits for the welcome, retries refusals for 15 s, resets its clock and the sync
  planner), the old host stops its server and joins as a guest. ArchiveTune's bare
  `host_transfer` / `host_transferred` messages still decode.
* **Relay** (Settings → Music Together → Relay server, then **Host via relay**): the
  host keeps one WebSocket to a [`music-relay`](#relay-server-music-relay) server and
  serves the guests it tunnels in; the link is `music-together://join?url=wss…&sid=…&key=…`
  and works from any network. Guests speak the normal protocol to the relay URL, so the
  session logic (key, approval, heartbeats, requests) is the same code as on the LAN (a
  `Link` trait over a direct connection or a tunnelled one). The relay re-routes on a
  transfer: the new host claims the room with its secret, new connections go to it,
  connections to the old host stay with it until it leaves.
* **Chromecast** (player bar Cast button, Settings → Chromecast): devices are found with
  mDNS when the menu / section opens (4 s); casting swaps the engine for `Engine::Cast`,
  which plays the queue on the device and reports its state back as `PlaybackState`, so the
  player bar ("Casting to Living Room"), Now Playing, the queue and auto-advance (on the
  receiver's `FINISHED`) work unchanged; play / pause / seek / next / previous / volume go
  to the device; the local engine waits paused (media keys that reach it are forwarded);
  Stop casting continues here at the device's position. Audio goes through the app's
  `MediaProxy` (googlevideo: see below; local files too); "Let the device fetch YouTube
  audio itself" sends plain YouTube URLs directly and falls back to the proxy on an error.
  The sleep timer, EQ and speed don't apply while casting.
* **AI content filter** (Settings → AI content filter; off by default): an
  OpenAI-compatible endpoint (OpenAI, OpenRouter, Ollama / LM Studio on this machine), model
  and optional key (keychain); categories explicit lyrics, AI-generated music, holiday,
  children's, sleep sounds / noise, sped-up / slowed edits, covers & karaoke, live
  recordings, plus up to 8 of the user's own. Applied next to "hide explicit" in
  `browse::ContentFilter`: pages classify unknown songs / albums in batches of 25 on their
  fetch worker before filtering; UI-thread callers (radio batches, search suggestions) read
  the cache and classify in the background. Verdicts are cached per video / album id in
  `<config>/cache/ai_filter.json` (re-asked when a new category is selected); unknown =
  shown. Only id, title, artists and album are sent, and nothing while it is off.
  (ArchiveTune's "AI content filter" is a different thing — channel blocklists of
  AI-generated music from a third-party list — and isn't used.)

**Verification** (`MUSIC_VOLUME=0.0`, demo clocks, temporary `MUSIC_CONFIG_DIR`s):

* Relay, three app instances + `music-relay` on localhost: Hana hosted via relay
  (`ws://127.0.0.1:42118`), Ann and Bob joined from the link; Hana handed the session to
  Ann at 24 s: Ann started hosting through the relay (room secret), Bob was re-routed
  ("Ann is the host now; reconnecting…" → "Reconnected to the new host"), Hana rejoined as
  a guest; 20 s later all three played the same track (status lines from each app's own
  timer, started up to 1 s apart: 34.9 / 36.3 / 36.5 s), the relay log showing the second
  host connection and the reconnects; `/health` answered.
* LAN, three instances with approval on: Hana let Ann and Bob in, transferred to Ann (port
  42119): Bob and Hana reconnected to Ann's address without approval (trusted ids), the
  positions were continuous (6.8 s at 22 s → 18.7 s at 34 s).
* Unit / integration tests: wire format (incl. ArchiveTune's bare transfer messages),
  relay envelopes, relay links / URLs; `host_transfer_on_the_lan` (state, trusted / banned
  lists, reconnect, clock); `crates/music-relay/tests/relay_e2e.rs` (relay + host + 2
  guests: wrong key, approval, full room, health, room state, clock through the relay,
  suggestions, a second host without the secret refused, transfer re-routed, end of
  session).
* Chromecast: `cargo run -p music-core --example cast -- list` found no device on this
  network (none available). `-- check khnokW3Mw24` resolved a TV-client stream and
  requested it like a receiver would: googlevideo accepted another User-Agent and no
  headers, but a GET without `Range` was throttled (6.1 MB in 173 s) while
  `Range: bytes=0-` took 5 s, after the pre-roll wait — hence the proxy by default. In the
  app against `examples/fake_cast_receiver.rs` (CASTV2 over TLS, downloads like a receiver):
  the current track was loaded at the current position (22 s), the receiver fetched the
  real stream through the proxy (2.2 MB, 206, 3.7 s), pause / play / seek 120 s reached it,
  the end of the track advanced the queue (next LOAD, 3.4 MB), Stop casting resumed here.
  Unit tests: protobuf framing, a fake TLS receiver (launch, load, controls, volume,
  refused load, stopped on the device), the proxy (ranges, 404 / 416, files).
* AI filter: request body / lenient parsing / a local stub server (batches, auth, cache,
  category change, persistence); in the app, a Python stub endpoint classified 17 search
  results for "christmas songs" (13 holiday) and the search page hid them.
* PoToken Windows / Linux: type-checked and clippy-clean for both targets in a temporary
  crate including the real files (stub `pkg-config` for the GTK sys crates; removed);
  `webview_host` tests run the shared call sequence against a fake engine. macOS unchanged
  (the default `user_agent` is the old constant).

**Not verified:** a real Chromecast / Google TV / Nest device (none here; the receiver's
real fetch behaviour, codec support for Opus-in-WebM and the TLS handshake with a real
device certificate are untested), PoToken on real Windows (WebView2) and Linux
(WebKitGTK) machines, the relay behind a real TLS reverse proxy and across the internet,
a host transfer between two different machines, a real LLM endpoint (only stubs).

### Relay server (`music-relay`)

`crates/music-relay`: one small binary (std threads + tungstenite, ~0.9 MB release) for
Music Together sessions across networks. It never decides anything: it checks a guest's
first frame (session id + key against the room's key hash; same answer for unknown
session and wrong key), then forwards text frames between the host and its guests.

```sh
cargo build --release -p music-relay
./target/release/music-relay --listen 0.0.0.0:42118 -v
curl http://localhost:42118/health   # {"app":"TuneDesk","connections":1,"guests":0,"rooms":0,"status":"ok"}
```

Options: `--max-rooms 100`, `--max-guests 20` (per room), `--max-connections 1000`,
`--max-message-kib 1024` (a guest's frame; hosts may send twice that), `--rate 20`
(messages / s per guest, burst 3×; over it a guest is disconnected, a host's excess frames
are dropped — room states repeat), `--idle-timeout 60`; `MUSIC_RELAY_LISTEN` sets the
address. Rooms live while their host is connected (a host transfer moves them).

TLS is left to a reverse proxy; give the app the `wss://` URL. Caddy:

```
relay.example.com {
    reverse_proxy 127.0.0.1:42118
}
```

nginx:

```nginx
location / {
    proxy_pass http://127.0.0.1:42118;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_read_timeout 120s;
}
```

The relay sees what the session sends (song ids and titles, display names, the session
key in each guest's hello), like any host would; run your own.

### Content round: video types, podcasts, long albums, DNS-over-HTTPS, covers

| ![Podcast show](docs/screenshots/content-podcast.png) | ![Episode page](docs/screenshots/content-episode.png) |
|---|---|
| ![Album of 294 tracks, all loaded](docs/screenshots/content-album-long.png) | ![Hide videos: playlist without its music videos](docs/screenshots/content-playlist-hide-videos.png) |
| ![Spelling correction](docs/screenshots/content-search-corrected.png) | ![Settings: Network → DNS](docs/screenshots/content-settings-network-dns.png) |
| ![Settings: Content & region](docs/screenshots/content-settings-content.png) | ![Library → Podcasts (signed out)](docs/screenshots/content-library-podcasts.png) |

* **Video types.** Every InnerTube track keeps `musicVideoType` (`Track::music_video_type`),
  so **Hide music videos** works inside lists, not only on shelves: `ContentFilter::tracks`
  drops OMV / UGC tracks from playlists (online pages and card actions), radio batches,
  queues built from watch targets and search tabs; `ContentFilter::album_tracks` does the
  same for albums, except that an album YouTube only has as official-audio videos (all
  OMV, e.g. *Random Access Memories*) stays complete instead of turning up empty.
* **Podcasts**: show and episode pages, the library tab and routing (`MPSP…` →
  `Route::Podcast`, `MPED…` → `Route::Episode`, episode menus "Go to podcast"); see
  Features above.
* **Long albums**: album pages list at most 200 tracks (a 294-track box set had no
  continuation); the rest comes from the album's playlist (`VL<audioPlaylistId>`, whose first
  pages repeat the 200 and are deduplicated). Card actions use `album_complete`; playlist
  card actions follow up to 10 continuations.
* **DNS-over-HTTPS**: Settings → Network → DNS (see Features).
* **Playlist covers**: "Change cover…" on your own playlists (online page when editable,
  library mirror page) → `playlist_cover::prepare_cover` (ArchiveTune's crop / scale / JPEG
  steps) → `InnerTube::upload_playlist_cover` (resumable upload to
  `playlist_image_upload/playlist_custom_thumbnail`, then `ACTION_SET_CUSTOM_THUMBNAIL`).
* **Search history fix**: see Extension points → Search history; the corrected-query banner
  never rewrites history.

**Verified:** fixture tests on trimmed live responses (video types on search / album /
playlist / next / artist rows, podcast page with and without continuation, episode
continuation, episode page, 294-track album + its playlist pages, corrected search, a
synthetic library-podcasts page); the live `browse` example (48 calls ok, incl. a
podcast with continuation and episode page, `album_complete` → 294 of 294, correction and
exact search) also with `MUSIC_DOH=cloudflare`; DoH unit tests against a local RFC 8484
stub server (resolution, A + AAAA, TTL cache, IP literals / local names, fallback on/off,
unreachable server) and the live providers (`cargo test -p music-core doh -- --ignored`:
Cloudflare and Google answer, Quad9's port 5053 is unreachable from this network). In the
app (screenshot build, temporary config): the screens above; the 294-track album loaded
in 3 extra requests; `@dns=cloudflare` + `@dns-test` → "music.youtube.com → … via
DNS-over-HTTPS in 52 ms" and `network.dns` saved; with Hide videos on, *Indie Disco* starts
at "Last Nite" (its OMV opener "Feel Good Inc." gone); a JRE episode (2 h 43 min) played
through the normal resolver on the real device (muted): full 9821 s length, seek to 1 h.
Search history: `@submit` "justice genesis", "justice genisis" (YouTube shows results for
"justice genesis"), "Daft Punk", "daft punk", "queen", `@quit` 0.1 s later → the
database holds queen, daft punk, justice genisis, justice genesis and the next launch lists
them in that order; unit tests for the ordered writer and case-insensitive dedupe.
**Not verified:** anything signed in (library Podcasts tab, Save to library, cover
upload — the upload's requests are unit-tested only), Quad9 DoH, DoH through a proxy, the
original "vanished" report itself (not reproduced; the race above is the cause found in the
code, removed by design), physical clicks on the new buttons (script actions instead).

### Translations (UI languages)

| ![German: search](docs/screenshots/i18n-de-search.png) | ![German: Appearance with the language picker](docs/screenshots/i18n-de-appearance.png) |
|---|---|
| ![German: welcome guide](docs/screenshots/i18n-de-onboarding.png) | ![Russian: Playback settings](docs/screenshots/i18n-ru-playback.png) |
| ![Russian: Now Playing](docs/screenshots/i18n-ru-np.png) | ![Japanese: search](docs/screenshots/i18n-ja-search.png) |
| ![Japanese: Appearance](docs/screenshots/i18n-ja-appearance.png) | ![Chinese (Simplified): Now Playing](docs/screenshots/i18n-zh-CN-np.png) |
| ![Chinese (Simplified): library](docs/screenshots/i18n-zh-CN-library.png) | ![Hindi: library](docs/screenshots/i18n-hi-library.png) |
| ![Hindi: Content & region](docs/screenshots/i18n-hi-content.png) | ![RTL check: Arabic lyrics (not shipped as a UI language)](docs/screenshots/i18n-rtl-arabic-content.png) |

**18 UI languages**, each 100 % of the 1,522 English keys (1,481 plain + 41 plural
groups): en, de, es, fr, hi, id, it, ja, ko, nl, pl, pt-BR, ru, tr, uk, vi, zh-CN, zh-TW
(`cargo test -p music-gpui coverage -- --nocapture` prints the table). Translations are
partly from ArchiveTune's Weblate project (GPL-3.0, see `crates/music-gpui/THIRD_PARTY.md`;
a matching English string's translation was reused where the meaning was the same —
15–160 keys per locale, i.e. 1–10 %), the rest written for this project.

* **Layer** (`src/i18n/`, no new crates — `sys-locale` was already in GPUI's graph):
  catalogs `locales/<locale>/*.lang` (`key = value`, `{name}` placeholders, `key.one` /
  `key.other` … plural forms, `{app}` = the app's name, filled in at build time) are
  embedded by `build.rs` as sorted key tables and one text blob + offsets per locale.
  `t!("key")` resolves the key in a `const` (unknown key = compile error) and returns a
  `&'static str` — an atomic load, two array reads, a slice; no allocation, so it is used
  in render paths at 60 fps. `t!("key", name = value)` and `tp!("key", n)` (CLDR plural
  rules for every shipped language, `i18n/plural.rs`) allocate a `String`. Missing
  translations fall back to English. `rust-i18n` (already linked by gpui-component) was
  considered: runtime string-key hashing, `%{}` replacement and no plurals or compile-time
  key checks.
* **Formatting** (`i18n/format.rs`): digit grouping per language (CLDR symbols, Indian
  grouping for Hindi, no grouping of 4-digit numbers in es / pl), full dates with genitive
  month names where the language needs them, "5 min ago", listening time, file sizes;
  playback times stay `m:ss`.
* **Language setting**: `appearance.language` (`system` or a code). The system's
  preferred languages (`sys-locale`: macOS `preferredLanguages`, Windows UI languages,
  Linux `LANGUAGE` / `LC_*` / `LANG`) map to a catalog (`de-AT` → de, `zh-Hant-HK` →
  zh-TW, `pt-PT` → pt-BR). Settings → Appearance → Interface language and the welcome
  guide's first page list "System default (…)" and every language by its own name
  ("Deutsch — German"). Switching is live (`MusicApp::apply_language`): menus rebuilt with
  `cx.set_menus` (the windows menu re-registered when "Window" is translated), the
  menu-bar item relabelled, tracked input placeholders reset (`i18n::track_placeholder`),
  pickers rebuilt, every window refreshed (cached Now Playing views too). gpui-component's
  own few strings follow where it has them (it, pt-BR, zh). On a fresh install the YouTube
  Music content language starts from the UI language and the country from the system
  region; during the welcome guide the content language follows the picker.
* **Not translated, on purpose**: logs, script actions, env vars, ids; error details from
  music-core / servers (English, inside a translated sentence); the default Discord
  activity text and the Together display name / reasons (other people see them); YouTube
  Music content (its own language setting); module manifests; brand names.
* **Tests**: `every_locale_matches_english` (keys ⊆ English, placeholders match, every
  plural category the language uses exists, no hard-coded app name),
  `lookups_fall_back_and_fill` (every key resolves in every locale), and
  `english_literals_stay_out_of_ui_code` — a lint over `src/` that fails on string
  literals that read like English outside `t!` / logging / parsing calls (allowlist:
  `src/i18n/lint_allowlist_*.txt`, `// i18n-ignore`). Plural, grouping and system-tag
  mapping have unit tests.
* **Layout**: checked with screenshots in de, ru, ja, zh-CN, hi (and spot checks of the
  others): settings section list widened to 232 pt, settings rows wrap their label and
  keep the control, button rows `flex_wrap`, sidebar / queue / player-bar labels truncate.

**Right-to-left**: GPUI shapes Arabic correctly (joined, each line right to left — see the
last screenshot, Arabic lyrics), but text stays left-aligned, mixed-direction lines run
left to right and nothing mirrors; Arabic, Persian and Hebrew are therefore not shipped
(ArchiveTune has them; adding them is a catalog + `plural.rs` change once GPUI aligns and
mirrors RTL).

**Hindi caveat (GPUI)**: Devanagari shapes correctly (conjuncts, matras), but gpui-pre
0.3.10's `LineWrapper::is_word_char` lacks the Devanagari block (U+0900–U+097F), so a
wrapped Hindi paragraph may break inside a word, and wrapping / truncation sum unshaped
per-character widths, so a label in a box exactly as wide as its text can lose a final
mark (ं). Sidebar labels were fixed by giving them the row's width; gpui-component buttons
whose label ends in such a mark (e.g. "चुनें") can still show it cut. Upstream fix: add
`'\u{0900}'..='\u{097F}'` (and `'\u{A8E0}'..='\u{A8FF}'`) to `is_word_char`.

**Measured** (release, macOS arm64, display awake, `MUSIC_ON_TOP=1`, 1180×793, "Instant
Crush" seeked to 1:10, muted real device; three interleaved runs each, avg of
`ps -o %cpu=` over 12 s):

| | before (main + spectrum merge base) | i18n, English | i18n, German |
|---|---|---|---|
| Now Playing, word-synced lyrics | 8.9 / 9.2 / 10.2 % | 9.1 / 9.2 / 9.8 % | 10.1 / 10.2 / 10.7 % |
| Now Playing closed, playing | 1.1 / 2.4 / 6.1 % | 0.4 / 1.4 / 1.4 % | 1.1 / 1.2 / 1.3 % |
| UI cost per frame (`@bench=600`, lyrics) | 0.82 / 0.84 ms | 0.84 / 0.74 ms | 0.83 / 0.91 ms |

RSS 453–477 MiB in every variant. Release binary 42,907,472 → 44,592,592 B (+1.6 MiB:
~1.0 MiB of catalog text for 18 languages, 0.23 MiB of offset tables, the rest code incl.
the merged spectrum feed). **Verified** by script (`@lang`, `MUSIC_LANG`): live switch
en → hi with Now Playing open and on the settings page (menus, sidebar, cached header,
pickers), `appearance.language` saved; first run with `MUSIC_SYSTEM_LANG=de-DE` sets the
content language to de and the country to DE; `@theme` / `@lyrics-align` / `@layout` still
match under German (stable keys, saved values checked). **Not verified**: the macOS menu bar and the menu-bar
item after a switch on screen (built through the same calls; not looked at), Windows /
Linux system-language detection (only the macOS path ran), native-speaker review of the
translations, every screen in every language (five locales were looked at).

### Keyboard shortcuts and menus

| Keys | Action |
|---|---|
| Space | play / pause (not while typing) |
| ⌘→ / ⌘← | next / previous (restart past 3 s) |
| ⌘↑ / ⌘↓ | volume ±5 % |
| ⌘F | search field |
| ⌘, | settings |
| ⌘L | Now Playing |
| ⌥⌘Q | queue panel |
| ⇧⌘M | mini player |
| ⌘S / ⌘R | shuffle / cycle repeat |
| ⌘[ / ⌘] (also mouse back/forward buttons) | back / forward |
| ⌘1 / ⌘2 / ⌘3 / ⌘Y | Home / Explore / Library / History |
| ⇧⌘R | Recognize: open the page and listen (again: stop) |
| Esc | close an open menu, popover or dialog; else Now Playing, else the queue panel, else back |
| ⌘H, ⌥⌘H, ⌘M, ⌘Q | hide, hide others, minimize, quit |

The menu bar (Music, Edit, View, Controls, Window) carries the same actions with their
shortcuts. All shell actions are global listeners that defer to the main window's app,
so keys, menus, the mini player and scripts share one path. Text inputs keep their own
bindings (Space, ⌘←/→, ⌘↑/↓ edit text while the search field is focused).

### Settings file and secrets

`settings.json` (volume, EQ, Last.fm API key + user name, ListenBrainz user name, Discord
switch + id + `activity`, theme + text size, shuffle + repeat, `audio`, `lyrics`,
`now_playing`, `scrobble_rules`, `content` (language / country / filters / autoplay),
`network` (proxy), `appearance.language` (UI language: a catalog code or `system`),
main window and mini player frames, and `library`: privacy switches,
play-cache limit, music folders, the signed-in YouTube account's name / avatar /
`auth_user` / channel, `library.downloads` (download liked songs), `sponsorblock`, `together`
(incl. `relay_url`), `spotify`, `ai_filter`, `cast`, `custom_theme`, `player_bar`) lives in the platform config dir — macOS
`~/Library/Application Support/tunedesk/`, Linux `~/.config/tunedesk/`, Windows
`%APPDATA%\tunedesk\config\` — or `$MUSIC_CONFIG_DIR`. It is written 500 ms after the last
change and on quit, atomically (temp file + rename), mode 0600 in a 0700 directory. Missing
fields fall back to defaults, unknown ones are ignored, out-of-range values are clamped, and a
file that isn't valid JSON is moved to `settings.json.bad`; an unknown theme / repeat value
(from a newer version) falls back to the default instead.

The Last.fm shared secret and session key, the ListenBrainz token, the Paxsenix /
BetterLyrics / AI-translation API keys, the proxy password, the YouTube Music sign-in
cookie, the Spotify refresh token and the AI-filter API key go to the OS keychain
(`keyring` 4: macOS Keychain, Windows Credential Manager, Secret Service), service
`tunedesk`, one entry each. If the keychain fails (or `MUSIC_SECRET_STORE=file`), they are
stored in the `secrets` object of `settings.json` (still 0600) and moved back into the keychain
the next time it accepts a write; the Storage and Scrobbling sections show where secrets are kept. The keychain
is only read for accounts the settings say exist, on a worker thread (macOS may show an
"allow access" prompt — for unsigned dev builds it does so after every rebuild, because the
item's ACL is tied to the binary). Secrets are never logged.

### Environment hooks

| Variable | Effect |
|---|---|
| `MUSIC_QUERY=…` | pre-fill and run a search (`MUSIC_GPUI_QUERY` still works) |
| `MUSIC_AUTOOPEN=1` | play the first hit and open Now Playing (`MUSIC_GPUI_AUTOPLAY=1`: play only) |
| `MUSIC_ROUTE=library:albums` | open a page at startup (`home`, `explore`, `search:q`, `settings:eq`, `artist:ID`, `album:ID`, `playlist:ID`, `podcast:MPSP…`, `episode:MPED…`, `library:podcasts`, `browse:ID\|PARAMS`, `artist-items:ID\|PARAMS`, …) |
| `MUSIC_OPEN_SETTINGS=1\|<section>` | open the settings page (`eq`, `integrations`/`accounts`, `appearance`, `about`, …) |
| `MUSIC_DEMO_CLOCK=1` | demo clock instead of audio (also used automatically when `Player::new` fails) |
| `MUSIC_VOLUME=0.0` | volume for this session (muted real-device tests; ⌘↑/↓ and `@volume` still save) |
| `MUSIC_EQ_PRESET=Bass` | apply an EQ preset at startup (saved like a click) |
| `MUSIC_CONFIG_DIR=…` / `MUSIC_SECRET_STORE=file` | other settings dir / skip the keychain |
| `MUSIC_TRAY=0` | no menu-bar item |
| `MUSIC_LANG=de` / `MUSIC_SYSTEM_LANG=ja-JP,en` | UI language for this session (not saved) / pretend the system prefers these languages |
| `MUSIC_ONBOARDING=1\|0` | force / suppress the first-run guide (it is skipped automatically for `MUSIC_SCRIPT` / screenshot runs) |
| `MUSIC_UPDATE_REPO=owner/name` | enable the update check against that repo (testing; the default comes from `packaging/app.env`) |
| `MUSIC_NO_TRANSITIONS=1` | switch dynamic colours at once (no 400 ms cross-fade) |
| `MUSIC_BAR_SPECTRUM_DUMP=1` | print the player bar analyzer's band statistics once a second (tuning the Equalizer style) |
| `MUSIC_MALLOC=system` | keep macOS's default allocator behaviour (no re-exec with `MallocSpaceEfficient=1`, see `memory.rs`; for comparisons) |
| `MUSIC_TRACE=1` | log playback transitions, seeks, saves, route changes, artwork downloads; `MUSIC_TIMINGS=1`: renders/s + worst frame gap |
| `MUSIC_SCRIPT="5:@next,9:@seek=75,30:@quit"` | timed actions (any build) |
| `MUSIC_ON_TOP=1` | keep the main window above other apps and on every Space (CPU sampling while the machine is in use: macOS sends an occluded or off-Space window no frames) |
| `MUSIC_GPUI_SCREENSHOT="12:a.png,13:@settings,14:b.png,15:mini:m.png"` | offscreen shots + actions, quits after the last (feature `screenshot`; `mini:` shoots the mini player) |

Actions (`script.rs` has the table): input `@key=cmd-right` (a real keystroke through
GPUI's key dispatch), `@click=X;Y @rclick=X;Y @move=X;Y @mouse-back @mouse-forward`
(synthetic mouse events at window coordinates: hit-testing, hover, context menus);
playback `@play=N @next @prev @seek=S @toggle @open @close @volume=V`; navigation
`@nav=ROUTE @back @forward @search=Q`; queue `@play-next=N @enqueue=N @queue[=on|off]
@queue-move=A:B @queue-remove=N @queue-jump=N @queue-clear @shuffle=on|off
@repeat=off|all|one`; theming (`theme_script.rs`) `@custom-theme=FIELD:VALUE @theme-preset=ID
@theme-reset @theme-status @settings-export=PATH @settings-import=PATH @theme-export=PATH
@theme-import=PATH @import-confirm @profile=ID @profile-new=NAME @profile-duplicate
@profile-rename=NAME @profile-delete @profile-reset @profile-export=PATH @profile-import=PATH
@profile-status @auto-download-liked=on|off @remove-unliked=on|off
@download-all|download-cancel|download-remove=liked|library @download-status`;
windows/appearance `@mini[=on|off] @theme=dark|black|dynamic
@font-scale=1.125 @resize=W;H`; settings `@settings[=section] @close-settings
@eq=Preset|on|off @field=lastfm-key|lastfm-secret|lb-token|discord-id:VALUE
@lastfm-connect @lastfm-finish @lastfm-disconnect @lastfm-clear @lb-validate
@lb-disconnect @discord=on|off`; player `@layout=split|centered|lyrics|immersive|card|minimal|editorial|queue
@np-bg=blur|gradient|solid|glow @mini-style=theme|artwork
@lyrics-style=karaoke|apple|slide|glow|fade|none @lyrics-size=40 @lyrics-spacing=1.5
@lyrics-align=left|center|right @translate=on|off @romanize=SCRIPT:on|off @lyrics-reload
@kanji-dict[=remove|cancel]
@sleep=MIN|eot|off|5s @speed=1.25 @pitch=-2 @crossfade=6 @quality=low|normal|high
@normalize=on|off @skip-silence=on|off`; browse `@radio=N` (radio from search result N),
`@suggest` (print the search dropdown), `@proxy=KIND:HOST:PORT|off`, `@locale=de:DE`,
`@scroll=X;Y;DY`, `@dns=system|cloudflare|google|quad9|URL[;nofallback]`, `@dns-test`,
`@hide-videos[=off]`; modules `@modules=youtube|first|only`, `@module-install=PATH|URL`,
`@module-catalog=URL`, `@module-confirm`, `@module-cancel`, `@module-open=N|ID`,
`@module-field=KEY:VALUE`, `@module-save`, `@module-check=N`, `@module-update=N`,
`@module-enable=N:on|off`, `@module-remove=N`; integrations (`integrations/script.rs`) `@sb=on|off @sb-mode=auto|ask
@sb-category=CAT:on|off @sb-skip @sb-status @play-id=ID[:TITLE] @set=FIELD:VALUE
@together-host[=FILE] @together-join=LINK|file:PATH @together-leave @together-approve
@together-deny @together-kick=NAME @together-ban=NAME @together-allow=add|control|approval:on|off
@together-suggest=N @together-control=play|pause|next|prev|seek:S @together-status
@together-host-relay[=FILE] @together-transfer=NAME @ai-filter=on|off @ai-category=KEY:on|off
@ai-filter-status @ai-filter-clear @cast-scan @cast=NAME|IP:PORT[:NAME] @cast-stop[=off]
@cast-status
@import-file=PATH @spotify-playlist=URL @spotify-liked @spotify-playlists @import-accept=N
@import-save[=NAME] @import-cancel @import-status @export=PLAYLIST:FORMAT:PATH`;
recognition `@recognize` (microphone), `@recognize-file=PATH`,
`@recognize-cancel`, `@recognize-history=N`; `@onboarding[=STEP|off]`, `@onboarding-next`,
`@onboarding-back`, `@update-check` (`release_script.rs`); `@lang=CODE|system` (UI language,
saved); lyrics cards (`lyrics_card.rs`)
`@card-select=3;4;5 @card-open @card-style=aspect:story|bg:blur|album|dark|light|align:left|size:large|art:off
@card-save=PATH @card-copy @card-close`; measurement player bar (`bar_script.rs`) `@bar-style=wavy @bar-bg=gradient @bar-gradient=dir:diagonal @bar-hover=0.4 @bar-status`, `@bench-bar=N[:MS]` (only the bar marked dirty), `@bench=N[:MS]` (draws the main window N
times MS ms apart and prints the draw times: the UI's CPU per frame, also with the screen
locked); `@quit`.

Screenshot builds draw both windows every 200 ms and right before each shot: while the
screen is locked macOS delivers no display-link frames, so nothing would redraw (and
lazily started artwork downloads would never begin).

### Player bar redesign: progress styles, backgrounds

| Before | After |
|---|---|
| ![Before: playing, 1280 pt](docs/screenshots/bar-before-playing-1280.png) | ![After: playing, 1280 pt](docs/screenshots/bar-playing-1280.png) |
| ![Before: 880 pt](docs/screenshots/bar-before-playing-880.png) | ![After: 880 pt](docs/screenshots/bar-playing-880.png) |
| ![Before: casting, Together, SponsorBlock at 880 pt](docs/screenshots/bar-before-chips-cast-880.png) | ![After: casting, Together, SponsorBlock at 880 pt](docs/screenshots/bar-chips-cast-880.png) |
| ![Before: loading](docs/screenshots/bar-before-loading.png) | ![After: loading (status under the artist)](docs/screenshots/bar-loading-1280.png) |
| ![Before: idle](docs/screenshots/bar-before-idle-880.png) | ![After: idle](docs/screenshots/bar-idle-880.png) |

| | |
|---|---|
| ![Hover: thumb + time](docs/screenshots/bar-hover-1280.png) | ![Volume bubble](docs/screenshots/bar-volume-tooltip.png) |
| ![Thick](docs/screenshots/bar-style-thick.png) | ![Simple](docs/screenshots/bar-style-simple.png) |
| ![Wavy](docs/screenshots/bar-style-wavy.png) | ![Equalizer](docs/screenshots/bar-style-equalizer.png) |
| ![Artwork glow, playing](docs/screenshots/bar-bg-glow-playing.png) | ![Artwork glow, paused](docs/screenshots/bar-bg-glow-paused.png) |
| ![Dynamic gradient, playing](docs/screenshots/bar-bg-gradient-playing.png) | ![Dynamic gradient, paused (dimmed)](docs/screenshots/bar-bg-gradient-paused.png) |
| ![Gradient, dim theme, playing](docs/screenshots/bar-bg-gradient-dim-playing.png) | ![Gradient, dim theme, paused](docs/screenshots/bar-bg-gradient-dim-paused.png) |
| ![Gradient, black theme, playing](docs/screenshots/bar-bg-gradient-black-playing.png) | ![Gradient, black theme, paused](docs/screenshots/bar-bg-gradient-black-paused.png) |
| ![Gradient, diagonal, 90 %](docs/screenshots/bar-bg-gradient-diagonal-90.png) | ![Gradient, right to left](docs/screenshots/bar-bg-gradient-rtl.png) |
| ![Pulse](docs/screenshots/bar-bg-pulse.png) | ![Dynamic theme](docs/screenshots/bar-theme-dynamic.png) |
| ![Mini player: standard](docs/screenshots/bar-mini-standard.png) | ![Mini player: wavy + gradient](docs/screenshots/bar-mini-wavy-gradient.png) |
| ![Now Playing visualizer](docs/screenshots/bar-np-visualizer.png) | ![Settings → Appearance → Player bar](docs/screenshots/bar-settings.png) |

**Review of the old bar** (screenshots `bar-before-*`): the transport sat off-centre (the
chip area beside it was only on one side), the source tag and the Cast button floated in
the transport row, the progress and volume thumbs were large black discs with no
contrast on the dark surface and the volume thumb at 0 % covered the mute icon; icons
mixed 24 and 32 pt buttons (secondary ones below the 28 pt hit-target minimum); the time
showed only the length; status messages ("Loading…", errors) sat far from the track
they belong to; there was no lyrics button and the Now Playing chevron had no active
state; idle / loading artwork was an empty box; at 880 pt the progress bar shrank to
~220 pt and the right zone kept every control.

**Now** (`components/player_bar.rs`): three zones — track info (artwork with a
placeholder, title, artists, a status line in the warning / danger colour, like) ·
transport (32 / 36 / 40 pt buttons, toggles tinted in the accent, tags left and
SponsorBlock / Together chips right of it, both flanks equal so it stays centred) above
the progress row (elapsed, track, remaining / length — click toggles) · secondary
(lyrics, queue, sleep timer, Cast, mini player, Now Playing, mute + volume). The side
zones are equally wide (27 % of the window, 200–380 pt), so the transport is centred in
the window. `bar_layout` drops controls by priority instead of overlapping: Now
Playing, queue and mute always, then an active Cast session and an armed sleep timer,
the volume slider (92 / 80 / 64 pt), lyrics, Cast, sleep timer, mini player, the timer's
label (unit-tested 700–2400 pt). Progress and volume are one custom element
(`components/progress.rs`): hover reveals the thumb and a lighter "ghost" fill to the
pointer, a time bubble follows the pointer / scrub, click or drag seeks (`@bar-hover`
drives a real mouse move for tests); the volume shows a percentage bubble and changes
by scrolling over the slider or the mute button. Buttons keep gpui-component's keyboard
focus ring and tab order. The lyrics button opens Now Playing with lyrics in view
(designs without a lyrics pane show "Lyrics first" until it closes).

**Progress styles** (Settings → Appearance → Player bar; `player_bar.progress_style`):
Standard (4 pt, today's look), Thick (8 pt, pill thumb), Simple (2 pt hairline), Wavy
(the played part is a sine wave — 20 pt wavelength, half a wavelength per second, calm
when paused or dragging, gap and pill thumb, stop dot; after ArchiveTune's
`WavySliderExpressive`), Equalizer (one bar per 5 pt from the bar's own
`SpectrumAnalyzer`, played bars in the accent, peak caps optional, still seekable). The
analyzer mapping was tuned on real music with `MUSIC_BAR_SPECTRUM_DUMP=1` (band means
per group, % full / empty, level, once a second): with the defaults pop / rock averaged
0.35–0.7, never reached the top and the low bands sat ~0.25 above the mids; quiet piano
used only the middle octaves. Now floor −64 dB, ceiling −16 dB, +4 dB / octave
(`bar_fx::bar_spectrum_config`): 0.4–0.65 across all groups for the same songs.

**Background reacting to play / pause** (`player_bar.background_effect`, off by
default): **Artwork glow** (the blurred artwork plus the artwork's main colour behind the
track info, fading towards the transport), **Dynamic gradient** (two colours of the
artwork; direction left → right / right → left / diagonal, intensity 30–90 %, optional
slow drift), **Pulse** (a band of the artwork's colour behind the transport, width and
strength follow the music's loudness). Colours come from the dynamic theme's artwork
analysis (`palette.rs` seeds as tonal colours, or `AppPalette` for grey art) — computed
once per track, also when only the bar needs them; nothing is extracted twice.
**Dim when paused** (default on) fades to a dimmed, desaturated version; otherwise the
colour stays. Glow and the static gradient change in one 350 ms transition (new track,
play / pause) and then draw nothing on their own.

**Mini player**: the same track (click / drag to seek) and background
(`player_bar.mini_follows_bar`); **Now Playing**: an optional spectrum strip along the
bottom edge (`player_bar.np_visualizer`).

**Static by default, animations opt-in.** Default settings never ask for frames: the
fill and times follow the app's once-a-second update. Wavy (~17 fps), Equalizer (~20 fps),
Pulse (~17 fps), the animated gradient (~13 fps) and the visualizer wake on timers
(`bar_fx::EQ_INTERVAL` 40 ms, `ANIM_INTERVAL` 50 ms, `GRADIENT_INTERVAL` 66 ms) only
while music plays; paused they finish their fade / let the bars fall and stop. A wake-up
is only re-armed by a render, so a hidden or occluded window (no frames) and a bar
covered by Now Playing stop by themselves. The spectrum tap (`Player::set_spectrum_enabled`)
is on only while a visible view asks for it (`MusicApp::want_spectrum`), and the poll
turns it off 2 s after the last request.

**Why a bar frame is cheap.** GPUI draws the whole window from the root on every frame
and re-renders every view that isn't cached; a full redraw of the shell with a search
page costs ~5.3 ms (`@bench=300`), far too much for 20 fps. The bar is its own view
(`PlayerBar`); its button zones and everything above it (`MusicApp::render_shell`) are
`AppRegion`s — rendered from the app, re-rendered whenever the app notifies — embedded as
*cached* views while the bar is "live" (animating, fading or under the pointer), and
inline otherwise (so the static path is exactly the old one). A bar frame then redraws
only the bar's tracks and background: `@bench-bar` 1.0–1.7 ms; the rest is GPUI replaying
the cached scene, mostly its tab-stop map (`TabStopMap::replay`, profiled with `sample`).

**CPU** (release, macOS arm64, 1180 × 793, `MUSIC_ON_TOP=1`, "Instant Crush" seeked to
1:10, muted real device, Now Playing closed; `ps -o %cpu=` once a second for 12 s, avg;
several runs where noted — other agents were building and measuring on the same machine,
single samples up to ~100 % from that were discarded and re-run):

| Scenario | CPU % avg | renders / s |
|---|---|---|
| Before (main) | 0.7 / 1.4 / 2.6 / 1.3 / 1.2 | ~1 |
| Static default (Standard, no effect) | 1.6 / 0.7 / 2.7 / 0.4 / 1.1 | ~1 |
| Minimal / Fast profile | 0.4 | ~1 |
| Thick / Simple | 1.2 – 1.1 / 1.2 – 0.4 – 0.9 | ~1 |
| Artwork glow / dynamic gradient (static) | 0.9 – 1.4 / 1.2 – 1.2 | ~1 (+ one 350 ms fade) |
| Wavy | 5.9 / 6.5 / 4.8 | 16–18 |
| Equalizer | 6.6 / 6.9 / 4.3 | 18–22 |
| Pulse | 6.5 / 5.4 / 5.1 | 16–17 |
| Dynamic gradient, animated | 2.7 / 5.0 | 13–14 |
| Wavy / Equalizer / Pulse / animated gradient, paused | 0.0 / 0.2–0.4 / 0.3 / 0.0–0.3 | 0 |
| Mini player open: static / with Wavy (both animate) | 1.2 – 1.3 / 7.3 | 1 / 17 |
| Now Playing open (lyrics) without / with the visualizer | 8.5 – 9.6 – 7.7 / 10.4 – 10.8 | 46–50 |

Each animated frame costs ~3–4 ms of process CPU in total (draw ~1–1.7 ms plus presenting
the whole window), which is why the rates are capped at 13–22 fps rather than 30.

**Settings** (`player_bar` in settings.json, carried by profiles and theme files through
`portable::BAR_STYLE_KEYS`; the least-resource values are what the Minimal / Fast
profile sets, `PlayerBarConfig::least_resources`):

| Key | Default | Least resources |
|---|---|---|
| `progress_style` | `standard` | `simple` (`standard` / `thick` are static too; `wavy`, `equalizer` animate) |
| `peak_caps` | `true` | `false` |
| `background_effect` | `false` | `false` |
| `background` | `glow` | any (unused while off; `pulse` animates) |
| `gradient_direction` | `left-to-right` | any |
| `gradient_intensity` | `0.5` | any (0.1–1) |
| `animate_gradient` | `false` | `false` |
| `dim_when_paused` | `true` | any |
| `show_remaining` | `false` | any |
| `scroll_volume` | `true` | any |
| `mini_follows_bar` | `true` | any |
| `np_visualizer` | `false` | `false` |

**Script actions** (`bar_script.rs`): `@bar-style=standard|thick|simple|wavy|equalizer`,
`@bar-bg=glow|gradient|pulse|off`, `@bar-gradient=dir:…|intensity:0.7|animate:on|dim:off`,
`@bar-peaks`, `@bar-remaining`, `@bar-scroll-volume`, `@mini-follow`, `@np-viz` (`=on|off`),
`@bar-hover=F|off`, `@bar-volume-hover=F|off`, `@bar-status` (settings, `animates`, tap
state), `@bench-bar=N[:MS]`; env `MUSIC_BAR_SPECTRUM_DUMP=1`.

**Verified** (screenshot build, temporary `MUSIC_CONFIG_DIR`s, real device at
`MUSIC_VOLUME=0.0` or the demo clock): every style and background on screen (above),
playing / paused / loading / idle, 880 / 1280 / 1600 pt, dark / dim / black / dynamic,
SponsorBlock "Skip" chip, Together host chip and casting to `fake_cast_receiver` at 1280
and 880 pt (the Cast label clips, nothing overlaps), mini player in three looks, the Now
Playing strip; `@bar-status` showed the tap on while the Equalizer played, off within
2 s after Now Playing covered the bar and after the bars had fallen when paused;
hover / volume bubbles through real mouse-move dispatch; the profile and theme-file
tests carry `player_bar`; Minimal / Fast sets `least_resources()` (unit test).
**Not verified:** physical mouse dragging and trackpad scrolling over the volume (the same
handlers ran through dispatched events and scripts), keyboard-only operation beyond the
buttons' tab order (the tracks themselves aren't focusable), Windows / Linux, a 120 Hz
display, a light theme (the app has none; "dim" darkness is the lightest).

### Theming round: Customize theme, dynamic colours, profiles, downloads

| ![Customize theme](docs/screenshots/theme-customize.png) | ![Accent and surfaces](docs/screenshots/theme-accent.png) |
|---|---|
| ![Dynamic colours from the artwork](docs/screenshots/theme-dynamic-np.png) | ![Rosé preset (tint, round corners)](docs/screenshots/theme-rose-home.png) |
| ![High contrast, sharp, compact](docs/screenshots/theme-high-contrast.png) | ![Profiles](docs/screenshots/theme-profiles.png) |
| ![Download all: downloading 2 of 5](docs/screenshots/theme-download-all.png) | ![Import preview](docs/screenshots/theme-import-preview.png) |
| ![Backup & sync](docs/screenshots/theme-backup.png) | ![German layout check](docs/screenshots/theme-de-customize.png) |

* **Colour pipeline** (`theme.rs`, pure up to `apply`): settings + the current artwork's
  colours form a `Look`; `resolve` turns it into a `Resolved` (accent, secondary,
  tertiary, surface tint); `themed_colors` derives every gpui-component token from the
  stock dark theme (surfaces re-lit for darkness and tinted in hue — lightness is kept,
  so contrast doesn't change —, foreground / border lifts for contrast, accent-derived
  primary / ring / slider / progress / selection / list-active / link / caret tokens);
  `apply` sets them in one `Theme::update` plus `AppColors` / `AppPalette` and the radius
  / density atomics. **Defaults override nothing** (test `defaults_override_nothing`), so
  the stock look is unchanged until an option is touched.
* **Contrast**: every accent (custom, preset or from artwork) is raised in tone, hue and
  chroma kept, until it reaches the level's WCAG ratio against the background (4.5 / 5.5
  / 7:1); text on it is black or white, whichever contrasts more.
* **Dynamic colours** (`palette.rs`, after Material Color Utilities as used by ArchiveTune
  through MaterialKolor): the 544 px artwork is reduced to 64 px, bucketed into a 15-bit
  histogram and clustered by weighted k-means in Oklab (16 clusters); Material's `Score`
  ranks them (hue share ± 14° × 0.7 + chroma) and picks up to four seeds with distinct
  hues. Tonal palettes use OkLCh hue / chroma and **CIE L\* as the tone** (`solve` finds
  the colour of a hue / chroma at an exact tone, clipping chroma to sRGB), so tone pairs
  have a known contrast. Styles: vibrant (seed hue, chroma ≥ 0.17), muted, monochrome,
  true to artwork (seed tone, tertiary from the second seed); strength scales chroma.
  Runs on the artwork worker thread once per artwork (LRU of 64 by URL), only while the
  Dynamic theme is selected (switching to it extracts the current artwork's colours on
  demand). Cost ≈ 1 ms per artwork in release; the 400 ms cross-fade between songs
  re-applies the theme ~12 times (33 ms steps) in Oklab, then stops
  (`MUSIC_NO_TRANSITIONS=1` or the switch turns it off).
* **Customize theme** (Appearance): presets (Standard, Midnight, Rosé, Forest, High
  contrast, Artwork), Reset to default, theme export / import, live preview, accent
  swatches + hex + hue / saturation sliders, darkness (dim / standard / deep / pure
  black), surface tint, contrast, corners (gpui-component radius + `theme::radius` in our
  views), density (row heights of every song list, sidebar and settings paddings), font
  (system font list, searchable), dynamic style / strength / transitions.
* **Hard-coded colours audit**: theme cards (Appearance, welcome guide) render from
  `theme::preview`, the onboarding scrim from `AppPalette::scrim`, all list / card /
  dialog / sidebar / queue radii from `theme::radius`. Left on purpose: Now Playing
  designs and the lyrics card draw white / artwork colours on the blurred artwork, the
  schematic Now Playing previews in Appearance. The player bar uses theme tokens
  (`slider_bar`, `secondary`, `popover`, `AppColors::accent`), its backgrounds the dynamic
  palette / `AppPalette`, the mini player `AppPalette::on_artwork` on the artwork.
* **Settings export / import** (Settings → Backup & sync, `settings/portable.rs`): a
  header (`format`, `schema` = `settings::file::VERSION`, `app_version`, `exported_at`)
  around every setting except `LOCAL_ONLY` (secrets, window frames, the account, the
  "key is stored" markers). Import parses into a plan — new settings plus a per-section
  diff and notes (older schema → defaults, newer schema / unknown fields → listed and
  ignored, sections with wrong types → skipped, plain `settings.json` accepted, secrets in
  the file ignored) — shown in a preview; nothing changes before **Import**. Applied live
  (theme, audio, lyrics, content, proxy, EQ, language), then a "Restart now / Later"
  prompt for the settings page's own fields. **Theme files** (`<name>.tunedesk-theme.json`)
  carry only the look and replace only it.
* **Profiles** (`settings/profiles.rs`): see the user guide. Files `profiles/<id>.json`
  (settings-export format with a `profile` header, so they import as settings and any
  export imports as a profile) and `profiles/active.json`. Every settings save updates
  the active profile; built-ins ("Default", "Minimal / Fast") take edits too, show
  "modified" and have **Reset**, and can't be renamed or deleted (decision: no automatic
  "Custom" fork — one place to look, one reset). The first launch snapshots the current
  settings into Default, so nothing changes on screen. Switching applies live.
* **Downloads** (`components/download_header.rs`, ArchiveTune's `HeaderDownloadState`):
  state from the UI-thread download flags (cached per list and flag generation, so a
  5,000-song header costs nothing while scrolling); library pages now reload only when a
  download *finishes*, not on every progress step of a batch.

**Settings JSON** (new parts, defaults shown):

```json
"custom_theme": {
  "accent": "", "surface_tint": 0.0, "darkness": "standard", "contrast": "standard",
  "corners": "regular", "density": "regular", "font_family": "",
  "dynamic": { "style": "vibrant", "strength": 1.0, "transitions": true }
},
"library": { …, "downloads": { "auto_download_liked": false, "remove_when_unliked": false } }
```

`darkness`: dim | standard | deep | black; `contrast`: standard | medium | high;
`corners`: sharp | regular | round; `density`: compact | regular | comfortable;
`dynamic.style`: vibrant | muted | monochrome | content. Unknown values fall back to the
default. Profiles: `profiles/<id>.json` = `{"format": "tunedesk-settings", "schema": 1,
"app_version", "exported_at", "profile": {"id", "name"}, "settings": {"appearance",
"custom_theme", "now_playing", "lyrics"}}`, `profiles/active.json` = `{"active": "<id>"}`.

**Verified** (release `screenshot` build, temporary `MUSIC_CONFIG_DIR`, muted, display
awake): presets, accent / tint / darkness / contrast / corners / density / font applied
live and saved; dynamic theme on "Instant Crush" (accent hue 218°, 9.2:1 against the
background, tint at 35 %); liking five songs, **Download all** on Liked songs →
"Downloading 0 of 5" → all five stored, two at a time; settings export → change the look
→ import preview listing the 4 changes in 2 sections → Import restored them; theme export
/ import; profiles switch / create / reset; German / Russian / Japanese layouts of the new
groups. **Not verified**: the macOS save / open panels (the same code paths ran through
`@settings-export` / `@settings-import` / `@theme-*` / `@profile-*`), the playlist page's
"load remaining pages, then download" on a > 100-song playlist, Windows / Linux, remove
when unliked with a song mid-download.

**Measured, Default vs Minimal / Fast profile** (release, macOS arm64, display awake,
`MUSIC_ON_TOP=1`, "Instant Crush" seeked to 1:10, muted real device, other builds running
on the machine; seven interleaved runs each, avg of `ps -o %cpu=` over 12 s, `footprint`):

| | Default | Minimal / Fast |
|---|---|---|
| CPU, Now Playing open (word-synced lyrics) | 7.1–9.9 %, median 9.2 % | 0.2–1.8 %, median 1.2 % |
| CPU, Now Playing closed, playing | 1.8–3.3 %, median 2.6 % | 0.2–2.8 %, median 1.3 % |
| Footprint, Now Playing open | 186–279 MB, median 204 MB | 142–342 MB, median 243 MB |
| Footprint, closed | 150–213 MB, median 206 MB | 142–268 MB, median 199 MB |

The CPU saving is the lyrics: Minimal shows the big-artwork design (current and next line,
no per-word animation) on a solid background. Memory does not differ measurably: the
spread between runs (stream buffers, artwork cache, PoToken web view) is far larger than
what the switched-off features hold (a 320 px blurred backdrop and three 64 px glow
images per recent track, the colour seeds); the artwork cache size (48 MiB) is not a
setting. One Default run without lyrics on screen measured 1.2 %.

### Release round: recognition, onboarding, packaging

| ![Recognition result](docs/screenshots/release-recognition-result.png) | ![From the history](docs/screenshots/release-recognition-history.png) |
|---|---|
| ![Listening page](docs/screenshots/release-recognition-idle.png) | ![Identifying](docs/screenshots/release-recognition-identifying.png) |
| ![Onboarding: welcome](docs/screenshots/release-onboarding-welcome.png) | ![Onboarding: language & region](docs/screenshots/release-onboarding-content.png) |
| ![Onboarding: theme](docs/screenshots/release-onboarding-theme.png) | ![Onboarding: account](docs/screenshots/release-onboarding-account.png) |
| ![Onboarding: music folders](docs/screenshots/release-onboarding-local-files.png) | ![Onboarding: privacy & previews](docs/screenshots/release-onboarding-privacy.png) |
| ![About with update check](docs/screenshots/release-about-update.png) | ![App icon](docs/screenshots/release-app-icon.png) |

* **Recognition** (sidebar "Recognize", ⇧⌘R, View → Recognize Song): big listen button
  (stop while busy), level ring + progress while recording 10 s, "Identifying…", then the
  result (art, title, artist, album · year · genre · label, Play / Add to queue / Radio /
  Search, Shazam / Apple Music / Spotify / YouTube links — Android deep links converted to
  web URLs) and the top five YouTube Music matches as track rows; history below (click
  to show again, × to remove, Clear with confirmation). Permission: AVFoundation status /
  prompt before recording on macOS; denied, restricted or pure-silence input shows "Open
  privacy settings". The session survives leaving the page (global entity).
* **Onboarding**: 6 steps over a scrim (Esc / "Skip setup" closes it for good, `onboarded`
  in settings.json; files from older versions count as onboarded). Reopen: Settings →
  About → "Show welcome guide".
* **Update check**: only with `UPDATE_REPO` (or `MUSIC_UPDATE_REPO`); once per launch +
  "Check now"; a newer tag shows a notification and a "Download…" link to the release
  page.

**Verified:** signature generation is bit-identical to a Java transliteration of
ArchiveTune's Kotlin generator on a synthetic fixture (golden file, unit test), plus
encode/decode round trip, request building and response parsing against a fixture.
**Live** (`examples/recognize.rs`, decoding YouTube streams, nothing played): "Get Lucky"
and "Take On Me" were recognized correctly by the live endpoint. In the app (screenshot
build, demo clock): `@recognize-file` with 12 s WAV clips → result, YouTube Music matches,
history of two entries, `@recognize-history=1`; clicking Play on the result played the
match. Onboarding all six steps + finish (`onboarded: true` written). Update check against
`zed-industries/zed` → "Version 1.25.3 is available". **Bundle:** `scripts/bundle-macos.sh`
→ `TuneDesk.app` (34 MB, ad-hoc signed, `codesign --verify --strict` passes, Info.plist
linted) + 15 MB DMG; launched with `open --env …` (temp config, `MUSIC_VOLUME=0.0`):
menu-bar item created, media controls started without error, search → play (real device,
muted) → next → pause/resume → mini player on/off → Recognition page → quit.

**Not verified:** a real microphone recording and the macOS permission prompt (not
triggered on purpose — the screen was locked; the capture code path is the example's
`--mic`, untested), the Windows resource embedding and installer, the Linux tarball /
AppImage and `.desktop` integration, the CI workflows (YAML parsed only), notarization,
clicking the menu-bar item / media keys from the bundle (no input injection possible).

### Player UI round: verification

| ![Split, karaoke](docs/screenshots/player-np-split-karaoke.png) | ![Big artwork](docs/screenshots/player-np-big-artwork.png) |
|---|---|
| ![Lyrics first](docs/screenshots/player-np-lyrics-first.png) | ![Glow background](docs/screenshots/player-np-glow-background.png) |
| ![Romanization, fade, centered](docs/screenshots/player-np-romanization-fade-center.png) | ![Line-synced, paused by the sleep timer](docs/screenshots/player-np-line-synced-sleep-paused.png) |
| ![Gradient background](docs/screenshots/player-np-gradient.png) | ![Bar: sleep timer, queue after a gapless advance](docs/screenshots/player-bar-sleep-gapless.png) |
| ![Settings: Playback](docs/screenshots/player-settings-playback.png) | ![Settings: Lyrics](docs/screenshots/player-settings-lyrics.png) |
| ![Settings: Appearance](docs/screenshots/player-settings-appearance.png) | ![Settings: Scrobbling & Discord](docs/screenshots/player-settings-integrations.png) |
| ![Mini player, artwork style](docs/screenshots/player-mini-artwork.png) | |

All through `MUSIC_GPUI_SCREENSHOT` / `MUSIC_SCRIPT` with a temporary `MUSIC_CONFIG_DIR`
(screen locked the whole session). **Real device, muted** (`MUSIC_VOLUME=0.0`): a ~60 s
preview ran out and the engine switched by itself (`Playing → Playing`, no `Loading`
or `Ended`); the UI logged `engine advanced to l4Gb0ViuJGw (advance #1); queue next was
l4Gb0ViuJGw` and Now Playing / up next / lyrics followed; crossfade 4 s, speed 1.5×
(position advanced 5 s per ~3.3 s), normalization off and quality high applied while
playing without errors; `@sleep=5s` paused 5 s later. **Demo clock:** gapless advance
to the next queue item, sleep timer countdown in the bar, "end of track" stopping paused
at the start of the next song, all six lyric styles, the three layouts, four
backgrounds, alignment, romanization (YOASOBI, word-synced TTML), mini player artwork
style. Lyrics came from BetterLyrics (Portato) and Binimum, a handful of tracks only.

**Not verified:** a successful translation (Google's free endpoint answered with an
error from this IP — the failure / Retry path is what the screenshots show; the AI path
had no key), Paxsenix / BetterLyrics keys against the live APIs, a provider result with
background vocals or duets on screen (covered by a unit test on a TTML fixture), the
output-device menu with a second device, Discord activity in a running Discord,
scrobbles with custom rules against the real services (tracker unit-tested), auto-skip
after a real load error, physical input. **CPU not measured:** with the screen locked
macOS delivers no display-link frames (`MUSIC_TIMINGS` prints nothing; 0 % CPU sampled).
Frames are still only requested while Now Playing is visible and something moves; the
glow background adds no frames while paused, the mini player none at all. Backdrop blur
+ palette + glow lights: ~9.5 ms per track on a worker thread.

### Lyrics round: verification

`examples/lyrics.rs` against the live providers (no API keys; Paxsenix skipped), after the
fixes. Cells: sync kind, format, lines; "credits" = credit lines the filter removed.

| Provider | Instant Crush (EN) | Hype Boy (KR) | アイドル (JP) | MALAMENTE (ES) | Time (instrumental) | 晴天 (ZH) | Double Talk (obscure) |
|---|---|---|---|---|---|---|---|
| BetterLyrics | Line TTML 82 | Word TTML 66, 1 bg | Line LRC 75 | Word TTML 57, 25 bg | Line LRC 20 (a fan poem) | Line LRC 50 | — |
| BetterLyrics Portato | Word QRC 76 (credits) | Word QRC 68 (title + credits) | Word QRC 75 | Word QRC 60 (title line was a "singer") | "纯音乐" → instrumental | Word QRC 50 | — |
| YouLyPlus | Word TTML 76 (songwriter line) | Word LRC 66 | Word TTML 76 | Word LRC 57 | — | — | — (11.5 s → stops at the 404) |
| Binimum | Word TTML 84 | Word TTML 66, 1 bg | Word TTML 74 | Word TTML 57, 25 bg | — | — | Line TTML 12 |
| LRCLIB | Line 80 | Line 49 | Line 76 | Line 57 (≈ 7 s early) | — | Line 50 (header + 词/曲) | Line 12 |
| KuGou | Line 76 (credits) | Line 68 | Line 75 | Line 60 (title line, with the query) | "纯音乐" → instrumental | Line 50 | — |
| Megalobiz | — | — | — | — | — | — | — (site rebuilt: search 301 → 404/410) |
| SimpMusic | Word LRC 82 | Word LRC 66 | Word LRC 76 | Word LRC 57 | Line 42: another song | Word LRC 56 → 50 (producer names, with the query) | — |
| Unison | Word TTML 78 | — | Word TTML 74 | — | — | Word TTML 49 | — |
| YouTube Music | Plain 100 | Plain 68 | Plain 88 | Plain 65 | — | Plain 48 | Plain 15 |
| **Service pick** | Portato (Word) | BetterLyrics (Word) | Portato / YouLyPlus (Word) | BetterLyrics (Word) | **no lyrics** (was the poem) | Portato (Word) | Binimum (Line) |

Google Translate (one live request, spaced): still refused for this IP — a 302 to
`google.com/sorry` that answers 429 without `Retry-After`; the translator now reports
"Google Translate rate-limited this connection; try again in 2 min" and sends nothing until
then. In the app (release build, temp `MUSIC_CONFIG_DIR`, demo clock — no audio): an
OpenAI-compatible fake server got one request for アイドル's 74 lines; after a restart the
translation came from the library (still one request in total). `@kanji-dict` downloaded
and verified the real IPADIC in ~1.6 s and loaded it; at the next start it was mapped in
0.45 s after launch; `@kanji-dict=remove` deleted it. Release binary 40,020,928 B
(+236,192 B for lindera without a dictionary and this round's code). **Not verified:** the
Settings → Lyrics dictionary row and progress bar on screen (rendered, not looked at),
in-app cancel of a running download (unit-tested; the real download finished first), a
successful Google translation, Paxsenix / BetterLyrics keys.

### Now Playing round: CPU, designs, lyrics cards

| ![Split (default)](docs/screenshots/np-split.png) | ![Immersive](docs/screenshots/np-immersive.png) |
|---|---|
| ![Card](docs/screenshots/np-card.png) | ![Minimal](docs/screenshots/np-minimal.png) |
| ![Editorial](docs/screenshots/np-editorial.png) | ![With queue](docs/screenshots/np-queue.png) |
| ![Big artwork](docs/screenshots/np-centered.png) | ![Lyrics first](docs/screenshots/np-lyrics.png) |
| ![Split, glow background, Apple style](docs/screenshots/np-split-glow-apple.png) | ![Appearance: eight designs](docs/screenshots/np-settings-appearance.png) |
| ![Selecting lines for a card](docs/screenshots/np-lyrics-select.png) | ![Card dialog](docs/screenshots/np-card-dialog.png) |
| ![Saved card, square](docs/screenshots/np-card-square.png) | ![Saved card, story, light, left](docs/screenshots/np-card-story-light.png) |
| ![Bar at 880 pt: casting + Together](docs/screenshots/np-bar-narrow-cast.png) | |

**CPU while lyrics animate.** Profiled first (`sample` on a release build with line
tables): of ~2 ms per frame, the lyrics list's per-line layout (every line of the song a
flex row with a measured text element and its own hitbox, re-shaped twice), per-glyph
bounds-tree inserts when painting (sweep slices and glow halos paint a glyph up to 12×),
and the gpui-component buttons of the header / toolbar / like button dominated; frames
were requested continuously while playing even when nothing moved. Changes:

* the synced list does its own layout (`np_lyrics.rs`): row heights measured once per
  width / size / spacing, only rows on screen are built, at fixed sizes, one hitbox for
  the list; `KaraokeLine` paints each line in one layer and reuses its layout-pass shaping;
* frames only while a word is being swept or a tween runs; otherwise `Frame::wake_at`
  re-renders at the next word / line boundary, the next second of the time label, at 30
  fps for the glow background, or per pixel of the Minimal fill;
* the header and lyrics toolbar are cached `Snapshot` views, the player column / strip
  cached regions (`np_region.rs`) — rebuilt only when what they show changes.

Real CPU, `ps -o %cpu=` once a second for 12 s, seeked to 1:10 of "Instant Crush"
(word-synced, singing), release, real audio muted, window 1180×793 kept visible with
`MUSIC_ON_TOP=1`, before = main at `faca533` + the measurement hooks, measured back to
back (min / avg / max):

| Design · background · style | before | after |
|---|---|---|
| Split · blur · Karaoke (default) | 14.9 / 21.9 / 25.5 | 2.3 / 11.6 / 14.4 (second run 12.3) |
| Split · glow · Karaoke | 14.7 / 24.0 / 29.6 | 7.5 / 13.8 / 17.0 ¹ |
| Split · blur · Apple | 22.8 / 27.7 / 31.7 | 7.5 / 13.5 / 19.7 ¹ |
| Split · blur · Glow | 18.2 / 24.3 / 28.7 | 4.9 / 12.8 / 16.4 ¹ |
| Lyrics first · glow · Apple (heaviest) | 14.9 / 24.4 / 30.2 | 6.9 / 13.0 / 15.7 |
| Big artwork | 11.7 / 12.7 / 14.0 at 61 r/s | 1.2 / 1.6 / 2.6 at 6 r/s |
| Immersive / Card / Minimal / Editorial / With queue | — | 1.8 / 1.5 / 0.4 / 4.5 / 12.3 ² |
| Now Playing closed, playing (browse page, bar) | 3.9 / 5.2 / 7.0 | 3.7 / 5.8 / 9.0 |

¹ measured before the cached player column (a further ~1–2 points off). ² the With queue
run included loading the queue's artwork; its frames are as rare as the other designs'
(16 renders/s in that window, ~6 when settled). Renders while words are swept: 55–60/s,
worst gap 17.7 ms (unchanged); between words and lines the screen idles. The same
scenarios measured earlier in the day under heavy machine load gave 26–28 % before.
Casting to `examples/fake_cast_receiver.rs` with Now Playing closed: 5 renders/s
(poll-driven), 2–8 % CPU including the stream proxy — the bar doesn't animate.

UI cost per frame without presenting (`@bench=600`, display-independent), before → after:
split · blur · Karaoke 2.28 → 0.91 ms, split · glow 3.11 → 0.90, Apple 3.20 → 1.07, Glow
style 3.60 → 1.44, lyrics first · glow · Apple 3.41 → 1.43, big artwork 1.47 → 0.97; the
new designs 0.88–1.35 ms (drawn only on wake-ups). Physical footprint (`footprint`) was the same in both builds (200–240 MB in these runs).

**Verified** (temporary `MUSIC_CONFIG_DIR`, `MUSIC_VOLUME=0.0`): every design and the
appearance cards via `@layout` / screenshots; lyrics cards end to end by script —
`@card-select=10;11;12` selected the expected three lines, the dialog previewed them,
`@card-style` changed shape / background / alignment, `@card-save` wrote 1080×1080 and
1080×1920 PNGs, `@card-copy` put a 292 KB PNG on the macOS clipboard
(`osascript -e 'clipboard info'`); renderer unit tests (sizes per aspect, ink in the text
band, PNG signature, wrapping keeps every word, auto-fit at all sizes, CJK breaks between
characters and kana drawn through font fallback, dark text on the light style). Esc via
`@key=escape`: with the design menu open it closed only the menu (Now Playing stayed),
the next Esc closed Now Playing; a track row's context menu (`@rclick`) closed on Esc
without navigating back, the next Esc went back. Player bar at 880 pt while casting and
hosting Together: title truncated, chips collapsed, Cast icon kept, no overlap.

**Not verified:** "Save image…" through the real file dialog (the same write path ran
via `@card-save`), pasting the copied card into another app, physical mouse / keyboard,
Linux / Windows fonts for cards (fallback depends on installed fonts), the cached regions
under a 120 Hz display, CPU of the final build with the screen awake (the screen locked
before the last run; the final code equals the measured one plus the bar and Esc
changes). Not measured: the Lyrics-first strip region separately.

### Measurements (app shell round)

macOS arm64, release (thin LTO), demo clock unless noted, sampled 10 s after launch
(`vmmap --summary` physical footprint = Activity Monitor "Memory", plus `ps` RSS);
"before" is the previous commit built from the same tree state, measured back to back:

| Scenario | Footprint before → after | RSS before → after |
|---|---|---|
| Idle, Home, nothing playing | 43.0 → 47.2 MiB (46.6 with `MUSIC_TRAY=0`) | 76 → 103 MiB |
| Search results + playing, bar updating | 50.2 → 54.3 MiB | 87 → 114 MiB |
| Same with real audio (muted) | — | 93 → 120 MiB |

The private memory cost of the shell is ~4 MiB (menu-bar item ~0.6 MiB); the larger RSS
step is mostly shared, clean pages (more of the 23.8 MiB binary, AppKit menus / status
item). Release binary 24,952,976 B (23.8 MiB, +1.2 MiB). Incremental release rebuild of
the crate: ~1.5 min; the `screenshot` build is separate (~3 min incremental).

**Not measured this round: CPU.** The machine's screen was locked for the whole session
(`CGSSessionScreenIsLocked`), so macOS delivered no display-link frames: `MUSIC_TIMINGS`
printed no renders, and every scenario sampled 0.0 % CPU (with audio too, i.e. the
process was throttled). Animation CPU therefore can't be compared to the previous
17–20 % from here. The Now Playing render path is unchanged (the shell isn't built while
the screen covers the window, frames are only requested while something moves), and the
browse UI renders only on the 250 ms poll or input; re-run the scenarios below with an
unlocked screen to confirm.

**Verified (synthetic input through GPUI's normal dispatch, `@key` / `@click` / `@rclick`):**
Space play/pause (and Space typed into the focused search field is text, not a toggle),
⌘→/⌘← next/previous-restart, ⌘↑/⌘↓ volume, ⌘L/Esc Now Playing, ⌘[ back, mouse back /
forward buttons, ⌥⌘Q queue panel, ⇧⌘M mini player open/close (repeatedly), ⌘1 Home,
⌘F + typing → search page, ⌘S/⌘R saved to settings, sidebar click, track-row click plays,
right click opens the context menu (per-artist "Go to …" entries). Queue move/remove/
clear, shuffle off restoring the order, repeat persisted, window frame restored without
drift. **Real device (muted):** search → autoplay, EQ preset changes while playing, seek
0→30→0 s, previous restarts, Discord switch + id saved, Last.fm key/secret flow, preview
end (`Ended` at 59.9 s) → next track loads and plays.

**Not verified:** physical mouse/keyboard and drag-and-drop in the queue panel (no
synthetic drag), clicking the menu-bar item (it is created; its commands go through the
same methods as the bar), the macOS menu bar itself (same actions as the shortcuts),
⌘, via script (a comma can't be written in `MUSIC_SCRIPT`), the mini player staying above
other apps (`setHidesOnDeactivate:` is called; not observable with a locked screen),
Linux/Windows.

### Measurements (previous round: queue, media controls, EQ, scrobbling)

macOS arm64, 60 Hz, release (thin LTO), `ps -o %cpu=,rss=` once a second for 20 s after an 8 s
warm-up, `MUSIC_QUERY="daft punk instant crush"`:

| Scenario | CPU % min / avg / max | RSS MiB min / avg / max |
|---|---|---|
| Now Playing animating, demo clock (`MUSIC_AUTOOPEN=1 MUSIC_DEMO_CLOCK=1`) | 11.2 / 17.7 / 23.1 | 110.9 / 112.4 / 113.4 |
| same, second run | 11.1 / 19.0 / 23.2 | 110.5 / 111.4 / 112.2 |
| Now Playing animating, **real audio, muted** (`MUSIC_VOLUME=0.0`) | 14.6 / 19.8 / 22.9 | 115.4 / 115.5 / 115.5 |
| Browse UI, real audio playing (muted), bar updating | 0.7 / 2.0 / 3.4 | 111.2 |
| Idle, nothing playing, window focused (caret blinking) | 0.2 / 0.7 / 1.5 | 87.5 / 87.6 / 87.6 |

61–62 renders/s with a worst frame gap of 17.7 ms while animating — unchanged from the lyrics
round (17.3 % / 16.8 % CPU then). RSS while playing is ~6–10 MiB higher than that round's
~102–106 MiB (media controls with artwork, scrobble/Discord/settings code); idle is lower
(88 vs 99 MiB). Release binary 23,661,248 B (22.6 MiB, +0.8 MiB). Incremental release rebuild
after touching the crate: 32–42 s here. The dependency graph grew by 17 crates (keyring + its
stores, `directories`).

**Real-device check** (`Player::new` works on this machine; always `MUSIC_VOLUME=0.0`, nothing
audible): position advances, seek 0→30 s and back works, rapid EQ changes (Bass → Treble → off
→ Loudness) during playback cause no errors, "previous" at 5 s restarts, and when a ~60 s
preview reaches its end (`Ended` at 59.9 s) the next result loads and plays. Seeking *past* a
preview's downloaded data snaps back to ~0 s (music-core decoder behaviour; not handled in the UI).

**Not verified:** real mouse/keyboard input (all interaction went through the script actions,
which call the same handlers as the buttons), OS media keys / Control Center (code path compiles
and registers; no key events could be injected), a complete Last.fm auth or actual scrobbles /
ListenBrainz listens (only the error paths with bogus credentials hit the real APIs), Discord
Rich Presence against a running Discord, Linux and Windows (including the HWND and keychain
backends).

### History: benchmark rounds (condensed)

**Round 1 — search spike.** Search, virtual list, artwork, bar with progress slider, built from
gpui-component almost without custom drawing (one ~800-line file, two trivial compile errors).
Crates: `gpui-component =0.8.1`, `gpui-kit-assets =0.8.1` and the GPUI snapshot it pins,
`gpui-pre =0.3.10` + `gpui-pre-platform =0.3.10` (aliased `gpui` / `gpui_platform`; features
`font-kit` — needed for glyphs on macOS —, `runtime_shaders` — no full Xcode —, `wayland`,
`x11`). Measured: 20.9 MiB binary, ≈10–11 min clean build (427 crates), idle RSS ≈88 MiB,
≈102 MiB with 20 results, 0.3–1 % CPU idle. Hard: confusing crate topology (the platform crate
and `runtime_shaders` only show up in `gpui-base`'s manifest), no examples in the tarballs,
build cost, `Slider` exposes no dragging state, `render_to_image` needs `test-support`.

**Round 2 — Now Playing & lyrics.** Slide-up screen with blurred backdrop (CPU blur, 4.5–14 ms
per track, cross-fade), artwork scaled with GPUI's `with_spring`, play/pause icon morph via
`svg().with_transformation`, LRCLIB lyrics as a custom `KaraokeLine` element (`shape_text` +
`paint_glyph` + `with_content_mask`: partial-word sweep with a 10-slice soft edge that survives
wrapping, sung words lift, inactive lines painted at 0.9 scale), Apple-Music-style staggered
scrolling from `ScrollHandle` child bounds, `SmoothClock` extrapolating the 250 ms polls, frames
requested only while something moves, `inactive_frame_interval: None` to avoid GPUI's 30 fps
throttle for unfocused windows. Measured 17.3 / 16.8 % avg CPU at 61.6 renders/s (worst gap
17.8 ms), ~102–106 MiB, 0–1 % idle. Learned: blocking work on GPUI's background executor
starves (artwork downloads delayed LRCLIB by >15 s → dedicated threads); value tweens readable
from state need a helper (`anim.rs`). Missing for an ArchiveTune-grade port: transforms on any
subtree, layer opacity, blur/backdrop filters, alpha/gradient masks, gradient text, shared
element transitions, a pausable animation clock, a documented non-blocking HTTP/image pipeline.
Screenshots: [karaoke sweep](docs/screenshots/gpui-karaoke-sweep-zoom.png),
[backdrop](docs/screenshots/gpui-now-playing-backdrop.png),
[open transition](docs/screenshots/gpui-now-playing-transition.png),
[pause/close](docs/screenshots/gpui-now-playing-pause-close.png).

**Round 3 — this one** (queue, media controls, EQ, scrobbling, Discord, persistence, volume).
Easy: `Sheet`, `Switch`, vertical `Slider`, masked `Input` with a reveal toggle and `Button`
selected states covered the settings UI; `Window` implements `raw-window-handle`; extra Lucide
icons can be embedded one by one with `icon_assets!`. Gotchas: `InputState::set_value` doesn't
emit `InputEvent::Change` (programmatic edits must be synced by hand), a vertical `Slider` must be
sized itself (`.h_full()` inside a sized box), not just its parent, and gpui-component's sheet leaves a title-bar gap
unless `Theme::sheet.margin_top` is zeroed. No changes to `music-core`.
