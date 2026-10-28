# Changelog

All notable changes. The format follows [Keep a Changelog](https://keepachangelog.com/),
versions follow [SemVer](https://semver.org/). "Music" is a working name.

## [Unreleased]

### Added

* **Full-length YouTube playback** (`youtube::WebResolver`): the TV client with YouTube's
  JavaScript challenges solved by yt-dlp's public-domain solver in QuickJS, or the mobile
  web client with a BotGuard PoToken (hidden WKWebView on macOS, ported from ArchiveTune).
  Waits out YouTube's pre-roll ad delay while playing the song's start from the iOS
  client, so playback starts in seconds. Settings → Playback → YouTube streams. Downloads
  are now full files. The ~1-minute preview remains as the last fallback.
* **Multichannel Opus** (5.1 / 7.1, mapping family 1 and 255) plays, mixed down to stereo.

### Changed

* **YouTube songs start faster**: about 0.5–0.8 s from click to sound instead of
  1.2–2.3 s (the start of the file downloads while the stream is still being resolved,
  the challenge solver loads during the player request, the session renews in the
  background).
* **Kanji readings without the 48 MB build**: Settings → Lyrics → Romanization downloads
  the Japanese dictionary on demand (10.5 MB, checksum-verified, 45 MB on disk) with
  progress, cancel and remove; until then romanization is kana-only.
* **Translations are kept** in the library: reopening a song doesn't translate it again.

### Fixed

* **Gapless AAC**: the encoder delay (~25–50 ms of warm-up) and the end padding of AAC
  files (local M4A, YouTube, modules) are trimmed, so gapless albums no longer get a gap
  or click between tracks; seeks in AAC are sample-accurate.
* **Expiring module streams** are renewed through their module shortly before they
  expire or when the server rejects them, and continue at the same byte (also for a
  preloaded next song and for downloads).
* Age-restricted YouTube videos no longer throw away the shared session (they were taken
  for YouTube's bot check), so they fail and get skipped in well under a second.
* **Credit lines** before the lyrics ("Julian Casablancas/Thomas Bangalter/…", "作词：…",
  "Written by: …", "Title - Artist") are filtered for every provider and format, without
  touching lyrics that contain "/" or ":".
* Instrumentals no longer show another song's lyrics (or a fan-written poem) when a
  provider reports "pure music"; QQ Music title lines like "MALAMENTE (Cap.1: Augurio) -
  ROSALÍA" are no longer shown as a duet singer.
* Google Translate rate limits: the app backs off (2 min, doubling up to 30 min, or the
  server's Retry-After), says "rate-limited, try again in N min" and retries by itself,
  instead of failing on every song. AI translation errors say what to check (key,
  endpoint, model, unreachable).
* Provider romanization is no longer shown under lines that are already Latin.
* A module playback test was sensitive to machine speed (it sampled positions on 100 ms
  ticks while rendering at ~3× real time).

## [0.1.0] — first release

### Added

* **Browse & search** YouTube Music: home feed with mood chips, Explore, Charts (per
  country), Moods & genres, New releases, artist / album / playlist pages, search with
  suggestions, recent searches and per-category tabs, radio and autoplay of similar songs.
* **Player**: gapless playback with optional crossfade, queue with shuffle / repeat /
  drag-and-drop, sleep timer, speed and pitch, loudness normalization, skip silence,
  10-band equalizer, output device choice, mini player, OS media controls (macOS, Windows
  SMTC, Linux MPRIS), macOS menu-bar item.
* **Now Playing** with three layouts, four backgrounds and word-by-word synced lyrics from
  many providers, translation and romanization.
* **Library** in a local database: likes, playlists, listening history, stats and "Year in
  Music", downloads and play cache, backup / restore; local music folders.
* **YouTube Music account** (optional): sign-in (macOS window or pasted cookie), library
  sync, likes and playlist edits on YouTube Music.
* **Scrobbling** to Last.fm and ListenBrainz, **Discord** Rich Presence.
* **Song recognition**: listen through the microphone (~10 s, cancellable) and identify the
  song with a Shazam-compatible fingerprint (ported from ArchiveTune's shazamkit); play,
  queue or search the match on YouTube Music; recognition history (50 entries). ⇧⌘R.
* **First-run guide**: language & region, theme, optional sign-in, optional music folders,
  privacy and limitations; skippable, re-openable from Settings → About.
* **App identity & packaging**: original app icon; name and ids in one file
  (`packaging/app.env`); macOS `.app` + DMG (`scripts/bundle-macos.sh`, ad-hoc signed,
  microphone usage description), Linux tarball / AppImage + `.desktop`
  (`scripts/bundle-linux.sh`), Windows icon / version resource, zip and Inno Setup
  installer (`scripts/bundle-windows.ps1`).
* **Optional update check** against GitHub releases (off unless `UPDATE_REPO` is set).
* **CI** (fmt, clippy, tests on macOS / Linux / Windows) and tag-triggered release builds.
* User guide (`docs/USER_GUIDE.md`).

### Known limitations

* YouTube songs play as ~1-minute previews without a PoToken (fixed in Unreleased).
* Account features and the Linux / Windows builds are not verified on real accounts /
  machines yet; builds are not signed with a Developer ID / Authenticode certificate.
