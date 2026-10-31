# Backlog

Known bugs, unverified areas, platform work, features ArchiveTune has that CrateTune lacks,
performance ideas and technical debt, all in one list so items can be picked up later.
Details stay in the linked sections. Keep this file current: remove an item once it is
fixed or verified, and add new ones here instead of only in a README round section.

Each item lists its **size** (small: about a day or less · medium: a few days ·
large: a week or more, or it depends on upstream) and its **priority**.

Last reviewed: 2026-10-29, against `main` at `bdb3625`.

## 1. Bugs

**B2. Windows: the Chromecast connection polls with socket read timeouts**
`crates/music-core/src/cast/session.rs` polls its TLS connection every 50 ms through
`SO_RCVTIMEO`. On Windows such a read can fail with error 997, lose bytes or crash the process
(see `crates/music-net`). Listen Together and the relay moved to `music_net::TimedTcp`; Cast
should too. · Size: small · Priority: medium

## 2. Unverified / needs real-world testing

The code below works in unit tests, fixtures, scripts or fake servers, but nobody has run it
against the real service or device yet.

**U1. Google sign-in and the signed-in features**
Cookie sign-in, library (including the Podcasts tab), history, likes, subscriptions, playlist
editing, Save to library and playlist cover upload were checked against fixtures only.
Picking a Google account of a multi-account login (Settings → Account; probing
`account/account_menu` per `X-Goog-AuthUser`) was checked against a fake server only: still
to confirm what Google answers for an index the login doesn't hold (expected: an error;
a fallback to account 0 is handled too) and that a two-account browser cookie switches. These
are the features users will notice first. Details: [README: Library, account, downloads, local
files](../README.md#library-account-downloads-local-files), [Content
round](../README.md#content-round-video-types-podcasts-long-albums-dns-over-https-covers),
[TESTING.md 5.7](TESTING.md#57-playlist-covers-signed-in-untested-so-far),
`crates/music-core/src/innertube/account.rs`. · Size: medium · Priority: high

**U1a. Keeping the YouTube session alive**
`Set-Cookie` rotation, the silent renewal through the sign-in web view and the "sign-in
expired" flow were checked against fake servers and unit tests only. Still to confirm with a
real account: that InnerTube responses carry rotating `Set-Cookie` lines at all (Google may
rotate only through `accounts.google.com/RotateCookies`, which the page's script calls), and
that loading music.youtube.com hidden on the persistent profile renews an expired session
(`LOGGED_IN` flag, cookies). Deferred: calling `RotateCookies` ourselves on a schedule (an
undocumented endpoint, and it rotates `.google.com` cookies the app doesn't keep); a
background renewal on Linux (no sign-in web view there). [TESTING.md
5.8a](TESTING.md#58a-staying-signed-in-windows-first-also-macos),
`crates/music-core/src/innertube/session.rs`, `crates/music-gpui/src/session_keeper.rs`.
· Size: small · Priority: high

**U1b. Player fixes: publishing a real fix**
The signed manifest's fetch, verification, cache and rollback rules are unit-tested against
in-memory sources, and the relay's `--player-fixes` route has an end-to-end test. Both
published paths are live (2026-10-31): `raw.githubusercontent.com/<UPDATE_REPO>/main/packaging/`
and the mirror at `relay.smhcoders.click/player-fixes.json` (byte-identical, refreshed by
`~/cratetune-relay/fetch-player-fixes.sh` on dev.home, hourly by cron). Still to do: publish a real fix (serial 2) and watch a running
app pick it up from each path. Details:
[RELEASING.md](RELEASING.md#player-fixes-packagingplayer-fixesjson). · Size: small ·
Priority: medium

**U1c. visionOS and TV Simply over time and on other networks**
Both were verified on one network in Oct 2026 (visionOS 6/6 songs and TV Simply 5/5, full
length). refryt and Limusic report that networks and regions differ in which anonymous clients
YouTube refuses. The breaker covers a refusal, but whether visionOS URLs keep serving
whole files in 10 MiB ranges, and how often its visitor id needs renewing, has only been seen
over hours, not weeks. · Size: small · Priority: medium

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
All tests ran on one machine with demo clocks. The relay behind a TLS reverse proxy now works
(two instances on one Mac through `wss://relay.smhcoders.click` with Caddy, joining by room
code), but both ran on the same home network. Untested: two machines, guests on a different
network (e.g. a phone hotspot), a firewall prompt, real audio on both sides, a host transfer
between machines. Room codes exist only for relayed sessions (a LAN session has a link only). Details: [README: Integrations
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

**U16. Automatic sync and per-song likes against a real account**
The sync triggers, throttle and guard (`youtube_sync.rs`), the liked-songs rules, the
offline rating queue and `like/dislike` were checked with unit tests, fixtures and loopback
fakes only. To confirm with a real account: (1) a song liked on the phone turns liked here
on play (`next` → `playerOverlays…likeButtonRenderer.likeStatus` = `LIKE` when signed in);
(2) whether WEB_REMIX `next` carries `responseContext…GFEEDBACK logged_in` (without it,
"neither" is ignored and un-likes made elsewhere wait for the next sync); (3) whether
queue rows of a signed-in radio carry `menu…likeButtonRenderer` (saves requests); (4)
`like/dislike` shows the dislike on YouTube Music and the song stays out of the account's
radio there; (5) a like made offline arrives after reconnecting; (6) a sync of a few
thousand liked songs (60-page cap) and its unlike guard. `authed/next_liked.json` is
hand-built from the recorded anonymous `next.json`. Details: [README: Account sync, likes
and dislikes](../README.md#library-account-downloads-local-files). · Size: small ·
Priority: high

**U17. Several accounts, browser sign-in and YouTube history against real services**
Built in `feat/accounts-plus` and tested with fake cookies, a loopback server and fixture
cookie databases only. To confirm: (1) adding a second Google login through the sign-in
window (its own profile) and switching back and forth: sessions, Home and the library follow,
and each account's background renewal uses its own profile; (2) "Sign in from your browser"
with real Chrome / Edge / Brave / Arc / Vivaldi / Opera / Chromium profiles on macOS
(Keychain prompt), Firefox everywhere, Safari with Full Disk Access, Linux with GNOME
Keyring and KWallet, and Windows Chrome with app-bound encryption (`v20`, expected to be
refused with the explanation); (3) that YouTube files the reported plays
(`videostatsPlaybackUrl` / `videostatsWatchtimeUrl`) into the account's history and they
appear on the phone, and whether the `player` request needs the player's
`signatureTimestamp` to return `playbackTracking` (none is sent today); (4) whether
`FEmusic_history` entries of episodes carry `startTimeSeconds` / a progress percentage for
the "Jump back in" card. Details: [README: Several accounts, browser sign-in, YouTube
history](../README.md#several-accounts-browser-sign-in-youtube-history). · Size: small ·
Priority: high

**U20. Look round on Windows, Linux and real hardware**
Checked on macOS only (unit tests, scripts, offscreen screenshots, window captures of the
blur): (1) the window materials on real desktops — Windows 10 (acrylic only; Mica hidden
below build 22621) and Windows 11 (acrylic, Mica, Mica Alt, with DirectComposition and
with `disable_direct_composition`), KDE Plasma on Wayland (blur), GNOME / Sway / X11 with a
compositor (see-through) and without one; (2) the Windows window icon (`WM_SETICON` from
PNG data through `CreateIconFromResourceEx`) on the title bar, Alt+Tab and the taskbar
button, and the X11 icon at the next start; (3) theme-file watching on Windows
(`ReadDirectoryChangesW`) and Linux (inotify) with editors that save through a temp file
and rename; (4) Ctrl + wheel zoom with real mice and touchpads on Windows / Linux (scripts
dispatch synthetic events); (5) React to the music by ear and eye on real music (only
the envelope's tests and frame counts were checked). Details: [README: Look
round](../README.md#look-round-theme-files-see-through-window-music-reactive-look-zoom-app-icon).
· Size: small · Priority: medium

**U18. Queue / selection round (`feat/queue-ux`) on real systems**
Checked with unit tests, scripts and offscreen screenshots on macOS only: (1) the in-app
log's stderr capture on Windows (`SetStdHandle` to a pipe; release builds without a
console, and with `MUSIC_CONSOLE=1`) and on Linux; (2) the ALSA device filter against real
Linux machines (PipeWire, PulseAudio-only, plain ALSA, USB DACs, HDMI) and the device list
on Windows (WASAPI names); (3) "Suggestions need my approval" with an ArchiveTune guest or
host (the new `suggestion_status` message must be skipped there, as our decoder does);
(4) ⌘-click / Shift-click and ⌘A with a real mouse and keyboard on each system (scripts
dispatch synthetic events), Ctrl-click on Windows / Linux; (5) continuing a queue tab while
casting (the seek to the saved position isn't sent to a Cast session). Details: [README:
Queue round](../README.md#queue-round-queue-parts-and-tabs-multi-select-devices-logs-suggestions).
· Size: small · Priority: medium

**U19. Now Playing extras on real systems**
Checked on macOS with unit tests, scripts and offscreen screenshots only: (1) the reverb by
ear (it was only measured on buffers: silence, stability, decay order, pre-delay, no
subnormals) — whether the rooms sound right, and the level at a high wet mix (it can
exceed 0 dBFS on loud masters; there is no limiter); (2) the mini player layouts on Windows
(topmost, own close strip) and Linux, the turntable arm's position on a Retina vs a 1×
screen, reopening on a layout change with a real mouse; (3) the ambient lights on a 120 Hz
display. · Size: small · Priority: low

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
  the folder picker, the resource embedding and the Inno Setup installer. New with the
  Windows polish (built and tested in CI only): the WebView2 sign-in window, the
  notification-area icon, smooth wheel scrolling, the single-instance handoff, the
  mini player's topmost style / drag strip, `MUSIC_CONSOLE`, local-time dates and the
  "Animation effects" setting. Checklist: [TESTING.md 5.10](TESTING.md#510-windows-checklist-for-the-user-after-updating-to-the-windows-polish-build).
- Linux: the tarball, the AppImage and `.desktop` integration.

Details: [TESTING.md 5.8](TESTING.md#58-windows-specific-untested), [README: Release
round](../README.md#release-round-recognition-onboarding-packaging). · Size: medium ·
Priority: high

**P3. PoToken on Windows (WebView2) and Linux (WebKitGTK)**
This code is type-checked only and has never run on those systems. On Linux it is also off by
default (feature `webkitgtk-potoken`). Without it the visionOS and TV clients are used, so full
streams still work, but with two fewer fallbacks (TV Simply and web need a PoToken). Details: [README: Full-length YouTube
streams](../README.md#full-length-youtube-streams-youtube-module),
`crates/music-core/src/youtube/webview2.rs`, `webkitgtk.rs`. · Size: medium · Priority: medium

**P4. The bundle ID is a placeholder (`io.github.moiz-smhcoders.cratetune`)**
Changing the identity in `packaging/app.env` moves the settings folder and keychain entries,
so pick the final ID before many users exist. Details: [README: Install /
build](../README.md#install--build) ("App identity"). · Size: small · Priority: high

**P5. No tray item on Linux**
macOS has the menu-bar item and Windows the notification-area icon (`tray-icon`). Linux would
need GTK or appindicator (`tray-icon`'s Linux backend runs a GTK loop). Details:
`crates/music-gpui/src/tray.rs`. · Size: medium · Priority: low

**P5b. "Sign in with Google…" on Linux**
macOS (WKWebView) and Windows (WebView2) have the sign-in window; Linux has only "Paste
cookie…". A WebKitGTK window would need a GTK main loop on its own thread (as the
WebKitGTK PoToken provider does, behind `webkitgtk-potoken`) plus `libwebkit2gtk-4.1` at
build and run time, which the default Linux build avoids. Details:
`crates/music-gpui/src/login_webview/`. · Size: medium · Priority: low

**P5c. Windows: smaller gaps found in the Windows polish**
* The mini player has its own taskbar button (GPUI's `Floating` kind is a normal app
  window on Windows; a tool window would hide it but also from Alt+Tab).
* Window chrome is the native title bar (light / dark follows Windows, not the app theme);
  a custom title bar like the mini player's strip would match the theme.
* High-DPI and per-monitor scaling are GPUI's (manifest `PerMonitorV2`); not looked at on a
  real 125 % / 150 % screen.
* Smooth wheel scrolling uses GPUI's line deltas; free-spinning wheels and the "one screen
  at a time" setting are handled, but not tried on hardware.
· Size: small · Priority: low

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

**P7. Publish to the package managers (accounts and first submissions)**
The manifests are generated and validated by every release (`packages` and `flatpak` jobs),
but nothing is published yet. Once each, by the maintainer: create the public
`Moiz-SMHcoders/homebrew-tap` repo with `Casks/cratetune.rb`; create
`Moiz-SMHcoders/scoop-bucket` from ScoopInstaller's BucketTemplate with
`bucket/cratetune.json` (keep its Excavator workflow); make an AUR account with an SSH key and
push `cratetune-bin` (and optionally `cratetune`) after `makepkg -si` and namcap, fixing the
`# Maintainer:` line; fork `flathub/flathub`, branch from `new-pr` and open the submission
pull request with the three files from `packaging/flatpak/` (consider the 26.08 runtime the
linter suggests first). After that, each release's `package-manifests` asset is copied into
each channel. Steps: [RELEASING.md](RELEASING.md#package-managers). · Size: small ·
Priority: very low

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

**M6. Look & motion ideas considered but not built (app-look round)**
Looked at Spotify, Apple Music, YouTube Music, Plexamp and ArchiveTune; these were left out
of [README: App-wide look & motion](../README.md#app-wide-look--motion):
* *Smooth (animated) wheel scrolling.* GPUI scrolls by the deltas the OS sends; trackpads
  already get the system's momentum, a mouse wheel moves in steps. Smoothing wheel steps
  needs a per-scroll-view animation driving `ScrollHandle` offsets (frames while
  scrolling, interaction with virtual lists). · Size: medium · Priority: low
* *Card "lift" by moving or scaling tiles on hover, and rows that smoothly expand.* GPUI
  has no transforms on arbitrary subtrees (T3); an offset would be clipped by the shelves'
  horizontal scroll areas, and animating it means frames per hover. Tiles get a raised
  background, a shadow under the art and the play button instead (one render per hover
  change). · Size: small after T3 · Priority: low
* *Blur-based looks* (frosted cards, blurred artwork behind every page, Apple Music's
  animated artwork background app-wide). No backdrop filters in GPUI (T3); a CPU-blurred
  image behind cached content would cost memory per window size. · Size: large ·
  Priority: low
* *Marquee (scrolling) long titles* in rows and the bar. Continuous frames for any
  truncated title on screen (F4: none exist today, titles truncate). · Size: small ·
  Priority: low
* *A separate compact list mode.* Customize theme → Density → Compact already gives 50 pt
  rows and tighter paddings everywhere. · Won't do unless asked
* *KDE's animation speed for the System animations level.* Only GNOME's
  `enable-animations` (and Windows' "Animation effects") are read. · Size: small ·
  Priority: low
* *Translucent sidebar on Windows (Mica) and Linux.* The look round added a see-through
  window on every platform (Appearance → Window → Window background, U20); the
  sidebar-only switch stays macOS-only. · Size: small · Priority: low

The look round (refryt, Limusic, Zuno, Deviload; [README: Look
round](../README.md#look-round-theme-files-see-through-window-music-reactive-look-zoom-app-icon))
built theme files, the see-through window, React to the music, zoom, the app icon and
"Reduce transparency"; these ideas from the same survey were left out:
* *A light theme and a dark / light pair per mode* (all four apps). Theme files can set
  light `[colors]`, but Now Playing, the lyrics card, the bar's backgrounds and several
  schematic previews draw white on dark on purpose; see M1. · Size: medium · Priority:
  medium
* *Wallpaper palette sync* (Limusic's Matugen file, watched). A Matugen template that
  writes our theme TOML would work with the watcher as is; needs a template and docs.
  · Size: small · Priority: low
* *A quick theme switcher in the top bar* (Deviload's popover with swatches). · Size:
  small · Priority: low
* *Scroll fades at list edges* (refryt, recommended with transparency). Needs alpha masks
  (T3). · Size: small after T3 · Priority: low
* *Card outlines and a window edge hairline* (refryt, Zuno). · Size: small · Priority: low
* *Sidebar modes* — a collapsed icon rail, expand on hover, a drag-resizable sidebar and
  queue panel (Zuno, refryt, Limusic). The shell's fixed widths are in many layout
  computations (`SIDEBAR_WIDTH`, the top bar's drag regions). · Size: medium · Priority:
  low
* *Window control styles* (macOS / Windows look on any system, Zuno) and *hiding single
  top-bar buttons*. · Size: small · Priority: low
* *Track list columns to show or hide* (album, added, plays, duration; Limusic). · Size:
  medium · Priority: low
* *Fonts from files and a separate heading font* (Limusic). GPUI can register font bytes
  at runtime; the font picker lists installed families only. · Size: small · Priority: low
* *A one-run theme from the command line* (refryt `--scheme`), e.g. `MUSIC_THEME=nord`.
  · Size: small · Priority: low
* *Shortcuts that follow the key position on non-Latin layouts* (Limusic). · Size: medium ·
  Priority: low
* *Fake liquid glass* (Deviload's WebGL refraction over a wallpaper). No shaders or
  backdrop filters in GPUI (T3). · Won't do

**M7. Translucent sidebar: the rest of the real-desktop check**
Checked on a macOS desktop in the look round (window captures with `screencapture -l`):
it showed nothing before, because gpui-component's `Root` painted an opaque background
under the whole app; the root is transparent now, the sidebar shows the blurred desktop,
the content area and bar stay opaque, and its opacity is raised until the sidebar's text
stays readable on a light desktop (45 % → about 74 %). Still to do: sample live CPU with
page transitions / hover off and Animations Reduced / Off (that run lost its display
frames), and check Now Playing / dialogs covering it. Details: `look_state.rs`, [README:
App-wide look & motion](../README.md#app-wide-look--motion). · Size: small · Priority: low

**M8. Sync settings and library between devices (Automerge)**
After the iroh device hand-off: keep settings, likes, playlists, speed-dial pins and queue
tabs as Automerge CRDT documents per device and merge them over the paired iroh links,
so a Mac and a laptop behave like one app, including after offline edits. Idea from
tono.fm's client (AGPL, ideas only). · Size: large · Priority: medium

**M9. Lasting friend groups and an activity feed**
Turn one-off Music Together rooms into friend groups (invite codes, a recovery phrase for
the identity) and add a feed of what friends are listening to, like Spotify's friend
activity. Builds on the device keys and iroh links. Idea from tono.fm. · Size: large ·
Priority: low

**M10. Full UI tabs (separate page stacks per tab)**
Zuno gives every tab its own player *and* its own page stack (back / forward, scroll). Only
the queue part was built (queue tabs, `queue_tabs.rs`): the router keeps one history and
one cached page per route, pages talk to the one app entity, and a second page stack would
double the cached pages (memory) for little gain while only one queue can play. Revisit if
users ask for parallel browsing. · Size: large · Priority: low

**M11. Now Playing / mini player / sound ideas from Zuno, Limusic, refryt and Deviload not built**
Surveyed for the Now Playing extras round (README: [Now Playing
extras](../README.md#now-playing-extras-round-sound-effects-mini-player-layouts-turntable)); what
was built is listed there. Left out, with the reason:
* *Fine reverb controls and convolution rooms* (Limusic: length 0.1–10 s, pre-delay 0–200 ms,
  brightness, early reflections, dry level; impulse responses through FFmpeg's `afir`). The
  four algorithmic rooms and the wet mix cover the common use; the extra sliders map onto
  the existing `ReverbParams` if asked for. · Size: small · Priority: low
* *Your own named sound presets* (Limusic, saved speed / pitch / reverb) and *named EQ presets,
  a draggable response graph, a headroom warning, plain-language band names* (Zuno). · Size:
  medium · Priority: low
* *A compressor / "volume smoothing" / night mode* (Deviload: threshold −24 dB, ratio 3). A
  new DSP stage with its own tests; normalization already evens out songs. · Size: medium ·
  Priority: low
* *Perceptual volume curve* (Limusic `10^(−(1−p)^1.5)`, Deviload `p²`): changes how every
  saved volume sounds; needs a migration. · Size: small · Priority: low
* *Mini player skins that need shaped or transparent windows or constant motion*: Zuno's
  Classic capsule (grows on hover, clicks pass through), Cassette (turning spools), Y2K
  (gel shape, LCD meter); a spinning picture disc (Deviload; GPUI cannot rotate images and the
  disc would redraw ~30 times a second while playing). · Size: medium · Priority: low
* *Up next / queue inside the mini player* (Deviload's flip side, Limusic's right column) and
  *shuffle, repeat, like, a volume slider and an always-on-top pin toggle* in it. · Size: small
  · Priority: low
* *Mini player anchored to its screen corner when its size changes* (Limusic): it keeps its
  top-left corner today. · Size: small · Priority: low
* *Click the artwork to play / pause, scroll over it for the volume, a "pop" when liking*
  (Limusic), *click the empty space to close Now Playing, open Now Playing on play*. ·
  Size: small · Priority: low
* *15-second skips and a speed button for podcasts in the bar* (Limusic), *an "∞" mark on
  songs added by autoplay*. · Size: small · Priority: low
* *Portrait-window layout for Now Playing* (refryt: cover above the lyrics on tall windows)
  and *a compact player bar on narrow windows* (refryt, Zuno's compact / expanded bar). ·
  Size: medium · Priority: low
* *Lyrics niceties*: a "follow the current line" button after scrolling away (Deviload),
  arrow-key focus over lines (Zuno), a skeleton while loading (refryt), a glow tinted by the
  theme (Deviload). · Size: small · Priority: low
* *A ring of bars around the record* (Zuno's site) for the Turntable design. · Size: small ·
  Priority: low
* *Not ours / won't do*: theater mode, auto-hiding controls, cinema, the ambient light around
  music videos and the song / video switch (the music-video work); a translucent blurred
  window (refryt; the look-themes work); a slow drift of the lyrics backdrop and an app-wide
  wash that pulses with the audio (continuous frames); a gapless switch (always gapless
  without a crossfade); volume above 100 % (clips); a phone remote with a QR code and a
  sidebar "now playing" flip card (Deviload; outside Now Playing).

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

**F3. Scrolling slows down when a page holds more artwork than the image cache**
When every shelf of a long Home brings its own covers (~700 images, more than the 48 MB
artwork budget), drawing the end of the feed got up to 1.8x slower on the Windows CI runner
even with culling on, most likely because the artwork cache evicts and re-decodes images as
they scroll back into view. Normal use stays well under the budget, so the CI frame-time
gate keeps its fixture artwork within it. Ideas: decode at display size before caching,
evict by distance from the viewport rather than LRU, keep small thumbnails for culled
shelves, or raise the budget on machines with plenty of memory. Repro: `MUSIC_FIXTURES`
with per-page artwork in `scripts/perf-frames.sh` (see the comment in
`crates/music-core/src/fixtures.rs` and docs/TESTING.md 6.3). · Size: medium · Priority: low


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
Full streams depend on YouTube's current client behaviour (visionOS, TV, TV Simply + PoToken,
web + PoToken, and the `n`/signature solver from yt-dlp/ejs 0.8.0). Now a broken
route costs at most three songs before the circuit breaker skips it. A failed solve refetches
`player.js` once, and the signed player fixes (`packaging/player-fixes.json`) can change client
versions, order and player pins without a release. A change that needs code (a new request
shape, a solver update) still needs an app update, and the preview is the last resort. Keep the
vendored solver current, run the live examples (`youtube -- auto`), and publish player fixes
([RELEASING.md](RELEASING.md#player-fixes-packagingplayer-fixesjson)) when a client version
rotates. Details: [README: Full-length YouTube
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
