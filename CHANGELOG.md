# Changelog

All notable changes. The format follows [Keep a Changelog](https://keepachangelog.com/),
versions follow [SemVer](https://semver.org/).

## [Unreleased]

### Added

* **Listen together has modes.** **Friends**: everyone plays, pauses, skips, seeks and
  edits the queue, as equals. **Host**: you control playback, the others listen along
  and suggest songs (as before). **DJ**: everyone adds songs, you control playback
  (with "Suggestions need my approval" if you like). Pick one in the new **Start
  listening together** sheet (one line each, your last choice remembered), together
  with **who can join** (anyone with the link or code, or only one of your friend
  groups) and the connection. The mode shows everywhere (a badge on the page and in the
  session panel, an icon on the listeners chip) and can be switched while listening, by
  the host or by anyone in a Friends session, after a confirmation. Joining tells you
  the mode.
* **Your own controls drive the session.** When the session lets you control playback
  (Friends mode, or a host that allows it), the player bar, media keys and the OS's
  now-playing controls (Control Center, MPRIS, SMTC), the mini player, every Now
  Playing design, keyboard shortcuts, the menu-bar / tray items, the queue panel, song
  menus ("Play now", "Play next", "Add to queue", "Play album / playlist / radio") and
  scripts all act on the session. Play / pause go out as intents, a skip names the song
  it skips (two people skipping together skip once), the newest seek wins. When you may
  not, the buttons are disabled with the reason as their tooltip (keys and menus show
  it once in a while, not on every press). Volume, sound and lyrics stay yours.
* **A shared queue.** Each queued song shows who added it (an initial, the name on
  hover); "Next in queue" is one list for everyone, in the order songs were added.
  **Remove from queue** and reordering go through the session: anyone in Friends mode,
  the host plus each person for the songs they added in DJ mode (and in Host mode while
  guests may add songs; anything when guests may control playback). Who removed what
  shows in the activity notes.
* **Presence and reactions.** A listeners chip on the player bar (stacked initials, the
  count, the mode) opens a session panel: the mode and what you may do, who is
  listening, reactions, invite, leave. Quiet **activity notes** ("Ana skipped", "Sam
  added 3 songs") merge and never stack up (Settings → Music Together → *Activity
  notes*). **Reactions** (six) float up over the bar for a moment for everyone,
  rate-limited, and follow Animations (Reduced: a short fade; Off: no motion).
* **Friend groups listen together in one click.** "Listen together" on a group joins the
  group's running session, or starts one in the group's mode (Friends unless you pick
  another in Settings → Friends or with the ▾ next to the button) that only the group
  can join. Members get a notification with **Join**, or join on their own with *Join
  automatically when a friend starts*; the Together page lists each group's live
  sessions.
* **Sessions last.** A Friends session goes on when the device it runs on leaves: it is
  handed to the next person (no break in the music), and if that device vanishes (quit,
  crash, network gone) the others carry the session on by themselves; it ends only when
  the last person leaves. Any session survives a dropped link: the host claims its relay
  room again and guests reconnect for up to a minute ("Reconnecting…"), at the same
  position and without being asked to approve again.
* **Keep the computer awake while playing** (Settings → Playback, on by default): no
  idle sleep while music plays or a Listen together session runs (the screen may still
  turn off).
* The queue panel has a **Listen together** button (start one with this queue, or open
  the session's panel).

### Changed

* The **Listen together page** was reorganized: start (the sheet asks how) and join (one
  field for a room code or a link) side by side; in a session, the mode and what you
  may do, the invite (code and link, or "only this group"), the room settings the mode
  leaves open, the shared queue with who added what, people, reactions and activity.
  "Ask the host" buttons are gone: your own controls do it.

### Fixed

* A guest's play / pause from media keys or Control Center no longer fought the session
  (the player was paused locally and the room put it back).
* Sessions over the internet that dropped after 45 minutes on a flaky link (the host's
  connection to the relay died at the network level and the relay closed the guests)
  now reconnect instead of ending; the drop is logged with its cause.

## [0.3.6] — 2026-11-08

### Fixed

* **Listen together sessions over the internet no longer time out while waiting.** The
  relay drops a host it hasn't heard from in 60 s, and a host with no guests (waiting for
  the first friend, or after the last one left) sent nothing, so the room vanished about a
  minute after it was opened ("timed out" in the relay's log; a friend's link or code then
  led nowhere). Hosts now send a small keepalive after 20 s of silence. Works with the
  relay as deployed; no server update needed.
## [0.3.5] — 2026-11-05

### Added

* **Artist and album names are links wherever a song is shown**: the player bar, every
  Now Playing design, theater mode, the mini player (its layouts and "Up next"), the queue
  panel and Now Playing's queue, song rows (search, history, library, playlists, albums;
  the album name too when the Album column is on), quick picks and an album page's
  artists. Each artist of "A, B & C" is its own link (underlined on hover); ⌘-click /
  Ctrl+click opens the page in a background tab, and a click in Now Playing or the mini
  player shows the page in the main window.
* **Song menus in more places**: right-click the player bar's song info, a row of Now
  Playing's queue design, or the mini player; Now Playing's header has a **⋯** button.
  They list **Go to artist** (one entry per artist, *Go to podcast* for an episode's show)
  and **Go to album**.
* **Choose what each Now Playing design and mini player layout shows.** Every design
  (Split and its stacked arrangement, Big artwork, Lyrics first, Immersive, Card,
  Minimal, Editorial, With queue, Turntable) and every mini player layout has its own
  switches for the elements that make sense there — like, dislike, shuffle, repeat,
  previous / next, seek bar, times, volume, add to playlist, download, artist & album,
  "Playing from", the source / quality tag, the current lyric line, "Up next", and in Now
  Playing's header the close button, sleep timer, sound menu, the ⋯ song menu, Cast and devices and new
  lyrics / queue buttons; the visualizer can be left out per design. So the Turntable can
  have a heart now. Untouched designs look exactly as before. Change them from **Customize
  this view…** in Now Playing's design menu (a panel with a switch per element and *Reset
  to default*), the mini player's new right-click menu, or Settings → Appearance → Now
  Playing / Mini player → **Elements** (with a design / layout picker). Changes apply at
  once, are kept per design, and travel with profiles and the settings export. Added
  buttons sit beside the title where the design has room, else in one row under the
  controls; what doesn't fit goes into a **More** (⋯) menu, and on short windows the
  artwork gives way so nothing overlaps or leaves the window.

### Fixed

* **"Go to artist" was greyed out for many songs** (radio, the queue, history, library
  rows), which come without the artist's channel id. A click on such a name or entry now
  looks the ids up (the song's own `next` answer, else a search for the artist / album
  whose top result has to match the name; the names dim meanwhile) and opens the page. The
  answer is kept for the session and written back to the queue and the library's copy of
  the song.

## [0.3.4] — 2026-11-04

### Fixed

* **Settings → Friends → Create did nothing.** The button updated the app window from
  inside that window's own click, which GPUI refuses; the error was ignored. It now uses
  the click's window (Enter in the name field already worked).
## [0.3.3] — 2026-11-03

The biggest release so far: page tabs, sidebar playlists and glass looks, a sound round
(fine reverb, convolution rooms, presets, EQ curve, night mode, a perceptual volume
slider), mini player and Now Playing extras with a spinning record, device sync and friend
groups, lower memory, browser sign-in from Firefox-family browsers (also on Windows), and
macOS updates that keep you signed in.

### Added

* **The sidebar slides.** Switching between the full sidebar and the icon rail, and
  *Expand on hover*, animate the width (~200 ms, eased; titles fade, icons glide to the
  rail's position; frames only while it moves, none at rest; follows Animations →
  Reduced / Off). A button in the sidebar's header collapses it to the rail, expands the
  rail, or keeps the hover panel open.
* **Your playlists in the sidebar**, as on YouTube Music and in refryt: Liked songs, the
  account's YouTube Music playlists (while signed in; from the library's sync) and playlists
  made in the app, under the sidebar's entries. Click opens the playlist (the open one is
  highlighted); right-click plays, shuffles, plays next, adds to the queue or pins it to the
  top (also from Library → Playlists). Options in Appearance → Layout → **Sidebar
  playlists**: show or hide the section, YouTube / local / Liked, order (Recent, A–Z, or
  Custom: drag a playlist in the sidebar), how many before *Show all* (5, 10, 20, all) and
  small covers or initials. Works in the icon rail (covers with tooltips) and the hover
  panel; nothing moves up or down when the sidebar slides.
* **Sidebar items.** Appearance → Layout → **Sidebar items**: show or hide each entry
  (Home, Explore, Charts, … Listen together, and the Library heading) and reorder them with
  the arrows or by dragging; *Reset* restores the shipped sidebar. Settings stays at the
  bottom. Scripts: `@sidebar-items=charts:off`, `@sidebar-playlists=sort:name`.
* **More see-through options** (Appearance → Window): **presets** (Subtle, Glass,
  Frosted); each **surface**'s own opacity (sidebar, top bar, player bar, queue panel; Auto
  keeps the previous look); a **tint** over the translucent surfaces (accent, frost, smoke,
  ice, rose, amber; 8–35 %); on macOS a **blur strength** (System, Light, Medium, Strong:
  the window server's blur radius, as refryt and terminals use) and a **Vibrant**
  background with macOS's own materials (Sidebar, Window, Menu, Popover, HUD, Full screen;
  in the app's light or dark mode); on Windows 11 an **Acrylic** background. Every surface's
  opacity is raised until its text stays readable over a white or black desktop, tint
  included, and the settings list what was raised. The OS draws the blur: nothing redraws
  at rest. Scripts: `@glass-preset=frosted`, `@surface-opacity=sidebar:0.6`,
  `@glass-tint=ice:0.25`, `@glass-blur=strong`, `@glass-vibrancy=hud`.
* **Offline signed-in fixtures**: with `MUSIC_FIXTURES`, signing in with any cookie that
  has a `SAPISID` works offline (the fixture account, its library and 13 playlists), for
  screenshots and checks of the signed-in UI.
* **Sign in from your browser** now reads Firefox forks too: **Zen, LibreWolf, Floorp and
  Waterfox**. Like Firefox, they keep plain cookie files, so this is also a working
  browser-import route on **Windows**, where Chrome, Edge and Brave lock their cookies to
  themselves (app-bound encryption) and can't be read by any other app.
* **Page tabs.** Browse several pages side by side, each tab with its own back / forward
  history and scroll positions; the player and the queue stay shared. ⌘T / Ctrl+T opens a
  tab, ⌘W / Ctrl+W closes it, Ctrl+Tab / Ctrl+Shift+Tab and ⌘1…⌘9 / Ctrl+1…9 switch;
  ⌘-click / Ctrl+click a card or link (or *Open in new tab* in its menu) opens it in a
  background tab. The tab bar shows under the top bar once a second tab is open; drag to
  reorder, middle-click to close; up to 12 tabs. Settings → Appearance → *Page tabs*: *Show
  tabs* (on), *Restore tabs at start* (off). Only the shown tab keeps its pages in memory
  (plus the current page of the two most recently shown tabs), so many tabs cost little.
  The keys are editable (Settings → Keyboard shortcuts → *Page tabs*); with one tab ⌘1–⌘3
  are still Home / Explore / Library.
* **Sync between your devices** (Settings → Devices → Sync, off by default): settings, sound
  settings, likes and dislikes, your playlists, speed-dial pins, queue tabs and, if you want,
  the last 90 days of listen history stay the same on your paired computers. It goes straight
  between them over the device hand-off links (end-to-end encrypted, on the LAN without the
  relay), merges changes made while apart when the devices meet (the later change wins;
  likes and playlist songs from both sides are kept), has a switch per kind of data, "Sync
  now" and when each device last synced. Never synced: secrets, sign-ins, this machine's name,
  window positions, output device, folders, proxy, shortcuts, volume, local files. A versioned
  document of last-writer-wins registers with hybrid clocks (`music_core::sync`, `sync.sqlite`
  next to the library); no new dependency.
* **Friend groups and an activity feed** (Settings → Friends, off by default): lasting groups
  instead of one-off Listen together rooms. Create a group, invite with a code / link / QR
  code (SPAKE2, like device pairing), see what your friends are listening to on the Listen
  together page (opt-in sharing, a private session), and start a session for the whole group
  with one click. Each installation has a member key in the keychain with a recovery phrase to
  move it to a new computer; groups are signed records every member keeps (no server). Over
  the same iroh links and relay as device hand-off.
* **Reverb fine controls**: *Length* (0.1–10 s), *Pre-delay* (0–200 ms), *Brightness*,
  *Early reflections*, *Width* and *Dry level* under the rooms (Settings → Equalizer →
  Reverb). The four rooms are presets of them; moving one makes the reverb *Custom*.
* **Convolution rooms**: *Studio*, *Chamber* and *Church* (impulse responses generated by
  the app, nothing bundled), and **Load your own (WAV)…** for an impulse response file of
  yours (WAV / FLAC / AIFF, mono or stereo, up to 6 s used). FFT convolution in the audio
  engine; off costs nothing.
* **Night mode**: a compressor on the output (threshold −24 dB, ratio 3 : 1, attack 3 ms,
  release 250 ms, makeup +6 dB; all adjustable) that evens out loud and quiet parts for
  listening at a low volume. A switch in the Sound menu and in Settings → Equalizer.
* **Your own presets**: save, rename and delete named **sound presets** (speed, pitch,
  reverb with its fine values, equalizer) and named **EQ curves**; both are listed beside
  the built-in ones in Settings and the Sound menu.
* **Equalizer curve**: the real response of the ten bands drawn above the sliders, with
  points to drag; plain-language band names (*Sub*, *Bass*, *Punch*, *Warmth*, *Low mids*,
  *Mids*, *Upper mids*, *Presence*, *Brilliance*, *Air*); **Automatic pre-gain** can be
  turned off, and then a warning (and the curve in red above 0 dB) says when the boosts can
  clip.
* Scripts: `@reverb-length`, `@reverb-pre-delay`, `@reverb-brightness`, `@reverb-early`,
  `@reverb-dry`, `@reverb-width`, `@reverb-ir=PATH`, `@reverb=studio|chamber|church|custom|own-impulse`,
  `@night=on|off`, `@night-threshold|ratio|attack|release|makeup`, `@eq-save=NAME`,
  `@eq=NAME`, `@eq-pre-gain=on|off`, `@sound-save=NAME`, `@sound=NAME`, `@sound-status`.
* **Mini player**: shuffle, repeat, the heart and a volume slider under the controls;
  **Up next** (the back of the artwork card and the turntable, a list under the other
  layouts) and an **always-on-top pin** in its top strip. When its size changes it stays
  in the screen corner it is nearest to instead of keeping its top-left corner.
* **Now Playing gestures**: click the artwork to play / pause, scroll over it for the
  volume (the level shows for a second), the heart pops when you like a song; optional
  **Open Now Playing when playback starts** and **Click the empty space to close Now
  Playing**.
* **Podcasts**: 15-second back / forward buttons beside play and a speed button (0.5–2×)
  in the bar for episodes; ⇧⌘← / ⇧⌘→ (Ctrl+Shift+← / →) jump 15 seconds in any song. Songs
  autoplay added carry an **∞** in the queue.
* **Tall windows**: the Split design puts the cover, title and controls above the lyrics
  (automatically when the window is about as tall as wide or taller, or always / never).
* **Narrow windows**: under 1000 pt the player bar collapses (song, transport, queue, Now
  Playing, mute, the progress as a line along the top) with **More controls** to expand it.
* **Lyrics**: a **Follow the lyrics** button after scrolling away (the hold is now 8 s),
  ↑ / ↓ and Enter to pick a line and play from it, a still outline while lyrics load, and
  an optional **glow** in the artwork's colour on the line being sung.
* **Turntable ring**: with the Now Playing visualizer on (bars), the Turntable design draws
  the spectrum as a ring of bars around the record.

### Changed

* **Updates keep you signed in on macOS.** Releases are signed with the project's own
  certificate instead of ad hoc, so macOS sees each update as the same app and keeps
  the keychain access you allowed ("Always Allow"); before, every update was a new app
  to macOS and asked again, and a refused or missed prompt looked like being signed out.
  The first update with this asks one last time. Windows and Linux were never affected.
* If the keychain still refuses the sign-in, Settings → Account says so and offers
  **Try again** (and a notice at start) instead of looking signed out; the account is
  kept.
* The icon rail's expand button shows in the logo's place on hover (it had a row of its
  own, which moved every entry down at the end of a slide), and the rail's rows are as tall
  as the full sidebar's.
* Over a window material, the artwork backdrop fills the content area only (the sidebar,
  top bar and queue now show the desktop through their own opacity).
* A sync now stores the account's YouTube playlists in YouTube Music's order of recent
  activity (the sidebar's *Recent*).
* **Video thumbnails and local cover art take far less memory.** Video and playlist
  thumbnails (which YouTube only serves at fixed sizes, up to 1280 × 720) and your local
  files' cover art are now decoded at the size they are shown: a music video in a list row
  takes 64 KB instead of 3.5 MB, a 1400 px local cover 36 KB instead of 7.5 MB. A
  library page of local songs with large covers peaked at ~400 MB before, ~200 MB now.
* **Fewer wake-ups while idle.** With nothing playing, the app checks the player every 2 s
  instead of four times a second (also in the background). Media keys, the menu-bar item,
  a paired device's hand-off and anything you do in the window are still handled at once;
  while something plays, loads, downloads, a sleep timer runs or a Music Together session
  is on, nothing changes.
* **Scrolling fast over a slow connection** no longer downloads (and decodes) the artwork
  that already went off screen before its turn: the end of a long Home is filled sooner,
  with about a third of the downloads.
* **The volume slider follows a perceptual curve** (gain = position²): its middle is about
  half as loud (−12 dB) and the lower part gives fine control of quiet levels. Everywhere
  the volume is set (bar, mini player, scroll wheel, media keys, scripts). A volume saved
  by an older version keeps its loudness: settings files (version 1 → 2), settings exports
  and backups are converted on load. A Chromecast still gets the slider position.

## [0.3.2] — 2026-11-01

0.3.0 and 0.3.1 were tagged but never published. 0.3.0's macOS Intel DMG didn't build (the
disk image was made too small for the larger app; `scripts/bundle-macos.sh` now sizes it
itself), and 0.3.1 crashed on opening Settings → Account (fixed below). 0.3.2 is the first
published 0.3 release.

Three rounds of features ported or remade from other desktop YouTube Music clients
(refryt, Limusic, Zuno, Deviload) and tono.fm's ideas, all self-hosted: several accounts and
browser sign-in, sturdier playback with signed player fixes, queue parts and tabs, lyrics
that match the track's length, package-manager installs, reverb and mini player layouts,
theme files, a see-through window, music videos and theater mode, Spotify-style hand-off
between your devices, light mode and a memory pass.

### Added

* **Light mode.** Settings → Appearance → Theme → *Mode*: Dark (default), Light, or System,
  which follows the OS's appearance on macOS, Windows and Linux (the XDG portal's colour
  scheme) and switches live. gpui-component's light theme is the base; Customize theme works
  in both modes (darkness runs Soft → Bright → Pure white in light mode, accents are darkened
  to their WCAG level instead of lightened), and the views that drew white on dark on purpose
  follow: Now Playing and its designs, the lyrics, the mini player, the player bar's
  backgrounds, the app-wide look, artwork-coloured headers, the schematic previews in
  settings and the see-through window's readability floor (now also checked over a black
  desktop). Theater mode stays dark. Theme files get a light set (`[colors.light]`, or
  `mode = "light"`); a file with one set looks as before in both modes; the four starter
  themes gained their light versions (Snow Storm, Latte, Gruvbox Light, Alucard).
* **Quick theme switcher** in the top bar (a palette button, after Deviload): light / dark /
  system and a swatch of every preset and theme file.
* **One-run themes**: `MUSIC_THEME=nord:light` or `--theme nord` shows a preset, base theme,
  theme file and / or mode for that run without saving it.
* **Layout** group in Appearance: the sidebar as *Full* (drag its edge, 176–360 px), *Icons
  only* or *Expand on hover*; a drag-resizable queue panel (280–560 px); song list columns
  to show or hide (album, *Added*, *Plays*, duration); single top-bar buttons to hide; the
  macOS or Windows look for the window buttons on any system.
* **Card outlines** and a **line along the window's edge** (Motion & effects).
* **Smooth wheel scrolling** is a setting now (System / On / Off; System = on except macOS)
  and steps again with Animations Reduced or Off.
* **Fonts from files and a heading font**: `.ttf` / `.otf` / `.ttc` files in the settings
  folder's `fonts/` are registered at launch and listed in the font pickers; Customize theme
  → *Heading font* sets page titles, headers and Now Playing's title apart.
* **Matugen template** (`crates/music-gpui/themes/matugen/cratetune.toml`) that writes a
  wallpaper-coloured theme file with dark and light colours; the themes folder watcher
  applies every new run.
* **Translucent sidebar on Windows and Linux**: Mica on Windows 11, acrylic on Windows 10,
  the compositor's blur on Linux, with the same readable-opacity floor as on macOS.
* **KDE Plasma's animation speed** under Animations → System: transitions scale with
  `AnimationDurationFactor` (0.25–2×), "Instant" counts as reduced motion.

* **Music videos in Now Playing.** Settings → Appearance → Music videos → *Show music videos in
  the artwork* (off by default): when the playing song has a music video, Now Playing shows it,
  muted, in the artwork's place and in step with the music, which keeps coming from the app's
  own audio engine. A video track shows its own picture; a song shows the music video YouTube
  Music pairs it with (signed-in accounts), following the stretches the two share, so a video's
  intro or a scene without the song doesn't put it out of step. The header's video button
  switches back to the artwork for the rest of the session. *Video quality* caps what is
  fetched (360p, 480p by default, 720p). Seeks, pauses and buffering are followed; late frames
  are dropped, never shown late. Decoding uses VideoToolbox on macOS (hardware), Media
  Foundation on Windows and OpenH264 on Linux; nothing runs and nothing is downloaded while no
  video is on screen. (After Limusic.)
* **Theater mode** (Limusic's idea): the song filling the window, chrome free — the music video
  or the artwork on its blurred backdrop, the current lyric line over it, and controls that
  fade after three still seconds while the music plays. From Now Playing's header, the View
  menu or ⌘⇧T / Ctrl+Shift+T; Esc leaves.
* **Reverb.** Four rooms (Small room, Hall, Plate, Cathedral) and a wet mix, in Settings →
  Equalizer, after the equalizer on the whole output (so crossfades share one room). Off by
  default and then not run at all; when you pause, the echo rings out. An algorithmic
  Freeverb-style reverb in `music-core` (`reverb.rs`), about 0.3–0.5 % of one core.
* **Sound presets** (after Limusic): Original, Slowed + reverb, Nightcore, Practice and Concert
  hall set speed, pitch and reverb in one click.
* **Equalizer presets Warm and Night** (after Zuno and Deviload).
* **Sound menu**: equalizer presets, reverb rooms and sound presets as submenus of the player
  bar's quality menu and a new sliders button in the Now Playing header, highlighted while the
  sound is changed.
* **Mini player layouts**: Bar, Compact, Artwork card, Lyrics (the line being sung) and
  Turntable (the artwork as a record's label, a tonearm that follows the song), each with its
  own window size (Settings → Appearance → Mini player → Layout, `@mini-skin=`). Scrolling over
  the mini player changes the volume.
* **Turntable** Now Playing design (the ninth).
* **Ambient lights** visualizer for Now Playing (after Deviload): three lights in the
  artwork's colours behind the screen that follow the bass, mids and highs (Settings →
  Appearance → Player bar → Visualizer style, `@np-viz-style=`).
* Scripts: `@reverb=`, `@reverb-mix=`, `@sound=`, `@mini-skin=`, `@np-viz-style=`.
* **Theme files.** Themes as editable TOML files in the settings folder's `themes/`
  (Settings → Appearance → Customize theme → Theme files): a base theme, the Customize-theme
  options and any colour token; listed next to the presets, applied again whenever the file
  is saved, a file with a mistake listed with the line and the reason, low-contrast text
  flagged. **Open themes folder**, **Export current theme as file**, a commented
  `example.toml` and four starter themes (Nord, Catppuccin Mocha, Gruvbox Dark, Dracula).
* **See-through window** (Appearance → Window → Window background): Blurred (macOS window
  blur, Windows acrylic, KDE Plasma), See-through, and Mica / Mica Alt on Windows 11, with an
  opacity that is raised automatically so text stays readable on a light desktop. Off when
  the system asks for reduced transparency.
* **React to the music** (App-wide look): the gradient wash or glow follows the music's
  energy while it plays, at up to 30 frames a second; stops when paused, hidden or with
  reduced motion. Off by default.
* **Zoom**: ⌘= / ⌘+ / ⌘− / ⌘0 (Ctrl elsewhere), Ctrl / ⌘ + mouse wheel and View menu items
  step the text size from 75 to 150 %; editable in Keyboard shortcuts.
* **App icon** (Appearance → Window): Midnight (standard), Paper, Sunset, Mint, Mono or your
  own image — the Dock icon on macOS, the window and taskbar icon on Windows, X11 from the
  next start.
* **Continue on another device** (device hand-off, Spotify-style, self-hosted). Pair two
  computers once in Settings → **Devices** (a short code on the same network, or a pairing link
  / QR code across networks; SPAKE2, so the code can't be guessed offline). A **Devices** button
  on the player bar then lists them with what each plays ("MacBook: Song · 1:23"), with
  **Continue here** (take over its queue at that position; it pauses) and **Play on …**. At launch,
  Home's continue card offers "Continue 'Song' from MacBook at 1:23?" when a paired device
  played more recently. Peer to peer over iroh 1.4 QUIC (end-to-end encrypted, only paired
  device keys accepted), mDNS on the LAN, and across networks our own `iroh-relay`
  (`IROH_RELAY_URL` in `packaging/app.env`, `https://relay.smhcoders.click` by path next to
  music-relay; a setting overrides or turns it off); iroh's public relays and DNS lookup are never
  used. Deployment files in `deploy/iroh-relay/`. The device key is kept in the keychain.
* **Keep playing here when I leave** (Settings → Music Together, on by default): a guest that
  leaves a session, or whose host ends it, carries on with the room's queue, position and repeat
  mode instead of stopping (works with ArchiveTune hosts: nothing changes on the wire).

* **Package managers.** Manifests for a Homebrew cask (`packaging/homebrew/`), a Scoop bucket
  (`packaging/scoop/`), the AUR (`cratetune-bin` from the release tarball and `cratetune` from
  source, `packaging/aur/`) and Flatpak (`packaging/flatpak/`, app id
  `io.github.moiz_smhcoders.cratetune`), generated by `scripts/update-packages.sh` from a
  release's `SHA256SUMS.txt`. The release workflow validates them (AppStream, desktop file,
  `makepkg --printsrcinfo`, a Flatpak build with Flathub's linter) and attaches them to the
  release as `cratetune-X.Y.Z-package-manifests.tar.gz`; publishing to the tap, bucket, AUR and
  Flathub is a manual step (docs/RELEASING.md, "Package managers").
* **"Updates come from …".** A copy installed by Flatpak, Homebrew, Scoop, the AUR or Snap
  doesn't check for or install updates itself; Settings → About says which package manager
  updates it, and "Check for Updates…" is hidden. Detected from `CRATETUNE_PACKAGED=<name>`, the
  Flatpak / Snap sandbox, a `packaged` marker file the package installs, or the install path
  (Homebrew's Caskroom, a Scoop `apps` folder).
* **PRIVACY.md**: every service the app contacts, what each receives and how to turn it off,
  what is stored locally and in the keychain, and no telemetry. Linked from Settings → About
  (*Privacy policy*) and the README.
* **docs/TRANSLATING.md**: the `.lang` format, adding a language, and Weblate settings
  (the catalogs are Java Properties compatible).
* The Linux tarball's AppStream metadata is now complete (description, screenshots, links,
  content rating, release date), from `packaging/linux/app.metainfo.xml.in`, shared with the
  Flatpak.
* **Several YouTube Music accounts, switched in one click.** Add accounts from Settings →
  Account → *Your accounts* or the top bar's account menu (**Add account…**); switch from
  either. Each account keeps its own sign-in in the keychain, its name, avatar, Google account
  index / channel, and (when signed in through the window) its own sign-in profile, so
  background renewals never mix accounts. A switch swaps the session at once, reloads Home and
  the pages, and syncs that account's library. Liked songs, saved albums, subscriptions and
  YouTube playlists follow the account in use; local files, local playlists, play history,
  downloads and settings are shared. Likes still owed to YouTube stay with their account.
  Signing out of one switches to another. (After Zuno and Limusic.)
* **Sign in from your browser.** Takes the YouTube sign-in of Chrome, Edge, Brave, Firefox,
  Safari (macOS), Arc, Vivaldi, Opera or Chromium on this computer: pick a profile, CrateTune
  reads only its youtube.com cookies and checks the account. macOS asks for the browser's
  "Safe Storage" keychain key; Safari needs Full Disk Access; on Windows, browsers with
  app-bound encryption (recent Chrome, Edge, Brave) are shown as locked, with the reason.
  (After refryt.)
* **Add plays to my YouTube history** (Settings → Account, on by default): signed in, the
  songs you play are reported to your YouTube Music history like ArchiveTune does
  (`videostatsPlaybackUrl` and watch-time pings), so they show up on your phone.
* **Home → "Jump back in"**: a small card for the newest song in your YouTube Music history
  when it was played on another device, with **Resume** (song + radio; episodes and long
  videos at YouTube's saved position) and dismiss.
* **visionOS stream client, first in line.** YouTube's visionOS `player` request returns
  direct audio URLs: one request (~0.4 s), no JavaScript challenges, no PoToken and no pre-roll
  ad wait. Its visitor id is earned once and kept in `<config>/cache/youtube/`. Automatic now
  tries **visionOS → TV → TV Simply + PoToken → Web + PoToken → ~1 min preview**. TV Simply
  sends a visitor-bound PoToken in the request and the video-bound one on the URL. Settings →
  Playback → YouTube streams also offers *visionOS client* on its own.
* **Circuit breaker per client** (after refryt). A route that fails 3 times in a row is skipped
  for 10 minutes, then tried once more. Failures that belong to one video (unavailable, private,
  age- or Premium-gated, blocked in the country) and network outages don't count, and the last
  enabled client is never skipped. Settings → Playback → *Stream route* shows which client
  played the last song, which ones are skipped and for how long, and which player fixes apply
  (`WebResolver::status` / `set_status_hook`). `MUSIC_TRACE=1` prints the winning client and
  its time for every song.
* **Signed player fixes** (`packaging/player-fixes.json` + `.minisig`). Running apps pick up
  new client versions and user agents, a client order, clients to skip, known
  `signatureTimestamp`s, and broken or pinned `player.js` versions from the app's own
  repository, with the relay as an optional mirror (`music-relay --player-fixes DIR`). No app
  update is needed. The file is data only and signed with Ed25519 in minisign format; the public
  key is compiled in and the private key never enters the repository. Unsigned or altered files
  are ignored, and a lower `serial` never replaces a higher one. It is checked every 6 h, and
  sooner when every route fails. Settings → Playback → *Player fixes* turns it off.
  `scripts/sign-player-fixes.sh` signs; `docs/RELEASING.md` explains it.
* **The next track's URL is resolved ahead.** When the queue sets the next song, its stream URL
  is resolved in the background (`StreamResolver::prefetch_track`). The preload near the end of
  the current song, or a skip, then starts without a resolve or ad wait. A prefetched URL is
  used at most 1 h after it was resolved and never within 10 min of its `expire` time; older
  ones are resolved again. Each one is handed out once, and a refresh after a rejected URL never
  reuses one.
* **Your queue and the rest, apart** (after Zuno). The queue panel and Now Playing's *With
  queue* design show **Next in queue** (what you added with *Play next* / *Add to queue*)
  above **Next from <album / playlist / page>** or **Next from <song> radio** (the list
  you started, a radio, autoplay), as Spotify does. Your songs always play first: *Add to
  queue* goes to the end of your part, not behind a 500-song playlist, starting another
  list keeps them, and jumping ahead in the list doesn't skip them. Each part has its own
  **Clear**; shuffle only shuffles the list part. The saved queue remembers both parts
  (queues saved by older versions load as before; older versions still read new ones).
* **Queue tabs** (after Zuno). A row of tabs in the queue panel: **+** starts a new queue
  ("Queue 2") while the one you were playing is put away where it was; click a tab to see
  its songs and where it was left, **Continue this queue** plays on from there. Right
  click renames or removes a tab. One tab plays at a time; tabs and their queues, positions,
  shuffle orders and repeat modes are kept across launches.
* **Multi-select on every song list** (after Zuno): ⌘-click (Ctrl-click) adds or removes
  a row, Shift-click selects a range, ⌘A / Ctrl+A selects the whole list under the
  pointer (outside text fields; in Settings → Keyboard shortcuts), Esc clears. A bar
  floats above the player bar with Play, Play next, Add to queue, Add to playlist, Like,
  Dislike, Download, the list's own Remove (library, liked songs, downloads, your
  playlists), Select all / none and ✕. Plain clicks still play from the row. Works in the
  library, playlists, albums, artist pages, search results, history, downloads and stats;
  the library lists' **Select** button still shows checkboxes.
* **Music Together: "Suggestions need my approval"** (after Limusic). With it on (room
  settings on the Listen together page, or Settings → Music Together), a guest's *Play
  next* / *Add to queue* waits in a **Suggestions waiting** card for **Approve** /
  **Reject**; approved songs go into the queue the way the guest asked. Guests see
  **Your suggestions** with *Waiting for the host*, then *Added* or *Turned down*, plus a
  message. Compatible with ArchiveTune and older versions: the setting is only sent when
  on, and the one new message (`suggestion_status`) is skipped by peers that don't know
  it.
* **Logs** in Settings → About: the app's recent log lines, live, newest first, filtered
  by Errors / Warnings / Info / Everything (trace lines with `MUSIC_TRACE=1`), with
  **Copy**, **Save logs…** and **Open log folder**. A log file (on by default, *Write a log
  file*) is kept in `logs/` of the cache folder (or of `MUSIC_CONFIG_DIR`), 1 MB plus one
  older file. Cookies, tokens, API keys, signatures, PoTokens, session keys and
  passwords are removed from every line before it is shown, kept, written or printed.
* **Output devices without the noise** (after Zuno): on Linux the picker drops ALSA's
  virtual PCMs (null, dmix, dsnoop, surround…, iec958, rate converters, …) and shows one
  entry per card (sysdefault / hdmi before raw `hw:` / `plughw:` duplicates), with
  friendly names (PipeWire, PulseAudio). On every system: the default first and marked,
  no repeated ids, identical names numbered. **Show all devices** (Settings → Playback)
  lists everything; a device you picked is never hidden.
* **Lyrics: sources in waves.** The fastest, most reliable lyrics sources (BetterLyrics,
  BetterLyrics Portato, LRCLIB, SimpMusic) are asked first, in parallel; Binimum, KuGou, Unison,
  YouTube Music and Paxsenix only when those have no good synced lyrics (or take longer than
  2.5 s), YouLyPlus and Megalobiz last. Once good lyrics are in, sources still working are
  cancelled. On 12 popular songs the search took a median 135–140 ms instead of 160–390 ms and
  asked 4 sources instead of 6 at once. Settings → Lyrics → *Ask sources in waves* (on).
* **Lyrics: match the track's exact length.** Lyrics made for a recording within 2 s of the
  playing song win over other cuts (radio edit, extended mix, live version) before word
  timing and provider order are compared; the matched title and artist must resemble the
  song's. LRCLIB, KuGou and Binimum report the length of what they matched, Apple-style TTML
  and LRC `[length:]` tags declare it, and video-id lookups (SimpMusic, Unison, YouTube Music)
  are exact by definition. LRC `[offset:]` tags are applied. A track without a length (some
  search cards) first gets it from YouTube Music's watch page, as Limusic does. Settings → Lyrics → *Match the
  track's exact length* (on).
* **"Lyrics from LrcLib · Line-synced"** above the lyrics, and behind it **Sources tried**:
  every source in the order it was asked, with its result (sync kind and same / different
  length, not found, error, timed out, slower · stopped, not asked) and time. Pick another
  source's lyrics for the song — remembered per song, and that source is asked first next
  time — or go back to *Automatic*.
* **Timing nudge per song**: − / + above synced lyrics move them 0.1 s earlier or later (up to
  ±10 s), remembered for that song; the value resets it. Clicking a line seeks with the nudge.
* Script actions `@lyrics-source=NAME|auto`, `@lyrics-nudge=MS|reset`, `@lyrics-sources`;
  `lyrics_bench` example (search times on real songs).
* music-core: `LyricsService::search` / `SearchOptions` / `LyricsSearch` (attempts, alternates),
  `fetch_from`, `with_waved_providers`; `LyricsResult::matched` (`MatchInfo`),
  `duration_fit` (`DurationFit`), `name_match`; `LyricsServiceConfig::{waves, wave_timeout,
  match_duration}`.

### Changed

* **Translucent sidebar (macOS) shows the desktop blur.** It drew an opaque background
  before (gpui-component's root painted one under the app); its opacity is now raised until
  the sidebar's text stays readable on a light desktop.
* **Home is back in the sidebar** as well, next to the top bar's Home button.
* A background sign-in renewal is used only if it is still the same login; a sign-in
  profile holding another account is never used to switch accounts silently.
* **Self-heal on a failed solve** (after Limusic). When the `n` / signature challenge can't be
  solved (a solver error, a missing or unchanged `n`, the solver's `enhanced_except` marker),
  the app drops the cached preprocessed player and its `sts`, fetches the watch page and
  `player.js` again, and solves once more before moving to the next client. A newer
  player-fixes manifest is fetched at the same time.
* Onboarding's privacy page and the stream settings describe the new routes and the player-fixes
  download.
* Starting a list, an album or a radio keeps the songs you queued yourself (they play
  next); Music Together guests still follow the host's queue exactly.
* Esc first clears a song selection before closing panels or going back.
* Lyrics `quality_grace` 1.5 s → 0.8 s and `priority_grace` 2 s → 0.3 s (waves make the long
  wait unnecessary). Binimum ranks the candidate of the same length above a word-timed one of
  another length.
* **Less memory on long listening sessions and long scrolls.** With Now Playing open, every
  song used to leave ~3 MB of graphics memory behind (about +100 MB after 30 songs); now the
  app stays flat. Scrolling quickly through a long Home peaks at ~370 MB instead of ~600 MB on
  a Mac, and drops back sooner. On displays without high-DPI scaling (most Windows and Linux
  setups) artwork is downloaded and kept at the size it is shown, a quarter of the memory it
  took. On Linux, memory freed by the artwork cache is handed back to the system.

### Fixed

* Opening **Settings → Account** (also through "Sign in") no longer crashes: the page read
  the settings while drawing them (`account_choices`). A new CI step,
  `scripts/smoke-pages.sh`, opens every Settings section and the main pages on all three
  systems and fails on a panic.

## [0.2.6] — 2026-10-31


### Added

* **Listen together over the internet, with room codes.** Hosting now goes through a relay
  by default (*Over the internet (relay)*; *Same network only (direct)* is one click away, and in
  Settings → Music Together → *Host sessions*), so guests on any network can join without setup.
  The host sees a short room code (`ABCD-EFGH`) in big letters with **Copy code**, next to the
  link; guests type it under **Join with code** (case, spaces and dashes don't matter). Links
  still work and now also carry the code. If the relay doesn't answer within a few seconds, the
  session starts on the local network with "Relay unreachable, guests must be on the same
  network". The page and Settings say that sessions through the relay pass song titles and
  display names via the relay server.
* **Default relay** `wss://relay.smhcoders.click`. Which relay: your *Relay server* setting,
  else `packaging/relay.txt` in the app's GitHub repository (fetched when needed, cached 6 h, so
  the relay can move without an app update), else the built-in `RELAY_URL` in
  `packaging/app.env`.
* **music-relay: room codes.** `POST /v1/together/sessions` gives a live room's host a code
  (8 characters, no 0 / O / 1 / I; unique among live rooms, freed when the room ends);
  `POST /v1/together/sessions/resolve` gives a guest the session id, guest key and WebSocket
  path for it, never the host key. Look-ups are rate-limited per client address
  (`--resolve-per-minute`, default 10; `--trust-proxy` for a reverse proxy's
  `X-Forwarded-For`). The WebSocket protocol is unchanged.
* Script actions `@together-join-code=CODE`; `@together-host-relay=FILE` also writes the code
  to `FILE.code`; `@together-status` prints the code.

### Changed

* Listen together: "Host via relay" is gone; **Start session** hosts the way chosen above it
  (relay by default). An empty *Relay server* setting now means the default relay, not "LAN only".

## [0.2.5] — 2026-10-31


### Fixed

* **Now Playing opens right after a fresh start.** With the queue restored from last time and
  nothing played yet, opening Now Playing (⌘L, the artwork, the lyrics button) did nothing until
  play was pressed once. It now loads the restored song paused where it was, then opens.
* **Signed-in session going stale** (likes failing with "HTTP status 401" until you signed
  out and in again). CrateTune stored the sign-in cookie once and never updated it, while
  Google rotates parts of it (`__Secure-1PSIDTS`, `SIDCC`, …) through `Set-Cookie`.
  Signed-in responses now update the cookie like a browser (only from YouTube / Google over
  HTTPS, only youtube.com cookies); the browse client, account features and playback share
  the updated cookie, and it is saved to the keychain at most once a minute and on quit.
  Requests are signed like YouTube's web client (`SAPISIDHASH`, `SAPISID1PHASH`,
  `SAPISID3PHASH`).
* A rejected sign-in (401) is renewed in the background first: the sign-in window, never
  shown, loads YouTube Music from its profile (at most every 10 minutes) and the request is
  retried once. If that fails, one notification says "Your YouTube Music sign-in expired"
  with **Sign in again** (also in Settings → Account), instead of repeated "retrying" toasts.
  Queued likes wait for a working sign-in instead of using up their 20 attempts, and are
  sent after signing in again; syncs stop at the first rejected request.
* Sign-in problems and network problems are now told apart in toasts and sync errors
  ("sign-in expired" vs "couldn't reach YouTube Music").

### Changed

* The "Sign in with Google" window (macOS, Windows) keeps a persistent browser profile
  (Windows: `%LOCALAPPDATA%\cratetune\data\SignIn` instead of InPrivate; macOS: its own
  WebKit data store instead of a non-persistent one), so the session can be renewed in the
  background. **Sign out** deletes it.

## [0.2.4] — 2026-10-30

### Added

* `scripts/perf-macos.sh`, the macOS counterpart of `perf-windows.ps1` (CPU from `ps`,
  memory from `footprint`, the display's refresh rate from `system_profiler`), plus a 6 ms
  budget for a full draw of Home (a 180 Hz frame is 5.5 ms).
* CI: a frame-time check on macOS and Windows ("perf: frame-time",
  `scripts/perf-frames.sh`). It draws Home offline from recorded responses at the top and
  at the end of a long feed and fails if a frame costs more than 1.5x further down (the
  Windows stutter fixed in 0.2.2) or exceeds a ceiling for the runners.
* `MUSIC_FIXTURES=DIR`: offline fixture mode (InnerTube from recorded JSON files,
  generated artwork, no network) for perf checks and screenshots; `MUSIC_NO_CULL=1` turns
  section culling off for comparisons.

### Changed

* A shelf skipped because it is far outside the window is now a single empty element
  (it was two), so a long feed adds next to nothing to a frame: on GitHub's Windows
  runner the end of a 60-shelf offline feed draws about as fast as the top of Home.
* Changing the **content country** (Settings → Content & region) now also starts a new
  anonymous YouTube visitor, so the signed-out Home feed follows the new region instead of
  the old visitor's history (the visitor is otherwise kept across launches).

## [0.2.3] — 2026-10-30

### Added

* **Sideways rows scroll every way**, like YouTube Music and Spotify: **‹ ›** at the right
  of every shelf's title (one window-width per click, stopping on a card's edge; gliding
  unless Motion is Off; dimmed at the ends, hidden when everything fits), **Shift + wheel**
  and horizontal wheels / trackpad swipes over a row (smooth on Windows and Linux), **click
  and drag** (after 6 px; the card under the pointer then doesn't open), and a thin
  scrollbar under a hovered row. Rows without a title (Home's category chips when they
  don't fit) show round ‹ › on their ends while hovered. Applies to Home (quick picks too,
  as cards or as the list grid), artist, album, Explore (moods and shelves), Charts, mood /
  genre pages and Stats.
* **"From YouTube Music · <country>"** on Home above YouTube Music's own feed, with
  **Change region** (opens Settings → Content & region on the country) and **Hide** (hides
  the feed section); YouTube's own quick picks and the community shelf say so too. On a
  first run whose content country came from the system's region, Home says which country
  it shows, with **Change**, until dismissed.
* Script hooks: `@shelf-status`, `@shelf-scroll=N:next|prev|start|end`,
  `@drag=X1;Y1;X2;Y2`; `@scroll` / `@wheel` take an optional DX and `shift`.

### Fixed

* **Listen Together: a guest could miss being banned or removed, or the end of the session**,
  and see "Connection lost" instead. When a message from the guest (a heartbeat, every few
  hundred ms) arrived just as the host acted, the server dropped the connection without
  sending what it had queued. In a stress test 1 in 40 goodbyes was lost, 1 in 8 under CPU
  load; now none are.
* **Windows: Listen Together and relay connections could drop, lose data or crash the
  app** while idle. Polling a socket with a read timeout sometimes fails with error 997
  and loses bytes; a new `music-net` crate waits with `WSAPoll` instead.
* **Shelves couldn't be scrolled sideways** at all (no wheel, swipe, drag or button):
  each row wrapped its cards in one flex box that layout stretched to the row's width, so
  GPUI measured the content as no wider than the view and the largest offset stayed 0.
  The cards are now the scroll container's own children. GPUI's own wheel handling would
  then have turned the vertical wheel over a row into sideways scrolling (and still
  scrolled the page); rows now read the wheel themselves and leave vertical scrolling to
  the page.
* Home's feed cache key includes the content country and language.

## [0.2.2] — 2026-10-30

### Added

* `scripts/perf-windows.ps1`: CPU, memory and frame-rate check of a release build on
  Windows (idle, scrolling Home, playing, Now Playing), with pass / fail checks; see
  docs/TESTING.md, "Performance".
* `MUSIC_TIMINGS=1` also prints the player bar's frame rate, and frame intervals
  (p50 / p95 / max) for the window and the bar.

### Fixed

* **Stutter on Windows** while scrolling and changing pages: every shelf on Home (and on
  artist, Explore, Charts and mood pages) was built, laid out and painted on every frame,
  visible or not, and the endless feed made each frame heavier the further it was
  scrolled. Shelves more than half a screen outside the window are now skipped (an empty
  box of the same height). On a Ryzen 7 6800H with a 180 Hz display: a full frame of Home
  10.6 → 6.5 ms, scrolling ~100 → 165–180 fps in a normal window, 60–73 → 90–170 fps
  maximized. Shelves keep their sideways scroll position while skipped.

## [0.2.1] — 2026-10-30


### Added

* **Custom top bar** (Spotify-style) in place of the system title bar and the old header,
  the same on macOS, Windows and Linux: **⋯** (Settings, Keyboard shortcuts, Check for
  Updates, About, Quit — on macOS too, so the bar looks the same everywhere), **← →**,
  **Home** and the **search field** centred in the window, and the **account** on the right
  (signed in: your name, Account settings, Sync now, Sign out; signed out: Sign in, which
  opens Settings → Account). macOS keeps its native traffic lights, moved into the bar and
  centred in it; in native full screen they hide and the bar moves left. Windows gets
  46 × 32 caption buttons with a red close hover, wired to Windows' own hit-testing
  (`WindowControlArea`), so drag, double-click to maximise, Aero Snap, the system menu and
  the Windows 11 snap layouts on the maximise button behave natively. Linux asks for
  client-side decorations and draws round buttons (only those the compositor offers),
  drags with the compositor, opens the window menu on right-click and adds resize edges;
  under server-side decorations the window manager's title bar stays. The bar uses the
  sidebar's colour (translucent like the sidebar over the artwork backdrop or the desktop
  blur) and density, stays above Now Playing (except in full screen) so the window
  controls are always reachable, and is a cached view that only re-renders when what it
  shows changes. Home moved from the sidebar to the bar. Script hooks: `@titlebar-status`,
  `@app-menu`, `@account-menu`, `@titlebar-account=NAME|off`, `@system-title-bar[=off]`;
  `MUSIC_TITLEBAR=system|custom` for one session, `MUSIC_TITLEBAR_STYLE=windows|linux|mac`
  (debug / screenshot builds) draws another platform's controls.
* **Settings → Appearance → Window → Use the system title bar** (off by default): the OS
  title bar and the old header (back, forward, search; ⋯ on Windows / Linux) as before.
  It applies the next time the app starts; the row says so and offers **Restart now**. In
  the settings search; not part of profiles (`appearance.system_title_bar`).

* **Home like ArchiveTune's**: personal sections from your listening above the YouTube
  Music feed, in ArchiveTune's order — category chips, **Quick picks** (up to four of your
  most played songs, one per artist, each seeding a radio; songs related to several rank
  first, songs already in the library are left out, and a refresh draws new ones), the
  community playlists shelf, **Speed dial** (pin songs, albums, artists and playlists from
  their context menu; Shuffle plays the pinned songs), **Keep listening** (songs, albums and
  artists of the last two weeks), **Your YouTube playlists** (signed in), **Forgotten
  favourites** (at least 3 plays before the last 30 days, under 20 % of that listening
  since) and **Similar to …** rows for two artists and a song you play a lot. New users get
  YouTube's quick picks and feed for their content language and country. The library
  sections show at once; the recommendations follow (about 8 requests per refresh, at most
  3 at a time), cached for 20 minutes per account, with a **Refresh** button. Content
  filters (explicit, videos, AI filter) apply everywhere, and disliked songs are left out
  of every section.
* **Settings → Content & region → Home**: quick picks from your favourites / your last song
  / off, cards or list layout, category chips, a tonal backdrop (skipped while the app-wide
  artwork look is on) and which sections show. In the settings search; `home` in
  `settings.json`; script hook `@home=OPTION:VALUE` (and `@home=pin:N`).

* **Full screen for Now Playing and lyrics**: a Maximize / Minimize button in the Now
  Playing header (every design and the lyrics view; its tooltip shows the shortcut) puts
  the window in full screen and back, on macOS as native full screen in its own Space.
  New **Toggle full screen** action, ⌃⌘F on macOS and F11 on Windows / Linux, editable
  in Settings → Keyboard shortcuts and in the View menu; it works anywhere. Settings →
  Appearance → Now Playing → **Full screen when Now Playing opens** (off by default, off
  in the Minimal / Fast profile, part of profiles and the settings export) enters full
  screen whenever Now Playing or the lyrics view opens. Closing Now Playing (Esc, the
  close button, ⌘L, navigating away) leaves full screen again, but only if CrateTune
  entered it for Now Playing: a window that was already in full screen stays there, and
  leaving full screen yourself (green button, the system) is never undone. Esc leaves
  the full screen entered with the button first, then closes Now Playing. The mini player
  keeps working over the full-screen window. Script hooks: `@np-fullscreen[=on|off]`,
  `@np-fullscreen-auto=on|off`, `@lyrics-view`, `@fullscreen-external`,
  `@fullscreen-status`.

### Changed

* The saved window frame is no longer replaced while the window is in full screen, so the
  next launch opens the normal window, not one the size of the screen.
* **Likes stay in sync with YouTube Music** (signed in): the liked songs sync at start
  (unless the last sync is under 10 min old), every 45 min, when the window comes back
  after 30 min and when the Liked tab opens; the whole library every 6 h and on sign-in /
  account switch / **Sync now**. Syncs never overlap. Settings → Account → **Sync with
  YouTube Music automatically** (on by default, like ArchiveTune) turns the background
  syncs off. The Liked tab shows the last sync time and a **Sync now** button.
* **A song's like from YouTube when it plays**: a few seconds in, its like or dislike is
  read from YouTube Music's own like button (reusing the radio's response when there is
  one), so a song liked on the phone shows the filled heart right away.
* **Dislike** (thumbs down): song menus, Now Playing (Split design) and an optional
  player-bar item (off by default). Rates the song on YouTube Music, removes a like,
  skips the playing song (**Skip songs you dislike**, on by default) and keeps disliked
  songs out of radio and autoplay. **Remove dislike** takes it back.
* **Offline likes**: a like or dislike YouTube Music didn't take is kept and retried
  every 5 min and before each sync, instead of staying local only.
* Library database schema v2 (adds `song.disliked_at` and `pending_rating`; older builds
  refuse a v2 database, as for every schema bump).
* Automatic and liked-only syncs no longer unlike everything when YouTube's liked list
  comes back empty or truncated; ratings still queued and local files keep their state.
* **Windows: "Sign in with Google…"** opens a sign-in window like on macOS (WebView2,
  InPrivate, its own data folder under `%LOCALAPPDATA%\cratetune\data\SignIn`); it
  closes by itself once YouTube Music opens. Without the WebView2 runtime the app says so
  and opens the paste-cookie dialog.
* **Windows: notification-area icon** with the same menu as the macOS menu-bar item
  (now playing, play / pause, next, previous, mini player, show window, quit); a left
  click shows the window.
* **Smooth mouse-wheel scrolling** on Windows and Linux: each notch glides ~108 px over
  ~140 ms instead of jumping, quick notches add up; trackpads, Ctrl + wheel and the volume
  / seek bars are unchanged; follows Motion (Reduced: shorter, Off: instant).
  `MUSIC_SMOOTH_WHEEL=0` turns it off. macOS is unchanged.
* **Windows / Linux: a "⋯" menu** at the right of the header with Settings, Keyboard
  shortcuts, Check for Updates, About and Quit (what the macOS menu bar holds).
* **Windows: one app at a time.** Starting CrateTune again brings the running window to the
  front (per settings folder, so a test copy with `MUSIC_CONFIG_DIR` still starts).
* `MUSIC_CONSOLE=1` (Windows): show the log in the terminal that started the app, or in a
  new console window.

### Fixed

* **Windows: no console window** behind the app any more (closing it closed the app).
  Release builds are GUI programs now.
* Windows: the mini player stays above other windows and has a close button and a drag
  strip (it had no title bar, so it couldn't be moved or closed from its window).
* Windows: dates in the library, history and stats used UTC instead of the local time
  zone.
* Windows: "Animation effects" off in the Windows settings now counts as reduced motion for
  Motion → System.
* The paste-cookie steps name the developer-tools key of the platform (F12 on Windows /
  Linux), and the YouTube streams texts mention the Windows PoToken route.
* **Home showed a generic feed**: it was fetched before the keychain returned the YouTube
  cookie, so a signed-in user got the anonymous feed (and kept it, cached), and the
  anonymous visitor YouTube assigns was forgotten at every launch. Home now reloads for the
  signed-in account (and on account or channel switches), and the visitor is kept across
  launches.

### Removed

* "Played this session" on Home (Keep listening replaces it).

## [0.2.0] — 2026-10-29

### Added

* **Settings search**: a search field at the top of the settings sidebar (⌘F / Ctrl+F
  while Settings is open; again for the music search). It suggests popular and recently
  opened settings, and while you type lists matching settings with their section and
  group and a description snippet, matches highlighted. Titles and descriptions are
  searched in the interface language and in English, with English synonyms ("dark",
  "colour", "bitrate", "EQ", "shortcut", …), word starts and one or two typos. ↑ / ↓ /
  Enter or a click opens the section, scrolls the row into view, highlights it for 1.5 s
  (instant with reduced motion) and focuses its control; Esc clears, then closes. Covers
  every row of every section and every keyboard shortcut. Script hooks:
  `@settings-search=QUERY`, `@settings-search-pick=N`, `@settings-anchors`.

* **App-wide look** (Settings → Appearance → App-wide look; off by default): the whole
  window can take its colours from the song's artwork, like the player bar's background.
  **Tint** tints the sidebar, panels, menus and cards with the artwork's hue; **Gradient
  wash** puts a large soft gradient in the artwork's colours behind the content (with a
  direction); **Glow** adds a soft artwork-coloured light along the top of the window.
  Options: intensity, dim when paused, tint menus and panels too, **Follow the player
  bar's style**, and an **Animate** switch (gradient only, off by default: a slow drift
  at about 13 fps that stops when paused). It is static otherwise: one 400 ms cross-fade
  per song, and text keeps at least 4.5:1 contrast.
* **Motion & effects** (Settings → Appearance):
  * **Animations: System / Full / Reduced / Off.** System follows the macOS "Reduce
    motion" setting (and GNOME's animations switch on Linux). Reduced turns slides into
    fades, caps transitions at 150 ms and stops ambient motion (gradient drift, Wavy,
    Pulse and Now Playing glows, the artwork spring). Off makes every change instant and
    rests the visualizers and word-by-word lyrics.
  * **Page transitions** (on): a 180 ms fade (and a small slide with full motion) when a
    new page opens.
  * **Hover effects on tiles** (on): album, playlist and song tiles light up and show a
    round play button that plays without opening the page.
  * **Artwork-coloured page headers** (on): album, playlist and artist headers take the
    cover's colour.
  * **Artwork corners** (match theme, square, rounded, round), **artwork shadows** (off)
    and **tile size** (small, regular, large).
  * **Translucent sidebar** (macOS, off): the desktop shows through the sidebar, blurred.
* **Player bar artwork: Standard or Edge to edge** (Appearance → Player bar). Edge to edge
  fills the bar's height with the cover, flush with the left edge. The mini player keeps
  its own layout.
* All of these are in profiles, theme files and settings export. **Minimal / Fast** turns
  them off (animations off, no app-wide look, no transitions, hover, header colours,
  shadows or blur).
* Script hooks: `@app-look`, `@app-look-opt`, `@motion`, `@effects`, `@bar-art`,
  `@look-status`.
* **The app updates itself** on macOS, Windows and Linux (Settings → About → Updates, or
  **Check for Updates…** in the app menu on macOS / the Help menu elsewhere):
  * The flow is **Update to X.Y.Z**, then a progress bar with Cancel, then **Restart to
    finish**.
  * Every package is checked against the release's `SHA256SUMS.txt` and is refused if
    the file is missing or doesn't match.
  * The swap is atomic, with rollback, so a failed update leaves the old version
    working.
  * Copies that can't replace themselves (built from source, a read-only folder,
    package managers) get a **Download manually** link.
  * The macOS bundle is replaced in place and restarted; the Windows installer runs
    silently and restarts the app; the portable zip, AppImage and tarball are replaced
    by the app itself. See the user guide, "Updates".
* New switches: **Check for updates automatically** (on: at launch and every 24 hours),
  **Install updates automatically** (off: downloads in the background, asks to restart,
  installs on quit) and **Include pre-releases** (on by default for 0.x and pre-release
  versions). They are part of settings export and import, not of profiles.
* The "Check for Updates…" action can be given a keyboard shortcut (none by default).
* Releases now include `cratetune-<ver>-macos-<arch>.app.zip` (the macOS update package)
  and `SHA256SUMS.txt`; `-beta` / `-rc` tags are drafted as pre-releases. How to cut a release:
  `docs/RELEASING.md`.

### Changed

* **Renamed to CrateTune** (app id `cratetune`, bundle id `io.github.moiz-smhcoders.cratetune`,
  repository `Moiz-SMHcoders/CrateTune`), with a new icon: a white katakana ク ("ku", as in
  クレート, crate) on a navy tile. Nothing is lost: on the first start the old `tunedesk`
  settings, library and cache folders are moved to the new name, keychain entries are taken
  over the first time they are read, and settings / theme files exported by TuneDesk still
  import. The macOS app is now `CrateTune.app` (delete the old `TuneDesk.app` after
  installing).
* The update check reads the release list instead of `releases/latest`, which skips
  pre-releases.
* Updating from **0.1.0** still needs one manual download; later versions update
  themselves.

### Fixed

* **Settings → Account → Google account** no longer offers bare numbers 0–3 that failed
  for accounts that aren't there. The row is for logins that hold several Google
  accounts (like the account switcher on youtube.com): CrateTune now finds which ones the
  cookie's login actually holds (at sign-in, at startup and on every sync, in the
  background) and shows them by name, e-mail and picture. With a single Google account
  the row is replaced by a note on how to add more; brand channels keep their own picker.
* Switching Google account or channel is safe: nothing is saved until the new account
  answers. If it fails, CrateTune stays signed in to the previous account and says why
  ("That Google account isn't signed in in this session"), instead of keeping a broken
  setting. A Google account index saved by 0.1.0 that the login no longer holds is reset
  to the first account at startup (with a message).

## [0.1.0] — 2026-10-29 (first public release)

### Changed

* **Uses much less memory, and the same amount every run** (macOS): freed memory is
  handed back to the system instead of being kept by the allocator, decoded artwork is
  capped by its real (decoded) size and also leaves the GPU when evicted (Settings →
  Storage → "Artwork in memory", 48 MB; Minimal / Fast: 16 MB), and the YouTube player
  script is no longer held in memory after it has been prepared.

* The player bar draws nothing on its own by default (the progress follows the
  once-a-second update); the animated styles cap their frame rate (13–22 fps), only run
  while music plays and the bar is visible, and use the spectrum tap only then. The
  Minimal / Fast profile sets every player-bar option to its cheapest value.

* **Now Playing uses about half the CPU while lyrics animate** (≈ 12 % instead of
  22–28 % on an M-series Mac with word-synced lyrics; designs without the lyrics list
  1–5 %): only the lyrics on screen are laid out, lines keep their measured layout,
  frames are drawn only while a word is being sung (otherwise the screen wakes at the
  next word or line), static parts are cached, the glow background drifts at 30 fps.
* The PoToken attestation is renewed on a background thread 30 minutes before it
  expires, so songs no longer wait ~1 s for it every ~12 h.
* The app is now called **TuneDesk** (app id `tunedesk`): settings live in
  `~/Library/Application Support/tunedesk` (macOS), `~/.config/tunedesk` (Linux),
  `%APPDATA%\tunedesk\config` (Windows), and keychain entries use the service `tunedesk`.

### Added

* **Waveform progress style** (Settings → Appearance → Player bar → Waveform): the
  song's loudness shape as bars, SoundCloud-style, played part in the accent. Static
  (no animation, the fill follows the once-a-second update); a flat row of low bars
  until the shape is known, then one short fade. Worked out once per song in the
  background from the audio already being downloaded (Opus: read from the bitstream,
  ~20 ms of CPU; AAC: decoded, ~0.15–0.3 s) and cached in `<config>/cache/waveforms/`
  (~260 bytes per song). Also in the mini player.
* **Editable keyboard shortcuts** (Settings → Keyboard shortcuts): change, clear or reset
  any shortcut by pressing the new keys; conflicts ask before taking keys from another
  action, keys needed for typing and system keys are refused; changes apply at once, also in
  the menu bar, and travel with settings exports. ⌘/ (Help → Keyboard Shortcuts) shows a
  cheat sheet.
* **Choose what the player bar shows** (right-click the bar, or Settings → Appearance →
  Player bar → *Show on the bar*): like, source / quality tag, shuffle, repeat, the
  SponsorBlock prompt, the Music Together status, lyrics, queue, sleep timer, Cast, mini
  player, Now Playing, volume slider, and a new **Add to playlist** button (off by
  default). Hidden items leave no gap; narrow windows give their room to the next buttons.
* **Quality menu on the source tag**: click "YouTube · AAC 128 kbps" to switch between
  Low, Normal and High; the playing song changes stream at the same position without a
  pause, and the choice is saved like in Settings → Playback.
* **Redesigned player bar**: track info · transport + progress · secondary controls, the
  transport centred in the window; elapsed and remaining time (click to switch), a
  thumb and time bubble on hover, a lyrics button, active states for queue / lyrics /
  mini player / Now Playing, a status line under the artist, 32–40 pt buttons, volume
  with mute, scroll-to-change and a percentage bubble. Narrow windows hide buttons by
  priority instead of squeezing them.
* **Progress bar styles** (Settings → Appearance → Player bar): Standard, Thick, Simple,
  Wavy and Equalizer (live spectrum bars, peak markers optional; still click / drag to
  seek). Also in the mini player, which can now seek.
* **Bar background that reacts to playback** (off by default): artwork glow, dynamic
  gradient in the artwork's colours (direction, intensity, optional slow drift) or a
  pulse that follows the loudness; dims when paused (switchable).
* **Visualizer in Now Playing** (off by default): a thin spectrum strip at the bottom.

* **Customize theme** (Settings → Appearance): six presets (Standard, Midnight, Rosé,
  Forest, High contrast, Artwork), accent colour (swatches, hex, hue / saturation),
  darkness (dim → pure black), surface tint, contrast (standard / medium / high), corners
  (sharp / regular / round), density (compact / regular / comfortable), any installed
  font, a live preview and **Reset to default**. Accents too dark to read are lightened
  automatically (WCAG contrast). With default settings the look is unchanged.
* **Better dynamic theme**: a full Material-style palette from the artwork (accent,
  secondary, tertiary, optional background tint) in four styles — vibrant, muted,
  monochrome, true to artwork — with a strength slider and ~400 ms colour fades between
  songs. Artwork colours are only analysed while the dynamic theme is on.
* **Profiles**: named looks to switch with one click (theme, Customize theme, Now
  Playing, lyrics display, player bar style); new / duplicate / rename / delete /
  import / export. Built in: **Default** and **Minimal / Fast** (no artwork colour
  analysis, no colour fades, plain Now Playing on a solid background, no lyrics
  animation, translation or romanization — about 1 % CPU with Now Playing open instead
  of ~9 %).
* **Backup & sync** (Settings): **Export / Import settings** as JSON with a preview of
  every change by section, never including passwords, keys, cookies or the account;
  older and newer files are handled. **Theme files** (`.tunedesk-theme.json`) share just
  the look.
* **Download all** on Liked songs, library songs, playlists and albums, with progress
  ("Downloading 3 of 12", Cancel) and **Remove downloads** once everything is stored.
  Settings → Storage: **Download liked songs** and **Remove download when unliked**.

* **The app speaks 18 languages**: English, German, Spanish, Brazilian Portuguese,
  French, Italian, Dutch, Polish, Russian, Ukrainian, Turkish, Indonesian, Vietnamese,
  Japanese, Korean, Simplified and Traditional Chinese, and Hindi — every screen, menu,
  the menu-bar item, notifications and dialogs. It follows the system's preferred
  languages; **Settings → Appearance → Interface language** (also on the welcome guide's
  first page) switches it live. Plural forms, dates, relative times and digit grouping
  follow the language. On a fresh install the YouTube Music content language and country
  start from the system's language and region. Some translations come from ArchiveTune's
  community translations (GPL-3.0).
* **Spectrum feed for visualizers** (`music-core`): `Player::spectrum(bands)` returns
  smoothed log-spaced bands, peak-hold markers and an RMS level of what is playing (after
  EQ / normalization / crossfade, before the volume), computed on demand from a lock-free
  tap on the output; `Player::set_spectrum_enabled` makes it free while no visualizer is
  shown. Groundwork for a spectrum-bar progress bar and Now Playing visualizers.
* **Full-length YouTube playback** (`youtube::WebResolver`): the TV client with YouTube's
  JavaScript challenges solved by yt-dlp's public-domain solver in QuickJS, or the mobile
  web client with a BotGuard PoToken (hidden WKWebView on macOS, ported from ArchiveTune).
  Waits out YouTube's pre-roll ad delay while playing the song's start from the iOS
  client, so playback starts in seconds. Settings → Playback → YouTube streams. Downloads
  are now full files. The ~1-minute preview remains as the last fallback.
* **Podcasts**: show pages (description, episodes with date / length / description,
  infinite scroll, Play / Shuffle / Add to queue, Save to library when signed in),
  episode pages, Library → Podcasts (signed in); podcasts and episodes from search, Home
  and Explore open there.
* **DNS-over-HTTPS** (Settings → Network → DNS): Cloudflare, Google, Quad9 or a custom
  RFC 8484 URL, answers cached per TTL, optional fallback to the system resolver, Test DNS.
  Applies to all requests (YouTube Music, streams, lyrics, modules, …).
* **Change cover…** for your own YouTube Music playlists (not yet tried against a real
  account).
* Search shows YouTube's spelling corrections ("Showing results for … · Search instead
  for …", "Did you mean …?").
* **Kanji readings without the 48 MB build**: Settings → Lyrics → Romanization downloads
  the Japanese dictionary on demand (10.5 MB, checksum-verified, 45 MB on disk) with
  progress, cancel and remove; until then romanization is kana-only.
* **Translations are kept** in the library: reopening a song doesn't translate it again.
* **Multichannel Opus** (5.1 / 7.1, mapping family 1 and 255) plays, mixed down to stereo.
* **Five more Now Playing designs**: Immersive (full-bleed artwork, big type, icon
  controls), Card (a floating card with the audio format and volume), Minimal (title and
  time only, the window fills as the song plays), Editorial (two tones, round cover, a big
  PLAY / PAUSE button) and With queue (the next songs beside the player). Settings →
  Appearance or the Now Playing header's design menu.
* **Lyrics cards**: share button above the lyrics → pick up to six lines → a card image
  with the artwork (blurred, album colour, dark or light background; alignment, size,
  square / portrait / story, cover on or off) → Save image… (PNG) or Copy image.

### Changed

* **YouTube songs start faster**: about 0.5–0.8 s from click to sound instead of
  1.2–2.3 s (the start of the file downloads while the stream is still being resolved,
  the challenge solver loads during the player request, the session renews in the
  background).
* **Hide music videos** now also hides music videos and user videos inside playlists,
  albums, radio, queues and card actions (each track carries its YouTube video type); an
  album that YouTube only offers as official-audio videos stays complete.

* **Music Together across networks**: a self-hostable relay server (`music-relay`, one
  small binary; TLS through a reverse proxy) — set it in Settings → Music Together and
  press **Host via relay**; the link works from anywhere.
* **Hand the host role to a guest** (Listen together → **Make host**): the queue, position,
  repeat and room settings move to them, everyone follows the new host automatically.
* **Chromecast**: cast the queue to a Chromecast, Google TV or Nest speaker from the
  player bar's Cast button (Settings → Chromecast); play / pause / seek / skip / volume
  control the device, Stop casting continues on this computer. Not yet tested with a real
  device.
* **AI content filter** (optional, off by default): hide songs in categories you choose
  (explicit lyrics, AI-generated music, holiday songs, …, or your own) using an
  OpenAI-compatible model you configure (OpenAI, OpenRouter, or a local Ollama / LM
  Studio). Settings → AI content filter.
* **PoTokens on Windows** (WebView2) and, optionally, **Linux** (WebKitGTK, build
  feature `webkitgtk-potoken`) for the web stream client. Not yet tested on those systems.

### Fixed

* **Seeking far ahead right after a song starts** no longer goes silent (or gives up
  after 10 s and jumps back): YouTube only serves the rest of the file once its
  pre-roll ad could have been skipped (a few seconds; longer for unskippable ads), so
  until then the song keeps playing where it was, the bar shows the target with a
  spinner, and playback moves there as soon as those bytes arrive.
* **High quality (Opus) playback uses ~100× less CPU for decoding**: the bundled Opus
  decoder computed a key transform the slow way (about a quarter of a CPU core while a
  song played); it now uses an FFT. Output is unchanged (90 dB SNR to the original).
* **Opus songs show their length**: the bar showed `--:--`, had no progress fill and
  couldn't seek by position for High-quality streams and downloaded `.webm` files.
* **Shortcuts on Windows and Linux** use Ctrl (they were bound to the Windows / Super key).
* **Esc with a menu or popover open** only closes it (it also closed Now Playing or went
  back).
* The player bar's title no longer runs into the transport buttons when the Together
  chip and the Cast button are shown in a narrow window: the title truncates, chips
  collapse, the Cast button stays.

* **Gapless AAC**: the encoder delay (~25–50 ms of warm-up) and the end padding of AAC
  files (local M4A, YouTube, modules) are trimmed, so gapless albums no longer get a gap
  or click between tracks; seeks in AAC are sample-accurate.
* **Expiring module streams** are renewed through their module shortly before they
  expire or when the server rejects them, and continue at the same byte (also for a
  preloaded next song and for downloads).
* Age-restricted YouTube videos no longer throw away the shared session (they were taken
  for YouTube's bot check), so they fail and get skipped in well under a second.
* Albums with more than 200 tracks showed only the first 200; the rest now loads (also
  for Play / Shuffle from cards). Playing a playlist from its card plays up to ~1,100
  songs instead of the first 100.
* A saved search could disappear from the search history: saves and removals ran on
  separate threads in no guaranteed order, and could be lost when quitting while the
  library was busy. They are now written in order and flushed on quit; searches that
  differ only in letter case are one entry.
* Podcast episode lengths ("2 hr 43 min") were missing.
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

## Before 0.1.0 — first development build (not released)

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

* YouTube songs play as ~1-minute previews without a PoToken (fixed in 0.1.0).
* Account features and the Linux / Windows builds are not verified on real accounts /
  machines yet; builds are not signed with a Developer ID / Authenticode certificate.
* The full, maintained list is in [docs/BACKLOG.md](docs/BACKLOG.md).
