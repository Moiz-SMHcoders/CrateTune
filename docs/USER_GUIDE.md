# User guide

"Music" is the working name of this app: a native desktop player for YouTube Music on
macOS, Windows and Linux. It is free software (GPL-3.0) and not affiliated with YouTube,
Google or Shazam.

* [Install](#install)
* [First start](#first-start)
* [Finding music](#finding-music)
* [Playing](#playing)
* [Now Playing and lyrics](#now-playing-and-lyrics)
* [Recognize music](#recognize-music)
* [Library](#library)
* [Your own music files](#your-own-music-files)
* [Signing in](#signing-in)
* [Settings](#settings)
* [Keyboard shortcuts](#keyboard-shortcuts)
* [Modules](#modules)
* [Privacy](#privacy)
* [Known limitations](#known-limitations)
* [Troubleshooting](#troubleshooting)

## Install

**macOS (11 Big Sur or later).** Open the `.dmg` and drag the app to Applications.
Builds that aren't signed with a Developer ID (the default) are refused by Gatekeeper on
first open: right-click the app → **Open** → **Open**, or allow it under System Settings →
Privacy & Security. The first time you recognize a song, macOS asks for microphone access.

**Windows 10/11 (x64).** Run the `…-setup.exe` (installs for the current user, no admin
rights) or unpack the `.zip` anywhere and start the `.exe`. SmartScreen may warn about an
unsigned download: **More info → Run anyway**.

**Linux (x86_64).** Either the AppImage (`chmod +x …AppImage` and run it) or the
`.tar.gz`: unpack it and run `./install.sh` (installs into `~/.local`; `PREFIX=/usr/local
sudo ./install.sh` for everyone). The app needs a Vulkan driver, Wayland or X11,
fontconfig, ALSA and D-Bus (all standard on desktop distributions).

**From source:** see "Install / build" in the [README](../README.md#install--build).

## First start

A short guide opens on the first start: content language and country, theme, optional
YouTube Music sign-in, optional music folders, and a note about privacy and previews.
Everything can be skipped (**Skip setup**, or Esc) and changed later in Settings. Open
the guide again from **Settings → About → Show welcome guide**.

## Finding music

* **Search** (⌘F): type to get suggestions (songs, artists, albums) and recent searches;
  ↑/↓ and Enter pick one. The search page has an **All** overview and tabs for songs,
  videos, albums, artists, playlists, podcasts and episodes; more results load as you
  scroll.
* **Home** shows YouTube Music's personalized feed (mood chips at the top), **Explore**,
  **Charts** (pick a country), **Moods & genres** and **New releases** the rest.
* Artist, album and playlist pages have Play, Shuffle, Radio and Add to queue.
* Right-click any song (or use "…") for play next, add to queue, start radio, add to
  playlist, like, download, go to artist / album.

## Playing

* The player bar at the bottom: artwork, title (click to open Now Playing), previous /
  play / next, progress (click or drag to seek), like, sleep timer, queue, mini player,
  volume.
* **Queue** (⌥⌘Q): now playing, up next (drag to reorder, remove), history. Shuffle (⌘S)
  and repeat off / all / one (⌘R) are remembered.
* **Radio** plays an endless mix based on a song, artist, album or playlist.
  **Autoplay similar songs** (Settings → Content & region) keeps playing related songs
  when the queue ends.
* **Mini player** (⇧⌘M): a small window that stays on top.
* **Sleep timer**: the moon button in the bar — 5 to 60 minutes, end of the song, or a
  custom length.
* **Media keys** and the system's now-playing controls work on all three systems; on
  macOS there is also a menu-bar item.

## Now Playing and lyrics

Open with ⌘L or by clicking the bar's title. Three layouts (artwork + lyrics side by side,
big artwork, lyrics first) and four backgrounds (blurred artwork, gradient, solid, glow),
from the layout menu in its header or Settings → Appearance.

Lyrics come from several providers (word-by-word where available). Click a line to jump
there. Translation (Google or your own AI endpoint) and romanization (Japanese, Korean,
Chinese, Hindi, Cyrillic, …) are in Settings → Lyrics.

## Recognize music

**Recognize** in the sidebar (or ⇧⌘R, or View → Recognize Song) listens through the
microphone for about 10 seconds and identifies the song. The result shows title, artist,
album, artwork and links (Shazam, Apple Music, Spotify, YouTube) and searches YouTube
Music for it: **Play**, **Add to queue**, **Radio** or **Search**. Click the button again
(or **Cancel**) to stop listening early.

Recognized songs are kept in a history below (newest first, up to 50); click one to see it
again, × removes it, **Clear** empties the list.

Only an audio fingerprint (frequency peaks, a few hundred bytes) is sent to Shazam's
service — never the recording. If access was denied, **Open privacy settings** jumps to
the system's microphone settings (macOS: System Settings → Privacy & Security →
Microphone).

## Library

Everything is stored locally (SQLite database in the settings folder):

* **Library** — songs you added, liked songs, albums, artists, playlists, downloads and
  local files, with filters, sorting and multi-select (**Select**).
* **Playlists** — create local playlists (and, when signed in, YouTube Music playlists),
  rename, reorder, delete.
* **History** — every song you listened to for at least 30 s, by day.
* **Stats** — listening time, top songs / artists / albums, by hour and weekday; **Year in
  Music** per year.
* **Downloads** — downloaded songs play from disk (see the limitation below).

## Your own music files

**Local files** in the sidebar (or the onboarding guide): add music folders, then
**Rescan**. MP3, M4A/AAC/ALAC, FLAC, Ogg Vorbis/Opus, WAV and AIFF are read with their
tags, cover art, embedded lyrics and ReplayGain. Local songs show up in the library and
play in full.

## Signing in

Optional — browsing, search and playback work without an account. Signing in adds your
likes, library, playlists, subscriptions and personalized recommendations.

* **macOS:** Settings → Account → **Sign in with Google…** opens Google's sign-in page in a
  window; when you reach YouTube Music the window closes.
* **Everywhere:** **Paste cookie…** — copy the `Cookie` request header of
  music.youtube.com from your browser's developer tools (step-by-step instructions are in
  the dialog).

The sign-in cookie is stored in the system keychain and only sent to YouTube. **Sync now**
copies your YouTube Music library into the local one; **Sign out** forgets the cookie
(the synced data stays).

## Settings

⌘, or the sidebar. Sections:

| Section | What's there |
|---|---|
| Appearance | theme (dark, pure black, dynamic accent from the artwork), text size, Now Playing layout and background, mini player style |
| Playback | shuffle / repeat, quality, loudness normalization, skip silence, skip failing songs, crossfade, speed and pitch, output device, sleep timer |
| Modules | provider modules (alternative audio sources) and where songs play from |
| Equalizer | 10-band equalizer with presets |
| Lyrics | look, animation style, providers and their order, translation, romanization |
| Content & region | content language and country, hide explicit, hide videos, autoplay similar songs |
| Network | HTTP / HTTPS / SOCKS proxy with test |
| Privacy | pause / clear listen and search history |
| Storage | file locations, play cache, downloads, backup and restore |
| Account | YouTube Music sign-in, channels, sync |
| Scrobbling & Discord | Last.fm, ListenBrainz, scrobble rules, Discord Rich Presence |
| About | version, license, credits, welcome guide, update check (when enabled) |

Settings live in `~/Library/Application Support/music-macos/` (macOS),
`~/.config/music-macos/` (Linux) or `%APPDATA%\music-macos\config\` (Windows); passwords
and keys in the system keychain.

## Keyboard shortcuts

macOS keys shown. On Windows and Linux the same bindings currently use the Windows /
Super key in place of ⌘ and Alt for ⌥.

| Keys | Action |
|---|---|
| Space | play / pause |
| ⌘→ / ⌘← | next / previous |
| ⌘↑ / ⌘↓ | volume up / down |
| ⌘F | search |
| ⌘, | settings |
| ⌘L | Now Playing |
| ⌥⌘Q | queue |
| ⇧⌘M | mini player |
| ⇧⌘R | recognize a song |
| ⌘S / ⌘R | shuffle / repeat |
| ⌘[ / ⌘] | back / forward (also the mouse's back / forward buttons) |
| ⌘1 / ⌘2 / ⌘3 / ⌘Y | Home / Explore / Library / History |
| Esc | close the welcome guide, Now Playing or the queue; else back |
| ⌘Q | quit |

## Modules

A **module** is a small add-on (an `.arch` file) that can supply the audio for the songs you
play from another source you have access to, for example a service that offers lossless
files. The song stays the same song — title, artists, lyrics, your likes and playlists don't
change; only the audio comes from elsewhere. **The app ships with no modules** and doesn't
recommend any: you get them from their authors. Install only modules you trust, like any
other software; they run in a sandbox (no access to your files, network only through the
app, the same proxy).

**Installing.** Settings → Modules → *Add a module*:

* **Install from file…** — pick an `.arch` file;
* **Install from URL** — paste the link to an `.arch` file (or to a small "descriptor" that
  points to the current version);
* **Add catalog** — paste a catalog link; its modules are listed under *Catalogs* with an
  **Install** (or **Update**) button.

Every package is checked first (format, checksum, settings, what it provides). A review card
shows the name, version, author, what it can do and which settings it needs; press
**Install**. If it needs settings (an account token, a server, a quality), its settings form
opens: fill it in and press **Save**. Passwords and tokens are kept in the system keychain.

**Using modules.** Choose under *Playback source*:

* **YouTube only** — modules are not used (the default);
* **Modules first, then YouTube** — each song is looked up in your enabled modules, top to
  bottom; the first module with the same recording plays it, otherwise YouTube does;
* **Modules only** — songs no module has fail (and are skipped when "Skip failing songs" is
  on).

The bar and Now Playing show where the audio comes from and its quality, e.g. "YouTube · AAC
128 kbps" or "*Module name* · FLAC 16/44.1". Songs from a module play in full (the YouTube
one-minute preview limit doesn't apply to them) and downloads use the same order. A module
only plays a song when title, artist and length agree closely, so a live version or a remix
is not swapped in for the studio recording. Your own music files never go through modules.

**Managing.** Each installed module has an on/off switch, arrows to change the order,
**Settings**, **Check** (does it load and is it configured), **Check for update** (for
modules installed from a link; re-import the file otherwise) and **Uninstall** (removes its
settings too). Under *Settings* → *Diagnostics* you see the last check, what happened with
the last song (played, no match, or an error code) and the module's own log; **Copy
diagnostics** copies a report without your settings or tokens, for the module's author.

**For module authors:** the format is ArchiveTune's `.arch` module format, API v1; see
"Provider modules" in the [README](../README.md#provider-modules-arch) for the APIs and limits
of this app and the packaging tool.

## Privacy

* Your library, history, stats and settings stay on your computer; nothing is sent to the
  developers.
* YouTube receives your searches and what you play (with your account cookie when signed
  in). Lyrics providers receive song titles and artists. Recognition sends an audio
  fingerprint to Shazam. Last.fm / ListenBrainz / Discord only get data if you set them up.
* When an update repository is configured, the app asks GitHub once per launch for the
  latest release (Settings → About); nothing is downloaded automatically.

## Known limitations

* **Previews only for YouTube songs.** Without a PoToken (which YouTube's own apps obtain
  through an anti-abuse challenge), YouTube serves only about the first minute of each
  song; the app doesn't work around this. Songs are marked "preview" in the player, the
  next song starts when the preview ends, and downloads are previews too. Your own music
  files, and songs played through a [module](#modules), play in full.
* The YouTube Music account features are new and not yet tested against many accounts.
* Recognition uses Shazam's public endpoint, which may rate-limit ("Too many requests —
  try again in a minute") or change without notice.
* Accessibility (screen readers) is limited by the UI toolkit.
* Builds are not signed with a Developer ID / Authenticode certificate yet (see Install).

## Troubleshooting

* **No sound:** check the output device in Settings → Playback and the volume in the bar.
* **"Couldn't play … skipped":** the song could not be loaded (region, removed video);
  turn off "Skip failing songs" in Settings → Playback to stop on errors instead.
* **Recognition hears only silence:** allow microphone access (see above); check that the
  right input device is selected in the system's sound settings.
* **Nothing loads:** check Settings → Network (proxy) → **Test connection**.
* **Start over:** quit and remove (or rename) the settings folder listed under Settings.
