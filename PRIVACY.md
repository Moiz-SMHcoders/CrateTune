# Privacy

CrateTune has **no telemetry**: no analytics, no crash reporting, no usage statistics and no
account of its own. It sends nothing to the project or its maintainers. It talks to the
services below so that it can do what you ask, and it keeps everything else on your computer.

This page lists every place the app connects to, what each one receives, and how to turn it
off. It describes the code in this repository; if a change here makes it wrong, that is a bug.

## Contents

* [What it connects to](#what-it-connects-to)
  * [YouTube and YouTube Music](#youtube-and-youtube-music)
  * [Lyrics providers](#lyrics-providers)
  * [Lyrics translation](#lyrics-translation)
  * [Updates (GitHub)](#updates-github)
  * [Music Together](#music-together)
  * [SponsorBlock](#sponsorblock)
  * [Last.fm and ListenBrainz](#lastfm-and-listenbrainz)
  * [Discord](#discord)
  * [Spotify (playlist import)](#spotify-playlist-import)
  * [AI providers](#ai-providers)
  * [DNS-over-HTTPS and proxies](#dns-over-https-and-proxies)
  * [Chromecast](#chromecast)
  * [Song recognition (Shazam)](#song-recognition-shazam)
  * [Downloads you ask for](#downloads-you-ask-for)
* [What it stores](#what-it-stores)
* [Removing your data](#removing-your-data)

## What it connects to

| Service | When | On by default |
|---|---|---|
| YouTube / YouTube Music | always (it is a YouTube Music client) | yes |
| Lyrics providers | when a song starts | yes (each can be turned off) |
| GitHub (update check) | at start, then every 24 hours | yes, except package-manager installs |
| GitHub (player fixes), relay as fallback | at start, then every 6 hours | yes (Settings → Playback → *Player fixes*) |
| GitHub (`relay.txt`) and the Music Together relay | only when you host or join a session | only when used |
| SponsorBlock | when a song starts | yes |
| Google Translate or an AI endpoint | when lyrics translation is on | no |
| Last.fm, ListenBrainz | while scrobbling is set up | no |
| Discord (local app only) | while Rich Presence is on | no |
| Spotify | while you import playlists | no |
| AI endpoint (content filter) | while the filter is on | no |
| DNS-over-HTTPS resolver | while DoH is on | no |
| Chromecast devices (local network) | when you open the Cast picker | only when used |
| Shazam | when you recognize a song | only when used |

### YouTube and YouTube Music

Hosts: `music.youtube.com`, `www.youtube.com`, `*.googlevideo.com` (the audio itself),
`i.ytimg.com`, `lh3.googleusercontent.com`, `yt3.ggpht.com`, `yt3.googleusercontent.com`
(artwork), and `accounts.google.com` when you sign in.

* **What they receive:** searches (suggestions are fetched as you type), the pages you open,
  the songs you play, likes and playlist changes, and your interface language and country
  (Settings → Content & region). Like any website, YouTube sees your IP address.
* **Signed in**, requests carry your YouTube session cookie, so YouTube knows which account
  makes them. Library sync ("Sync with YouTube Music automatically", on by default when signed
  in) reads your library at start, every 45 minutes and when you come back.
* **Signed out**, YouTube hands out an anonymous visitor id. CrateTune keeps it in its library
  database and reuses it, so YouTube can tell your requests apart from other people's, but not
  who you are.
* **Plays go to your YouTube history** when you're signed in and *Add plays to my YouTube
  history* (Settings → Account) is on, which it is by default: about 5 seconds into a song the
  app sends YouTube's playback ping, then a watch-time ping every 30 seconds and at the end,
  so the song shows up in YouTube Music on your other devices. Nothing is sent while listen
  history is paused (Settings → Privacy), for local files, or when signed out. The cookie only
  ever goes to `*.youtube.com`. YouTube still sees which songs the app asks streams for either
  way.
* **"Jump back in"** on Home reads the newest entry of your YouTube Music history (the same
  request as the History page) to offer resuming what you played on another device.
* **Streams:** to get full-length streams, the app may fetch YouTube's player script and run
  YouTube's anti-abuse check (a PoToken) in a hidden web view (macOS, Windows). Settings →
  Playback → *YouTube streams* chooses how.
* **Sign-in** happens in a web view showing Google's own page. CrateTune keeps the resulting
  `.youtube.com` cookies in the system keychain, and the web view's own profile so that an
  expired session can be renewed. Signing out deletes both. With several accounts, each one
  has its own keychain entry and its own sign-in profile.
* **Sign in from your browser** (optional) reads the cookie database of the browser profile
  you pick, on this computer only. The database is copied to a private temporary folder,
  only its `youtube.com` rows are read, and the copy is deleted straight away. Decrypting them
  needs the browser's key: on macOS the system asks whether CrateTune may read that browser's
  "Safe Storage" keychain item; Safari's cookies need Full Disk Access. Nothing from the
  browser is sent anywhere except the YouTube cookie to YouTube, to check the account.

Google's privacy policy applies: <https://policies.google.com/privacy>.

### Lyrics providers

When a song starts, CrateTune asks the lyrics providers for it, a few at a time: the fastest
ones first, the others only if those find nothing good (*Ask sources in waves*), in the order
set in Settings → Lyrics → *Providers*. Each provider asked receives the song's **title,
artist, album and length**; some receive its **YouTube video id** instead. Nothing about you
is sent. Found lyrics are cached on your computer, so a song is looked up once; a source you
pick for a song and your timing nudges are kept in `lyrics-tracks.json` next to the settings.

| Provider | Host | Receives |
|---|---|---|
| BetterLyrics (and Portato) | `lyrics-api.boidu.dev` | title, artist, album, length |
| YouLyPlus | `lyricsplus.binimum.org`, `lyricsplus.prjktla.my.id`, `lyricsplus.prjktla.workers.dev`, `lyricsplus.atomix.one`, `lyricsplus-seven.vercel.app` (mirrors, tried in turn) | title, artist, album, length |
| Binimum | `lyrics-api.binimum.org`, then the lyrics file it points to (for example on `lrc.red`) | title, artist, album, length |
| LRCLIB | `lrclib.net` | title, artist, album, length |
| Kugou | `mobileservice.kugou.com`, `lyrics.kugou.com` | title, artist, length |
| Megalobiz | `www.megalobiz.com` | title, artist |
| SimpMusic | `api-lyrics.simpmusic.org` | the video id |
| Unison | `unison.boidu.dev` | title, artist, album, length, or the video id |
| Paxsenix (Apple Music, Spotify, Musixmatch) | `api.paxsenix.org` | title, artist, length; **only if you enter an API key** |
| YouTube Music | `music.youtube.com` | the video id, without your cookie |

Turn any provider off in Settings → Lyrics → *Providers*.

### Lyrics translation

Off by default (Settings → Lyrics → *Translate lyrics*). When on, the lyric lines (not the
title or artist) are sent to the translator you choose: **Google Translate**
(`translate.googleapis.com`, no account) or an **AI endpoint** you configure (see
[AI providers](#ai-providers)).

### Updates (GitHub)

* **Update check:** at start and every 24 hours, the app asks `api.github.com` for the list of
  CrateTune releases. The request carries no information about you or what you play, only what
  any download does (your IP address and the app's user agent). Turn it off with *Check for
  updates automatically* in Settings → About; *Check now* still works.
* **Update download:** only from `github.com/Moiz-SMHcoders/CrateTune/releases` (redirected to
  `objects.githubusercontent.com` / `release-assets.githubusercontent.com`), and only when you
  choose to update or turn on *Install updates automatically* (off by default). Every download
  is checked against the release's `SHA256SUMS.txt` before it is installed.
* **Package-manager installs** (Flatpak, Homebrew, Scoop, AUR, Snap) **never check**: they are
  updated by the package manager, and Settings → About says "Updates come from …".
* **Player fixes:** at start and every 6 hours (sooner when no stream route works), the app
  downloads `packaging/player-fixes.json` and its signature from this repository
  (`raw.githubusercontent.com`), or from `relay.smhcoders.click` if GitHub can't be reached.
  It is a small signed data file with YouTube client settings; nothing about you or what you
  play is sent. Turn it off with *Player fixes* in Settings → Playback.
* **`relay.txt`:** when you host or join a Music Together session, the app reads
  `packaging/relay.txt` from this repository (`raw.githubusercontent.com`) to find the current
  relay, and caches the answer for six hours.

GitHub's privacy statement applies:
<https://docs.github.com/site-policy/privacy-policies/github-general-privacy-statement>.

### Music Together

* **Over the internet** (the default way to host): the session goes through the relay server,
  `wss://relay.smhcoders.click` unless you set another one in Settings → Music Together →
  *Relay server*. The relay passes on what the session shares: everyone's **display name**, the
  playing song's **title, artists, length, video id and artwork link**, the queue and the
  playback controls. It sees the participants' IP addresses. Traffic to the relay is encrypted
  (TLS), but not end to end: the relay could read it. The relay keeps nothing once a session
  ends and logs nothing unless its operator turns on verbose logging; it never sees your
  YouTube account. The server code is in this repository (`crates/music-relay`).
* **Display name:** you choose it; until you do, it is your computer user name, capitalized.
  Change it in Settings → Music Together before hosting if you'd rather not share it.
* **Same network only:** guests connect straight to the host's computer (TCP port 42117).
  Nothing leaves your network.

### SponsorBlock

**On by default** (Settings → SponsorBlock → *Skip segments*). When a song starts, the app asks
`sponsor.ajay.app` for community-marked segments to skip. It sends only the **first four
characters of the SHA-256 hash of the video id**, so the server receives a bucket shared by
many videos and can't tell which song you play. Local files are never looked up. The server is
configurable (*API URL*).

### Last.fm and ListenBrainz

Off until you set them up with your own account.

* **Last.fm** (`ws.audioscrobbler.com`; sign-in opens `www.last.fm` in your browser): the
  playing song (artist, title, album, length) as "now playing", then a scrobble with its start
  time once it counts (Settings → Scrobbling & Discord → *When a song counts*).
* **ListenBrainz** (`api.listenbrainz.org`): the same, plus a link to the song on
  `music.youtube.com`.

### Discord

Off by default. With *Discord Rich Presence* on, CrateTune talks only to the **Discord app on
your computer** through its local socket; it makes no requests to Discord's servers. Discord
then shows the song title, artist, cover and a "Listen on YouTube Music" link to people who can
see your profile, as set in Discord.

### Spotify (playlist import)

Used only when you import playlists from Spotify, with a Spotify developer app you register
yourself. Sign-in happens in your browser (`accounts.spotify.com`, read-only scopes); the app
then reads your playlists and liked songs from `api.spotify.com` (covers from `i.scdn.co`) and
searches YouTube Music for each track. The refresh token is kept in the keychain; disconnecting in
Settings → Spotify import deletes it.

### AI providers

Off by default, and only with an endpoint and key you enter (any OpenAI-compatible service,
such as OpenAI, OpenRouter or a local server):

* **Lyrics translation with AI:** the lyric lines and the target language.
* **AI content filter** (Settings → AI content filter → *Filter songs with an AI model*): the title,
  artists and album of the songs on the pages you open, to decide what to hide. Nothing about
  you and no listening history is sent. Verdicts are cached on your computer.

The provider's own privacy policy applies to what you send it.

### DNS-over-HTTPS and proxies

Both off by default (Settings → Network).

* **DNS-over-HTTPS:** host names are resolved by the provider you pick (Cloudflare
  `cloudflare-dns.com`, Google `dns.google`, Quad9 `dns.quad9.net`, or your own URL) instead of
  your system resolver. The provider sees which hosts the app looks up.
* **Proxy:** HTTP, HTTPS or SOCKS. It carries the app's own requests (YouTube, streams,
  artwork, lyrics, scrobbling, updates). The sign-in and PoToken web views use the system's
  network settings.

### Chromecast

Only when you open the Cast picker: the app looks for Cast devices on your **local network**
(mDNS), and while casting it serves the song to the device from your computer and sends it the
title, artist, album, length and artwork link. The device fetches the artwork itself. With
*Let the device fetch YouTube audio itself* (off by default), the device downloads the stream
from YouTube directly.

### Song recognition (Shazam)

Only when you start a recognition. The app listens to the microphone (or reads the file you
pick) for about ten seconds and computes an **audio fingerprint** on your computer. It sends
`amp.shazam.com` the fingerprint, never the recording, plus a timestamp and your time zone. The
request ids are random each time; there is no account, device id or location (the location
field is fixed at 0, 0). Recognition history stays on your computer.

### Downloads you ask for

* The **Japanese reading dictionary** (Settings → Lyrics → *Kanji readings*), from
  `github.com/lindera/lindera` releases, checked against a fixed SHA-256.
* **Provider modules** you install yourself (none come with the app). A module receives the
  searches made through it and can make web requests of its own; review a module before
  installing it.

## What it stores

Everything stays on your computer, in your user account:

| What | Where |
|---|---|
| Settings (`settings.json`, owner-only permissions), library database (`library.sqlite`: library, playlists, listen and search history, lyrics cache, recognition history), downloads and the optional play cache (`media/`), profiles, modules | macOS `~/Library/Application Support/cratetune`, Linux `~/.config/cratetune`, Windows `%APPDATA%\cratetune\config`, Flatpak `~/.var/app/io.github.moiz_smhcoders.cratetune/config/cratetune` |
| Caches: update downloads and `update.log`, the `relay.txt` answer, the Japanese dictionary | macOS `~/Library/Caches/cratetune`, Linux `~/.cache/cratetune`, Windows `%LOCALAPPDATA%\cratetune\cache` |
| Sign-in web view profile | macOS `~/Library/WebKit/io.github.moiz-smhcoders.cratetune`, Windows `%LOCALAPPDATA%\cratetune\data\SignIn` (further accounts: `SignIn-<id>`; on macOS 14+ a separate WebKit data store per account) |
| Secrets (below) | the system keychain |

**Keychain.** Secrets go to the operating system's credential store (macOS Keychain, Windows
Credential Manager, the Secret Service keyring on Linux) under the service name `cratetune`,
never in plain files: the YouTube sign-in cookie (one per saved account), the Last.fm secret and session, the
ListenBrainz token, the Spotify refresh token, the proxy password, and API keys for lyrics
providers and AI endpoints. Only if no keychain is available are they kept in `settings.json`,
which only your user can read.

**History** stays on your computer. Settings → Privacy pauses or clears listen and search
history. Backups you export contain your library, playlists, history and settings, but no
passwords or cookies.

**Logs.** Diagnostics stay on your computer. The app keeps its recent log lines in memory
for Settings → About → Logs and, unless you turn off *Write a log file* there, in a rotating
file (`logs/` in the cache folder, at most about 2 MB). Cookies, tokens, keys, signatures and
your stream IP are removed from every line before it is shown, kept or written. The updater
writes its own `update.log` in the cache folder. Nothing is uploaded and there are no crash
reports.

## Removing your data

Uninstalling leaves your data in place. To remove it, delete the folders above, and the
`cratetune` entries in your keychain (Keychain Access on macOS, Credential Manager on Windows,
Seahorse or KWallet on Linux). Signing out in Settings removes the YouTube cookie and the
sign-in web view's data. With Homebrew, `brew uninstall --zap cratetune` removes the folders
(not the keychain entries).

Questions: [open an issue](https://github.com/Moiz-SMHcoders/CrateTune/issues).
