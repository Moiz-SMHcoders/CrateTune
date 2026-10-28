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
* [Listen together](#listen-together)
* [Cast to a TV or speaker](#cast-to-a-tv-or-speaker)
* [AI content filter](#ai-content-filter)
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
  scroll. When YouTube corrects a typo, the page says **Showing results for …** with a
  **Search instead for …** link (or **Did you mean …?**); recent searches keep what you
  typed.
* **Home** shows YouTube Music's personalized feed (mood chips at the top), **Explore**,
  **Charts** (pick a country), **Moods & genres** and **New releases** the rest.
* Artist, album and playlist pages have Play, Shuffle, Radio and Add to queue. Long
  albums (over 200 tracks) load the rest of their tracks automatically.
* **Podcasts**: a show's page (from search, Home or Explore) has its description, Play /
  Shuffle / Add to queue, **Save to library** (signed in) and the episodes with date,
  length and description — more load as you scroll. Click an episode to play it;
  right-click → **Open episode page** for its full description. Saved shows are in
  Library → **Podcasts** (signed in).
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
there. Songwriter / producer credits that some providers put above the lyrics are left out,
and songs the providers know as instrumentals show no lyrics. Translation (Google or your
own AI endpoint) and romanization (Japanese, Korean, Chinese, Hindi, Cyrillic, …) are in
Settings → Lyrics. A song is translated once per language and kept, so opening it again
doesn't wait for the translator.

**Kanji in Japanese romanization** need a dictionary: Settings → Lyrics → Romanization →
**Kanji readings** → Download (10.5 MB, about 45 MB on disk; it also starts when you turn
Japanese romanization on). Until it is there, only kana are romanized and kanji stay as
they are. Remove deletes it again.

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
  rename, reorder, delete. Your own YouTube Music playlists get **Change cover…** (any
  JPEG / PNG / WebP; cropped to a square and uploaded).
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
| Content & region | content language and country, hide explicit, hide videos (also inside playlists, albums, radio and queues), autoplay similar songs |
| Network | HTTP / HTTPS / SOCKS proxy with test; DNS-over-HTTPS (Cloudflare, Google, Quad9 or your own URL, with optional fallback to the system resolver) and **Test DNS** |
| Privacy | pause / clear listen and search history |
| Storage | file locations, play cache, downloads, backup and restore |
| Account | YouTube Music sign-in, channels, sync |
| Scrobbling & Discord | Last.fm, ListenBrainz, scrobble rules, Discord Rich Presence |
| SponsorBlock | skip non-music sections and other segments |
| Music Together | your name, port, guest permissions, relay server |
| Spotify import | your Spotify app's Client ID for importing playlists |
| AI content filter | endpoint, model, key, categories to hide |
| Chromecast | devices, stop casting, direct fetch |
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

## Listen together

Play the same music in sync with others (sidebar → **Listen together**).

* **Start session** hosts on your network; **Host via relay** hosts through a relay server
  (set one in Settings → Music Together → *Relay server*, e.g. `wss://relay.example.com`),
  so people on other networks can join. Share the link; it contains a random key.
* Guests paste the link and press **Join**. Their player follows yours; "Play next" /
  "Add to queue" become suggestions to the host, and, if you allow it, they can play,
  pause, seek and skip.
* In the people list you can let people in (with approval on), remove or ban them, or
  **Make host**: that guest takes over the session with your queue and position, and
  everyone, including you, follows them from then on.
* The relay server is a small program you run yourself (see the README, "Relay server");
  this app doesn't come with a public one.

## Cast to a TV or speaker

* The **Cast** button in the player bar lists Chromecasts, Google TVs and Nest speakers on
  your network (it looks for them when you open it; "Search again" if yours is missing).
  Pick one: the current song continues there. Play / pause, seeking, next / previous and
  the volume slider now control the device; the bar says "Casting to …".
* **Stop casting** (in the same menu or Settings → Chromecast) continues the song on this
  computer.
* The audio is streamed from this app to the device, so keep the app running and both on
  the same network; if the system asks whether to accept incoming connections, allow it. The sleep timer, equalizer and speed don't apply while casting.
* Not yet tested with real devices; please report problems.

## AI content filter

Optional and off by default (Settings → **AI content filter**). It hides songs and albums
that a language model puts into categories you pick — explicit lyrics, AI-generated music,
holiday songs, children's music, sleep sounds and noise, sped-up / slowed edits, covers and
karaoke, live recordings — plus your own ("songs about cars"), like "Hide explicit content".

* You need an OpenAI-compatible service: OpenAI, OpenRouter, or a model on your own
  computer (Ollama: endpoint `http://localhost:11434/v1`, LM Studio, llama.cpp server).
  Enter the endpoint, the model name and, if the service needs one, an API key.
* Each song is asked about once; the answers are remembered on your computer ("Forget
  classifications" clears them). Songs the model hasn't judged yet stay visible.

## Privacy

* Your library, history, stats and settings stay on your computer; nothing is sent to the
  developers.
* YouTube receives your searches and what you play (with your account cookie when signed
  in). Lyrics providers receive song titles and artists; translators receive the lyrics.
  The kanji dictionary is downloaded from lindera's GitHub releases. Recognition sends an audio
  fingerprint to Shazam. Last.fm / ListenBrainz / Discord only get data if you set them up.
* When an update repository is configured, the app asks GitHub once per launch for the
  latest release (Settings → About); nothing is downloaded automatically.
* The **AI content filter** (off by default) sends the titles, artists and albums of the
  songs on the pages you open to the service you configured, and nothing while it is off.
* A **relay server** (only if you set one) forwards your Listen together session: song
  titles, names in the session, the session key.
* **Casting** looks for devices on your network only when you open the Cast menu or the
  Chromecast settings.

## Known limitations

* **How YouTube songs are streamed.** Full songs come from YouTube's TV client, or its web
  client with a PoToken (an anti-abuse token the app makes in a hidden browser view on
  macOS). YouTube makes each new stream wait a few seconds; the app plays the start of the
  song from a preview meanwhile, so songs usually start within a second (two or three for
  the first song after launch). Choose the
  route in Settings → Playback → **YouTube streams** (Automatic is best). Windows makes the
  PoToken with the built-in WebView2 (new, untested); Linux builds can opt in with
  WebKitGTK; otherwise they use the TV client. If YouTube changes
  something, songs fall back to ~1-minute previews (marked "preview") until the app is
  updated. This isn't allowed by YouTube's terms of service.
* The YouTube Music account features are new and not yet tested against many accounts
  (playlist cover upload has not been tried against a real account at all).
* Quad9's DNS-over-HTTPS is reached on port 5053 (its port-443 server only speaks
  HTTP/2); some networks block that port.
* Recognition uses Shazam's public endpoint, which may rate-limit ("Too many requests —
  try again in a minute") or change without notice.
* Google Translate's free endpoint rate-limits busy connections ("rate-limited this
  connection; try again in N min"); the app then leaves it alone for a while and retries by
  itself. An AI endpoint of your own avoids this.
* Accessibility (screen readers) is limited by the UI toolkit.
* Builds are not signed with a Developer ID / Authenticode certificate yet (see Install).

## Troubleshooting

* **No sound:** check the output device in Settings → Playback and the volume in the bar.
* **"Couldn't play … skipped":** the song could not be loaded (region, removed video);
  turn off "Skip failing songs" in Settings → Playback to stop on errors instead.
* **Recognition hears only silence:** allow microphone access (see above); check that the
  right input device is selected in the system's sound settings.
* **Nothing loads:** check Settings → Network (proxy) → **Test connection**; with
  DNS-over-HTTPS on, **Test DNS** shows whether names resolve (turn on the fallback, or
  pick another provider, if the DoH server is blocked on your network).
* **Start over:** quit and remove (or rename) the settings folder listed under Settings.
