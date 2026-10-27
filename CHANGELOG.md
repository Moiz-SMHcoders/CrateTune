# Changelog

All notable changes. The format follows [Keep a Changelog](https://keepachangelog.com/),
versions follow [SemVer](https://semver.org/). "Music" is a working name.

## [Unreleased]

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

* YouTube songs play as ~1-minute previews without a PoToken (not worked around).
* Account features and the Linux / Windows builds are not verified on real accounts /
  machines yet; builds are not signed with a Developer ID / Authenticode certificate.
