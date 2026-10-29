# Backlog

Known bugs, unverified areas, platform work, features ArchiveTune has that CrateTune lacks,
performance ideas and technical debt, all in one list so items can be picked up later.
Details stay in the linked sections. Keep this file current: remove an item once it is
fixed or verified, and add new ones here instead of only in a README round section.

Each item lists its **size** (small: about a day or less · medium: a few days ·
large: a week or more, or it depends on upstream) and its **priority**.

Last reviewed: 2026-10-29, against `main` at `bdb3625`.

## 1. Bugs

**B1. Windows: guests see "Connection lost" when the host ends a Listen Together session**
Guests should see "Session ended". The server flushes the close frame, waits up to 1 s for
the reply, then half-closes and drains the socket; that fixed Linux, but on Windows the close
frame is still lost now and then. The CI test accepts either message on Windows until this is
looked at on a real Windows machine. Details: `together::tests::session_end_to_end`,
`Conn::close` in `crates/music-core/src/together/conn.rs`. · Size: medium · Priority: low

## 2. Unverified / needs real-world testing

The code below works in unit tests, fixtures, scripts or fake servers, but nobody has run it
against the real service or device yet.

**U1. Google sign-in and the signed-in features**
Cookie sign-in, library (including the Podcasts tab), history, likes, subscriptions, playlist
editing, Save to library and playlist cover upload were checked against fixtures only. These
are the features users will notice first. Details: [README: Library, account, downloads, local
files](../README.md#library-account-downloads-local-files), [Content
round](../README.md#content-round-video-types-podcasts-long-albums-dns-over-https-covers),
[TESTING.md 5.7](TESTING.md#57-playlist-covers-signed-in-untested-so-far),
`crates/music-core/src/innertube/account.rs`. · Size: medium · Priority: high

**U2. Physical input: mouse, trackpad, keyboard and media keys**
Every interaction test used synthetic events (`@key`, `@click`, scripts). Still untested by
hand: drag and drop in the queue, volume drag and trackpad scrolling, OS media keys and Control
Center, clicks on the macOS menu bar and the menu-bar item. Details: [README: Measurements (app
shell round)](../README.md#measurements-app-shell-round), [Player bar
redesign](../README.md#player-bar-redesign-progress-styles-backgrounds), [Keyboard shortcuts
and menus](../README.md#keyboard-shortcuts-and-menus). · Size: small · Priority: high

**U3. Non-US keyboard layouts**
Shortcuts and the shortcut editor were only tested with a US layout. Details: [README: Keyboard
shortcuts and menus](../README.md#keyboard-shortcuts-and-menus). · Size: small · Priority: medium

**U4. Last.fm and ListenBrainz scrobbling**
Only the error paths with bogus credentials reached the real APIs. A full Last.fm auth, real
scrobbles and listens, and custom scrobble rules are untested. Details: [README: Measurements
(previous round)](../README.md#measurements-previous-round-queue-media-controls-eq-scrobbling),
[Player UI round](../README.md#player-ui-round-verification). · Size: small · Priority: medium

**U5. Discord Rich Presence**
Never run against a real Discord client. Details: [README: Player UI
round](../README.md#player-ui-round-verification). · Size: small · Priority: low

**U6. Spotify sign-in and playlist import**
There was no developer Client ID to test with. The code follows the documented Web API,
including the 2026 `/playlists/{id}/items` rename with a fallback to `/tracks`. Details:
[README: Integrations](../README.md#integrations-sponsorblock-music-together-playlist-import--export),
`crates/music-core/src/spotify.rs`. · Size: small · Priority: medium

**U7. Lyrics keys and translation: Paxsenix, BetterLyrics, AI translation, Google Translate**
API keys were never used against the live services. AI translation was only tested against a
local fake server. Google's free endpoint rate-limited the test IP, so a successful Google
translation was never seen. Details: [README: Lyrics round](../README.md#lyrics-round-verification),
[Player UI round](../README.md#player-ui-round-verification). · Size: small · Priority: low

**U8. Chromecast on real devices**
Only `examples/fake_cast_receiver.rs` was used. Still unknown: how a real receiver fetches the
stream, whether it plays Opus-in-WebM, and the TLS handshake with a real device certificate.
Details: [README: Integrations round 2](../README.md#integrations-round-2-relay-host-transfer-chromecast-ai-filter-potoken).
· Size: medium · Priority: medium

**U9. Song recognition from the microphone**
Recognition from WAV clips and the live Shazam endpoint work. A real microphone recording and
the macOS permission prompt were never triggered (the capture path is the example's `--mic`).
Details: [README: Release round](../README.md#release-round-recognition-onboarding-packaging).
· Size: small · Priority: medium

**U10. Listen Together across real machines and networks**
All tests ran on one machine with demo clocks. Untested: two machines, a firewall prompt, real
audio on both sides, a host transfer between machines, and the relay behind a TLS reverse
proxy across the internet. Details: [README: Integrations
round 2](../README.md#integrations-round-2-relay-host-transfer-chromecast-ai-filter-potoken),
[Relay server](../README.md#relay-server-music-relay). · Size: medium · Priority: medium

**U11. 120 Hz displays**
Animation pacing and the cached Now Playing regions were only seen at 60 Hz. Details: [README:
Now Playing round](../README.md#now-playing-round-cpu-designs-lyrics-cards). · Size: small ·
Priority: low

**U12. AI content filter with a real LLM endpoint**
Only stub servers were used. Details: [README: Integrations
round 2](../README.md#integrations-round-2-relay-host-transfer-chromecast-ai-filter-potoken).
· Size: small · Priority: low

**U13. Provider modules: a real third-party provider and keychain secrets**
Only the bundled test module (`echo`) was used, with the file secret store. The "Install from
file…" picker was also never used on screen. Details: [README: Provider
modules](../README.md#provider-modules-arch). · Size: small · Priority: low

**U14. macOS file dialogs and window behaviour**
Not tested through the real panels: settings, theme and profile export and import, and lyrics
card "Save image…". Also untested: pasting a copied card into another app, and the mini player
staying above other apps. The same code paths ran through script actions. Details: [README:
Theming round](../README.md#theming-round-customize-theme-dynamic-colours-profiles-downloads),
[Now Playing round](../README.md#now-playing-round-cpu-designs-lyrics-cards). · Size: small ·
Priority: low

**U15. Smaller edge paths**
Not yet tested:
- Quad9 DNS-over-HTTPS, and DoH through a proxy.
- Downloading a playlist of more than 100 songs (load all pages, then download).
- Removing a song from downloads when it is unliked mid-download.
- The output-device menu with a second device.
- Auto-skip after a real load error.
- Duets and background vocals on screen (covered only by a unit test).

Details: the "Not verified" paragraphs of the [Content
round](../README.md#content-round-video-types-podcasts-long-albums-dns-over-https-covers),
[Theming round](../README.md#theming-round-customize-theme-dynamic-colours-profiles-downloads)
and [Player UI round](../README.md#player-ui-round-verification). · Size: small · Priority: low

## 3. Platform & distribution

**P1. Builds are unsigned**
There is no Apple Developer ID or notarization and no Windows Authenticode signature, so
Gatekeeper and SmartScreen warn on first open. `scripts/bundle-macos.sh` already signs with
hardened runtime when `MACOS_SIGN_IDENTITY` is set. What's missing is a certificate,
notarization in `release.yml`, and Authenticode for the `.exe` and the installer. Details:
[README: Install / build](../README.md#install--build), [USER_GUIDE: Install](USER_GUIDE.md#install).
· Size: medium · Priority: high

**P2. Linux and Windows builds are untested on real desktops**
CI builds them and runs the tests, and nothing more. Untested by hand:
- Windows: media keys (SMTC), Credential Manager, the window handle, Ctrl shortcuts, fonts,
  the folder picker, the resource embedding and the Inno Setup installer.
- Linux: the tarball, the AppImage and `.desktop` integration.

Details: [TESTING.md 5.8](TESTING.md#58-windows-specific-untested), [README: Release
round](../README.md#release-round-recognition-onboarding-packaging). · Size: medium ·
Priority: high

**P3. PoToken on Windows (WebView2) and Linux (WebKitGTK)**
This code is type-checked only and has never run on those systems. On Linux it is also off by
default (feature `webkitgtk-potoken`). Without it the TV client is used, so full streams still
work, but with one fewer fallback. Details: [README: Full-length YouTube
streams](../README.md#full-length-youtube-streams-youtube-module),
`crates/music-core/src/youtube/webview2.rs`, `webkitgtk.rs`. · Size: medium · Priority: medium

**P4. The bundle ID is a placeholder (`io.github.moiz-smhcoders.cratetune`)**
Changing the identity in `packaging/app.env` moves the settings folder and keychain entries,
so pick the final ID before many users exist. Details: [README: Install /
build](../README.md#install--build) ("App identity"). · Size: small · Priority: high

**P5. No menu-bar or tray item on Linux and Windows**
The item is built on macOS only. Linux would need GTK or appindicator, and Windows is unbuilt
and untested. Details: `crates/music-gpui/src/tray.rs`. · Size: medium · Priority: low

**P6. Self-update: Windows and Linux paths not run on a real system**
Self-update is implemented. On macOS it was tested end to end against a fake release
server: download, SHA-256 check, bundle swap, relaunch and rollback. The Windows paths
(the silent Inno Setup update with `/RELAUNCH=1`, the portable `--apply-update` helper)
and the Linux paths (the AppImage swap, the tarball `bin/` + `share/` swap, the relaunch)
are only unit-tested and type-checked. Also unverified:
* the keychain and microphone prompts after an ad-hoc-signed macOS update;
* App Management on `/Applications` (macOS 13+);
* a download from a real GitHub release with `SHA256SUMS.txt` (the first one will be the
  release after 0.1.0).

0.1.0 users must install the next version by hand once. Details: [README:
Self-update](../README.md#self-update), [RELEASING.md](RELEASING.md). · Size: small ·
Priority: high

## 4. Missing features vs ArchiveTune

The README has no full feature-by-feature comparison. These are the gaps it and the code
mention explicitly.

**M1. Light theme**
The app is dark only, and "dim" is the lightest darkness. ArchiveTune offers light and dark.
Details: [README: Player bar
redesign](../README.md#player-bar-redesign-progress-styles-backgrounds) ("a light theme (the
app has none)"), `crates/music-gpui/src/settings/theme_prefs.rs`. · Size: medium · Priority: medium

**M2. Screen-reader accessibility and keyboard focus for track rows**
Screen reader support is limited by GPUI. Track rows can't take keyboard focus, so keyboard-only
use stops at the buttons. Details: [USER_GUIDE: Known limitations](USER_GUIDE.md#known-limitations),
[README: Player bar redesign](../README.md#player-bar-redesign-progress-styles-backgrounds).
· Size: large · Priority: medium

**M3. Module extension-compatibility runtime**
`.arch` packages that ship `index.js` (the extension runtime) are refused with
`ARCH_UNSUPPORTED_runtime`. Only the `provider.js` API v1 is supported. Details:
`crates/music-core/src/modules/package.rs`, [README: Provider modules](../README.md#provider-modules-arch).
· Size: large · Priority: low

**M4. Spotify `sp_dc` cookie login (by design, won't do)**
ArchiveTune's cookie login needs a TOTP made from a secret extracted from Spotify's web player,
which isn't a legitimate client flow. CrateTune uses the official Web API with the user's own
Client ID, or an Exportify CSV. Listed so nobody ports it by accident. Details: [README:
Integrations](../README.md#integrations-sponsorblock-music-together-playlist-import--export).
· Size: n/a · Priority: low

**M5. ArchiveTune's blocklist-based AI-music filter (by design, not used)**
ArchiveTune hides AI-generated music using channel blocklists from a third-party list.
CrateTune's AI content filter asks an LLM endpoint instead. Details: [README: Integrations
round 2](../README.md#integrations-round-2-relay-host-transfer-chromecast-ai-filter-potoken).
· Size: medium · Priority: low

Right-to-left UI languages, which ArchiveTune also has, are under [Translations](#7-translations-low-priority).

## 5. Performance ideas

**F1. Memory footprint: look into the GPU and graphics buffers**
The footprint is about 60–180 MB since the memory round, which added `MallocSpaceEfficient`, an
artwork budget by decoded size, and a shorter lifetime for the prepared solver. Most of what
is left is GPU and graphics buffers (IOSurface). Next step: find out which surfaces GPUI
keeps, and whether atlases or offscreen layers can shrink. Details:
`crates/music-gpui/src/memory.rs`, CHANGELOG 0.1.0 "Uses much less memory". · Size: medium ·
Priority: medium

**F2. Re-measure CPU with the screen awake**
The screen was locked while the final Now Playing build and the app shell round were measured,
so macOS sent no display-link frames. The Lyrics-first strip region was never measured on its
own. Details: [README: Now Playing round](../README.md#now-playing-round-cpu-designs-lyrics-cards),
[Measurements (app shell round)](../README.md#measurements-app-shell-round). · Size: small ·
Priority: low

## 6. Technical debt

**T1. Vendored `opus-decoder`**
`vendor/opus-decoder` is a copy of 0.1.1 with two patches: an FFT-based inverse MDCT, and the
removal of debug-log file opens. Drop it once upstream releases both fixes. Also, dev builds
turn off overflow checks for this crate because its CELT shifts overflow on real streams. That
belongs upstream too. Details: `vendor/opus-decoder/VENDORED.md`, workspace `Cargo.toml`.
· Size: small · Priority: low

**T2. GPUI version pinning**
`gpui-component =0.8.1` pins the `gpui-pre =0.3.10` snapshot, so GPUI fixes only arrive with a
gpui-component upgrade. The Hindi wrapping fix (L2) and RTL support (L3) depend on this, and
patching it locally would mean vendoring GPUI. Details: `crates/music-gpui/Cargo.toml`,
[README: History](../README.md#history-benchmark-rounds-condensed). · Size: large · Priority: medium

**T3. Work-arounds for missing GPUI rendering features**
GPUI has no transforms on arbitrary subtrees, layer opacity, blur or backdrop filters,
alpha or gradient masks, or radial gradients. The app works around this with CPU pre-rendering
(blurred backdrops, glow images) and custom elements. Revisit after a GPUI upgrade. Details:
[README: History](../README.md#history-benchmark-rounds-condensed), [Now Playing
round](../README.md#now-playing-round-cpu-designs-lyrics-cards). · Size: large · Priority: low

**T4. YouTube stream access is fragile**
Full streams depend on YouTube's current client behaviour (TV client, PoToken, the
`n`/signature solver from yt-dlp/ejs 0.8.0). When YouTube changes something, the app falls
back to previews until it is updated. Keep the vendored solver current and the live examples
running. Details: [README: Full-length YouTube
streams](../README.md#full-length-youtube-streams-youtube-module). · Size: medium (ongoing) ·
Priority: medium

## 7. Translations (low priority)

> **Not a priority.** The owner finds English good enough for personal use. Pick these up
> only when everything above that matters is done, or when a native speaker volunteers.

**L1. Native-speaker review of the 17 non-English locales**
The catalogs were never reviewed by native speakers, and screenshots covered only five locales
(de, ru, ja, zh-CN, hi) plus spot checks. Details: [README: Translations (UI
languages)](../README.md#translations-ui-languages). · Size: medium · Priority: low

**L2. Hindi (Devanagari) line wrapping in GPUI**
GPUI's `LineWrapper::is_word_char` lacks the Devanagari block, so wrapped Hindi can break
inside a word. Width sums can also cut a final mark (ं) on a few gpui-component buttons. The
upstream fix is one range in `is_word_char`, but applying it here means vendoring or patching
GPUI (see T2). Details: [README: Translations](../README.md#translations-ui-languages) ("Hindi
caveat"). · Size: medium · Priority: low

**L3. Right-to-left languages (Arabic, Persian, Hebrew)**
ArchiveTune ships them. GPUI shapes RTL text but can't right-align it or mirror the layout, so
they're left out. Once GPUI supports this, adding them is a catalog and `plural.rs` change.
Details: [README: Translations](../README.md#translations-ui-languages). · Size: large ·
Priority: low

**L4. Texts that stay in English**
Some texts stay English in every language: messages from the engine or servers, the default
Discord activity text, the input fields' right-click menu (in most languages), and module
names and settings. Details: [USER_GUIDE: Known limitations](USER_GUIDE.md#known-limitations).
· Size: medium · Priority: low

**L5. Language detection and the menu bar after a switch**
System-language detection on Windows and Linux never ran; only the macOS path was tested. The
macOS menu bar was also never checked on screen after a live language switch. Details: [README:
Translations](../README.md#translations-ui-languages). · Size: small · Priority: low
