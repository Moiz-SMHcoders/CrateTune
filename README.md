# music-macos — native desktop YouTube Music client

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
shortcuts, settings, sign-in, limitations) and [CHANGELOG.md](CHANGELOG.md).

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

**Packages** (`target/bundle/`):

| Platform | Command | Output |
|---|---|---|
| macOS | `scripts/bundle-macos.sh [--no-build] [--no-dmg]` | `Music.app` (Info.plist with bundle id, version from Cargo, `LSMinimumSystemVersion` 11.0, `NSMicrophoneUsageDescription`, high-res, music category; ad-hoc signed, or Developer ID + hardened runtime with `MACOS_SIGN_IDENTITY`) and `music-macos-<ver>-macos-<arch>.dmg` |
| Linux | `scripts/bundle-linux.sh [--no-build] [--appimage]` | `.tar.gz` with `bin/`, `.desktop`, hicolor icons, AppStream metainfo, `install.sh`; AppImage via `appimagetool` |
| Windows | `pwsh scripts/bundle-windows.ps1` | `.zip` and an Inno Setup 6 per-user installer (`packaging/windows/installer.iss`); the `.exe` gets its icon + version resource from `build.rs` (`embed-resource`, native Windows builds). An MSI could be made with `cargo-wix` instead. |

CI (`.github/workflows/ci.yml`) runs fmt, clippy (also with `--features screenshot`) and
tests on macOS, Ubuntu and Windows; `release.yml` builds every package on a `v*` tag and
attaches them to a draft release.

**App identity.** No final name has been chosen: "Music" and the `music-macos` ids are
working names. They live in **one file, `packaging/app.env`** (`APP_NAME`, `APP_ID`,
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
  rodio/cpal only mix and output. Engine features:
  * plays `Track::local_path` files (ReplayGain / R128 track gain from tags) or streams
    YouTube progressively (`StreamBuffer`: playback starts with the first bytes, seeks
    into the undownloaded part wait up to 10 s; an expired URL is re-resolved once);
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
* `together` — Music Together on the LAN: ArchiveTune's JSON protocol over WebSockets
  (`tungstenite` on std threads), `TogetherServer` / `TogetherClient`, heartbeat clock sync,
  `GuestSync` (drift / play-state planner), join links.
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
    MediaControlsConfig::new("music_macos", "Music"),
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
  playback usually starts in 2–5 s anyway.

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
  is dispatched to the main thread, so never call the resolver from it). No PoToken
  provider on Linux / Windows yet; they use the TV client.
* `WebResolver::prewarm()` does the slow one-time work at launch; the app calls it.
* Settings → Playback → **YouTube streams**: Automatic (TV, then web + PoToken, then
  preview), TV client, Web + PoToken, Previews only.

```sh
cargo run --release -p music-core --example youtube -- tv  "daft punk instant crush"  # resolve + full download + decode
cargo run --release -p music-core --example youtube -- web khnokW3Mw24                # same with a PoToken (macOS)
cargo run --release -p music-core --example youtube_play -- tv "daft punk"            # muted real-device playback
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
                     artwork (shared cache), sidebar, player_bar, queue_panel
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
  theme.rs           dark / pure black / dynamic accent, font scale, AppColors
  mini_player.rs     always-on-top mini player window
  tray.rs            menu-bar item (macOS)
  now_playing.rs     Now Playing: layouts, backgrounds, transport, lyrics pane
  lyrics.rs          LyricsService wrapper: fetch, prepare (duets, background vocals,
                     romanization), translation cache, cache hook
  karaoke.rs, anim.rs, backdrop.rs (blur, palette, glow lights), clock.rs (engine +
                     demo clock + smooth position)
  settings/prefs.rs  audio / lyrics / Now Playing / Discord activity / scrobble-rule
                     model; settings/prefs_ui.rs: their panel widgets + methods
  module_service.rs  provider modules: the shared `Modules` store, the module-first resolver
                     (player + downloads), keychain hook for secret module settings;
                     settings/modules.rs: the Modules section
  integrations/      SponsorBlock, Music Together, import / export state + script actions
                     (config.rs: their settings; settings/integrations_ui.rs: sections;
                     pages/together.rs, pages/import.rs)
  services.rs, secrets.rs, script.rs, util.rs, icons.rs
  recognize.rs       recognition session (Recognizer global entity: permission → capture →
                     signature → request → history → YouTube Music search);
                     pages/recognition.rs renders it; mic_permission.rs (macOS AVFoundation)
  settings/onboarding.rs  first-run guide overlay (reuses the settings sections)
  update_check.rs    optional GitHub release check; identity.rs (name / ids from
                     packaging/app.env via build.rs); release_script.rs (script actions)
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
* **Renaming the app:** edit `packaging/app.env` only (see Install / build).

### Features

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
  SponsorBlock, Music Together, Spotify import, About (version, GPL-3.0 notice, ArchiveTune and Hummingbird credits). The old sheet
  is gone; the Now Playing gear opens the page.
* **Themes** (`theme.rs`): dark (gpui-component's), pure black, dynamic (accent = the most
  common vibrant hue of the current artwork, lightened to ≥ 4.5:1 against the
  background, black or white text on it; grey art keeps the previous accent), text size
  88–138 %. Applied to gpui-component's `Theme` and our `AppColors`.
* **Artwork cache** (`components/artwork.rs`): one cache for every view and both windows,
  48 MiB LRU of encoded bytes (evicting also drops GPUI's decoded copy), 4 dedicated
  fetch threads taking the newest request first, failed URLs retried after 30 s. Now
  Playing's 544 px art comes from it too (blur + accent for the last 8 tracks).
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
* **Now Playing**: three layouts — split (artwork + controls | lyrics), big artwork
  (centered, the current and next lyric line below the controls), lyrics first (lyrics
  across the screen, artwork / title / progress / transport in a bottom strip) — and four
  backgrounds: blurred artwork, gradient between two artwork colours, solid colour,
  animated glow (three soft lights in the artwork's colours that drift only while
  playing; pre-rendered images, since GPUI has no radial gradients). Colours cross-fade
  between tracks. Layout and background are also in the header's layout menu. Mini
  player style: theme or blurred artwork.
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
  usage and clear, optional "cache played songs", download folder + usage, **backup export /
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
  YouTube, unless the source is "Modules only". Local files never go through modules. Streams
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
  session with a message. LAN only: there is no relay server.
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
progress bar by a guest (it is overridden by the host's state), a host transfer (not
implemented), physical input.

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
| Esc | close Now Playing, else the queue panel, else back |
| ⌘H, ⌥⌘H, ⌘M, ⌘Q | hide, hide others, minimize, quit |

The menu bar (Music, Edit, View, Controls, Window) carries the same actions with their
shortcuts. All shell actions are global listeners that defer to the main window's app,
so keys, menus, the mini player and scripts share one path. Text inputs keep their own
bindings (Space, ⌘←/→, ⌘↑/↓ edit text while the search field is focused).

### Settings file and secrets

`settings.json` (volume, EQ, Last.fm API key + user name, ListenBrainz user name, Discord
switch + id + `activity`, theme + text size, shuffle + repeat, `audio`, `lyrics`,
`now_playing`, `scrobble_rules`, `content` (language / country / filters / autoplay),
`network` (proxy), main window and mini player frames, and `library`: privacy switches,
play-cache limit, music folders, the signed-in YouTube account's name / avatar /
`auth_user` / channel, `sponsorblock`, `together`, `spotify`) lives in the platform config dir — macOS
`~/Library/Application Support/music-macos/`, Linux `~/.config/music-macos/`, Windows
`%APPDATA%\music-macos\config\` — or `$MUSIC_CONFIG_DIR`. It is written 500 ms after the last
change and on quit, atomically (temp file + rename), mode 0600 in a 0700 directory. Missing
fields fall back to defaults, unknown ones are ignored, out-of-range values are clamped, and a
file that isn't valid JSON is moved to `settings.json.bad`; an unknown theme / repeat value
(from a newer version) falls back to the default instead.

The Last.fm shared secret and session key, the ListenBrainz token, the Paxsenix /
BetterLyrics / AI-translation API keys, the proxy password, the YouTube Music sign-in
cookie and the Spotify refresh token go to the OS keychain
(`keyring` 4: macOS Keychain, Windows Credential Manager, Secret Service), service
`music-macos`, one entry each. If the keychain fails (or `MUSIC_SECRET_STORE=file`), they are
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
| `MUSIC_ONBOARDING=1\|0` | force / suppress the first-run guide (it is skipped automatically for `MUSIC_SCRIPT` / screenshot runs) |
| `MUSIC_UPDATE_REPO=owner/name` | enable the update check against that repo (testing; the default comes from `packaging/app.env`) |
| `MUSIC_TRACE=1` | log playback transitions, seeks, saves, route changes, artwork downloads; `MUSIC_TIMINGS=1`: renders/s + worst frame gap |
| `MUSIC_SCRIPT="5:@next,9:@seek=75,30:@quit"` | timed actions (any build) |
| `MUSIC_GPUI_SCREENSHOT="12:a.png,13:@settings,14:b.png,15:mini:m.png"` | offscreen shots + actions, quits after the last (feature `screenshot`; `mini:` shoots the mini player) |

Actions (`script.rs` has the table): input `@key=cmd-right` (a real keystroke through
GPUI's key dispatch), `@click=X;Y @rclick=X;Y @move=X;Y @mouse-back @mouse-forward`
(synthetic mouse events at window coordinates: hit-testing, hover, context menus);
playback `@play=N @next @prev @seek=S @toggle @open @close @volume=V`; navigation
`@nav=ROUTE @back @forward @search=Q`; queue `@play-next=N @enqueue=N @queue[=on|off]
@queue-move=A:B @queue-remove=N @queue-jump=N @queue-clear @shuffle=on|off
@repeat=off|all|one`; windows/appearance `@mini[=on|off] @theme=dark|black|dynamic
@font-scale=1.125 @resize=W;H`; settings `@settings[=section] @close-settings
@eq=Preset|on|off @field=lastfm-key|lastfm-secret|lb-token|discord-id:VALUE
@lastfm-connect @lastfm-finish @lastfm-disconnect @lastfm-clear @lb-validate
@lb-disconnect @discord=on|off`; player `@layout=split|centered|lyrics
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
@import-file=PATH @spotify-playlist=URL @spotify-liked @spotify-playlists @import-accept=N
@import-save[=NAME] @import-cancel @import-status @export=PLAYLIST:FORMAT:PATH`;
recognition `@recognize` (microphone), `@recognize-file=PATH`,
`@recognize-cancel`, `@recognize-history=N`; `@onboarding[=STEP|off]`, `@onboarding-next`,
`@onboarding-back`, `@update-check` (`release_script.rs`); `@quit`.

Screenshot builds draw both windows every 200 ms and right before each shot: while the
screen is locked macOS delivers no display-link frames, so nothing would redraw (and
lazily started artwork downloads would never begin).

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
→ `Music.app` (34 MB, ad-hoc signed, `codesign --verify --strict` passes, Info.plist
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
