# Testing on Windows (and building the .exe)

How to check a build on Windows 10/11 by hand, which automated checks to run, and how to
make the `.exe`, `.zip` and installer. Most of the app was developed and verified on
macOS; Windows has only been built in CI and type-checked. That is why the
Windows-specific parts below (media keys, keychain, WebView2 PoToken, installer) are
marked **untested**.

Every check lists what to do and what you should see. Note any difference, together with
the console output (see [Reporting](#reporting-a-problem)).

---

## 1. One-time setup

1. **Visual Studio 2022 Build Tools** (or Visual Studio) with the **"Desktop development
   with C++"** workload. It provides MSVC, the Windows SDK and `rc.exe`; the build embeds
   the icon and version info with them.
2. **Rust** from <https://rustup.rs> (default MSVC host). The repository pins the
   toolchain (`rust-toolchain.toml`, Rust 1.99); `cargo` installs it on first use.
3. **Git**, then clone the repository:
   ```powershell
   git clone <repo-url> cratetune
   cd cratetune
   ```
4. For the installer only: **Inno Setup 6** (<https://jrsoftware.org/isinfo.php>).
   `iscc` must be on `PATH`, or installed in the default `Program Files (x86)\Inno Setup 6`.
5. **WebView2 Runtime**: already part of Windows 10/11. The app needs it for full-length
   YouTube streams through the PoToken route.

A clean release build takes 10–20 minutes the first time (hundreds of crates).

## 2. Automated checks

Run these in a PowerShell window from the repository root. All of them should pass. CI
runs the same commands on `windows-2022`.

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Live checks against YouTube Music (they need internet; nothing is played):

```powershell
# Every anonymous InnerTube endpoint, incl. podcasts, a 294-track album and spelling
# correction. Last line should be "N ok, 0 failed []".
cargo run --release -p music-core --example browse -- "daft punk"

# The same through DNS-over-HTTPS (Cloudflare); first line shows "via Doh".
$env:MUSIC_DOH = "cloudflare"; cargo run --release -p music-core --example browse -- "daft punk"; Remove-Item Env:MUSIC_DOH

# Real DoH providers: Cloudflare and Google must answer; Quad9 may time out (its port 5053
# is blocked on some networks; that is reported, not failed).
cargo test -p music-core --lib doh -- --ignored --nocapture
```

## 3. Building the .exe, zip and installer

```powershell
cargo build --release -p music-gpui          # -> target\release\music-gpui.exe
pwsh scripts\bundle-windows.ps1              # build + zip + installer
pwsh scripts\bundle-windows.ps1 -NoBuild     # reuse the existing release build
pwsh scripts\bundle-windows.ps1 -NoInstaller # zip only (no Inno Setup needed)
```

Output in `target\bundle\`:

* `cratetune-<version>-windows-x64.zip`: `cratetune.exe` + `LICENSE`, portable.
* `cratetune-<version>-windows-x64-setup.exe`: per-user installer (no admin rights).

To make a GitHub release instead, push a tag `v<version>`. `.github/workflows/release.yml`
then builds every platform, Windows included, and attaches the files to a draft release.

**Check the .exe itself:**

* [ ] Explorer shows the app icon on `cratetune.exe`. Properties → Details shows the name
      and version.
* [ ] The installer installs without admin rights, creates a Start-menu entry, and the
      app starts from it. Uninstall (Settings → Apps) removes it. The settings folder
      `%APPDATA%\cratetune\config\` stays.
* [ ] Note whether a console window opens next to the app. The binary currently uses the
      console subsystem; that helps testing, but a release may want it hidden.

## 4. Test setup for each session

Use a throw-away settings folder so tests don't touch your real library. Keep volume at 0
when you don't want to hear anything:

```powershell
$env:MUSIC_CONFIG_DIR = "$env:TEMP\cratetune-test"   # delete it to start fresh
$env:MUSIC_VOLUME = "0.0"                            # muted (real audio device)
$env:MUSIC_TRACE = "1"                               # log routes, fetches, playback
$env:MUSIC_CONSOLE = "1"                             # release builds: log to this terminal
.\target\release\music-gpui.exe
```

* `$env:MUSIC_DEMO_CLOCK = "1"`: fake playback without an audio device.
* `$env:MUSIC_SECRET_STORE = "file"`: skip Windows Credential Manager (secrets go to the
  0600 settings file). Leave it unset to test the Credential Manager itself.
* `$env:MUSIC_ROUTE = "podcast:MPSPPLk1Sqn_f33KuWf3tW9BBe_4TP7x8l0m3T"`: open a page at
  start (also `album:ID`, `playlist:ID`, `episode:ID`, `library:podcasts`,
  `settings:network`, …).
* `$env:MUSIC_SCRIPT = "5:@submit=daft punk,9:@quit"`: timed actions; the full list is in
  the README under *Environment hooks*.
* Clear a variable with `Remove-Item Env:NAME`.

The console prints `music-gpui: …` lines (errors, and with `MUSIC_TRACE` routes / fetches /
playback). Copy them into problem reports. Release builds have no console of their own:
`MUSIC_CONSOLE=1` sends the log to the terminal that started them (closing that terminal
then closes the app).

## 5. Manual checklist

### 5.1 Smoke test (any build)

* [ ] The app starts; Home shows mood chips and shelves with artwork.
* [ ] Search (Ctrl+F or click the field) for "daft punk": suggestions appear while typing.
      Enter opens the results with the All tab and category tabs.
* [ ] Clicking a song plays it: the bar shows title, progress and time. With
      `MUSIC_VOLUME` unset you hear it.
* [ ] Within ~5 s the bar shows no "Preview" badge: full-length stream (TV client or
      WebView2 PoToken). If it says Preview, note it with the console output.
* [ ] Seek by clicking the progress bar; next / previous; pause / play with Space.
* [ ] Now Playing (Ctrl+L) opens with lyrics for a popular song.
* [ ] Close and reopen the app: the queue comes back paused at the same song.

### 5.2 Hide music videos

1. Settings → Content & region → turn **Hide music videos** on.
2. Open `playlist:RDCLAK5uy_lfTOUBaGMSEoO2The1XiaG75RghLtrVtM` ("Indie Disco") with
   `MUSIC_ROUTE`, or search for it.
   * [ ] The first song is **"Last Nite"** (The Strokes). With the switch off it is
         "Feel Good Inc." (a music video).
3. Search "daft punk":
   * [ ] The **Videos** tab is gone, and the All tab has no "Video" rows.
4. Open `album:MPREb_K8qWMWVqXGi` (Random Access Memories).
   * [ ] All 13 tracks are still there. YouTube only has this album as official-audio
         videos, so it is deliberately kept whole.
5. Start a radio from any song (right-click → Start radio).
   * [ ] The queue panel (Ctrl+Alt+Q) holds songs only, no music videos.
6. Turn the switch off again: pages reload and the videos are back.

### 5.3 Podcasts

1. Search "joe rogan" → **Podcasts** tab → click "The Joe Rogan Experience" (or
   `MUSIC_ROUTE=podcast:MPSPPLk1Sqn_f33KuWf3tW9BBe_4TP7x8l0m3T`).
   * [ ] Header with art, "PowerfulJRE", "20+ episodes", Play / Shuffle / Add to queue.
   * [ ] Episode rows show title, "4d ago · 2:43:00"-style date and length, and two lines
         of description.
   * [ ] Scrolling to the bottom loads more episodes (the count grows, no error footer).
2. Click an episode.
   * [ ] It plays; the length in the bar is the full episode (2+ hours), not ~1 minute.
   * [ ] Seeking to the middle works.
3. Right-click an episode → **Open episode page**.
   * [ ] Episode page with show name, date · length, Play / Add to queue /
         **Go to podcast**, and the full description. Go to podcast returns to the show.
4. Right-click an episode → **Go to podcast** opens the show (not an artist page).
5. Library → **Podcasts** tab while signed out.
   * [ ] "Sign in to see your podcasts".
6. *(Signed in, untested so far)* The show page has **Save to library**. After saving,
   the show appears in Library → Podcasts; removing it works too.

### 5.4 Long albums

1. Open `album:MPREb_8Up7Y28SB9h` (J.S. Bach: Complete Edition, Vol. 1/10).
   * [ ] The header says "294 songs". A loading footer appears briefly under the list,
         then the list ends at track **294** ("XV. Canon a 4 Augmentationem…").
   * [ ] Track numbers 100+ are not wrapped onto two lines.
2. Right-click the album's card (e.g. in search results) → **Play**.
   * [ ] The queue has 294 songs.

### 5.5 Spelling correction and search history

1. Search **justice genisis** (typo) and press Enter.
   * [ ] Below the tabs: "Showing results for **justice genesis** · Search instead for
         **justice genisis**".
   * [ ] Clicking **justice genisis** reloads the All tab without the correction; it now
         says "Did you mean **justice genesis**?".
   * [ ] Clicking the corrected query searches for it.
2. Search history: search, in this order, "justice genesis", "justice genisis", "Daft
   Punk", "daft punk", "queen", then quit at once (Alt+F4 or the tray / Quit).
   * [ ] After a restart, the empty search field's dropdown (click into it) lists:
         queen, daft punk, justice genisis, justice genesis. "Daft Punk" and "daft punk"
         are one entry, and nothing is missing.
   * [ ] The × on an entry removes it, and it stays gone after a restart.
   * Scripted version of the same:
     ```powershell
     $env:MUSIC_SCRIPT = "3:@submit=justice genesis,6:@submit=justice genisis,9:@submit=Daft Punk,10:@submit=daft punk,11:@submit=queen,11.1:@quit"
     .\target\release\music-gpui.exe
     $env:MUSIC_SCRIPT = "3:@suggest,4:@quit"
     .\target\release\music-gpui.exe    # prints: recent ["queen", "daft punk", "justice genisis", "justice genesis"]
     ```

### 5.6 DNS-over-HTTPS (Settings → Network → DNS)

1. Pick **Cloudflare** → Apply → **Test DNS**.
   * [ ] "music.youtube.com → 142.250.… via DNS-over-HTTPS in N ms".
   * [ ] Pages still load, songs play, artwork shows.
2. Test DNS again: it says "(cached)".
3. **Google**: same result. **Quad9**: works, or fails with a timeout if port 5053 is
   blocked. With the fallback switch on, the app keeps working either way.
4. **Custom** with `https://cloudflare-dns.com/dns-query` → Apply → Test DNS works.
   * [ ] An `http://…` or garbage URL is refused with a message, and the previous
         setting stays.
5. Fallback: Custom `https://127.0.0.1:9/dns-query` (nothing listens there):
   * [ ] fallback **on**: Test DNS says "via the system resolver" and pages load.
   * [ ] fallback **off**: Test DNS fails and pages show load errors with Retry.
6. Restart the app: the DNS choice is kept (`%TEMP%\cratetune-test\settings.json` →
   `"network": {"dns": …}`).
7. **System** again → Test DNS says "via the system resolver".

### 5.7 Playlist covers (signed in; untested so far)

1. Sign in (Settings → Account → **Paste cookie…**), then Library → Playlists → one of
   *your own* YouTube playlists, or open it online.
2. **Change cover…** → pick a JPEG / PNG.
   * [ ] "Uploading the cover…", then "Playlist cover changed". The page shows the new
         (square) cover, and music.youtube.com shows it too.
   * [ ] A non-image file gives "This file isn't an image the app can read".
   * Errors appear in a notification and the console. Please send them: this path has
     never run against a real account.

### 5.8 Windows-specific (untested)

* [ ] **Editable shortcuts** (Settings → Keyboard shortcuts): the section shows Ctrl+Shift+M
      style names; "Change" + Ctrl+Shift+K records it and Ctrl+Shift+K then works, the old keys
      don't; Alt+Tab / Alt+F4 are refused; typing in the search field is unaffected; ⌘/ is
      Ctrl+/ (cheat sheet). Same on Linux (Super key shown as "Super").
* [ ] **Shortcuts use Ctrl** (the docs write ⌘; on Windows / Linux that is Ctrl): Ctrl+F
      search, Ctrl+L Now Playing, Ctrl+Alt+Q queue, Ctrl+→ / Ctrl+← next / previous,
      Ctrl+↑ / Ctrl+↓ volume, Ctrl+1/2/3 Home / Explore / Library, Ctrl+, settings, Space
      play / pause. Before this fix they were bound to the Windows key. Also check that
      Ctrl+← / Ctrl+→ in the search field still move the cursor by word (or note that they
      skip tracks instead).
* [ ] **Media keys** / the volume flyout's media controls (SMTC) show the song and
      control play / pause / next.
* [ ] **Credential Manager**: without `MUSIC_SECRET_STORE=file`, paste a Last.fm secret or
      a proxy password, restart, and it is still there. Windows Credential Manager has a
      `cratetune` entry.
* [ ] **WebView2 PoToken**: Settings → Playback → YouTube streams → **Web + PoToken**. A
      song then plays in full (no "Preview"). Console lines mention PoToken / WebView2.
* [ ] **Window**: resize, maximize, restart; the size and position come back. The mini
      player (Ctrl+Shift+M) stays on top.
* [ ] **Fonts**: no missing glyphs (boxes) in titles with accents / CJK characters.
* [ ] **Folder picker**: Local files → add a music folder → Rescan finds MP3 / FLAC / M4A.

### 5.9 Player bar (Settings → Appearance → Player bar)

* [ ] Hover the progress bar: a thumb and the time under the pointer appear; drag seeks.
* [ ] Scroll (mouse wheel and touchpad) over the volume: it changes, a "NN %" bubble shows.
* [ ] Shrink the window to its minimum width: buttons on the right disappear one by one,
      nothing overlaps the title or the transport.
* [ ] Progress style **Equalizer** while playing: the bars move with the music and fall
      when paused. **Wavy**: the wave moves while playing and flattens when paused.
* [ ] Background **Dynamic gradient** / **Artwork glow** / **Pulse**: colours of the
      cover; dimmed when paused.
* [ ] Task Manager: with the default look the app stays near idle while a song plays
      (Now Playing closed); with Wavy / Equalizer a few percent; paused, near 0.

### 5.10 Windows checklist for the user (after updating to the Windows polish build)

Run the installed app normally (Start menu) unless a step says otherwise.

* [ ] **No console window.** Only the app's window opens; nothing behind it. From
      PowerShell with `$env:MUSIC_CONSOLE = "1"` the `music-gpui: …` log appears in that
      terminal.
* [ ] **One instance.** With the app running, start it again (Start menu or the `.exe`):
      no second app; the running window comes to the front, also when it was minimized.
      After Settings → About → update + restart, the new version starts (once).
* [ ] **Sign in with Google.** Settings → Account → **Sign in with Google…**: a "Sign in to
      YouTube Music" window with Google's page opens, centred on the app. Sign in; when
      YouTube Music appears the window closes and the account card fills in. Closing the
      window early changes nothing. Note if Google says "This browser or app may not be
      secure" (then **Paste cookie…** still works). Signing in again later asks for the
      password again (nothing is kept).
* [ ] **Paste cookie** dialog says F12 for the developer tools.
* [ ] **Mouse wheel** on Home, an album, the library song list, Settings, the queue and the
      sidebar: each notch glides smoothly (no jumps), about 3 lines; fast spinning adds up
      and stops promptly; reversing direction turns at once. Settings → Appearance →
      Motion → **Off**: notches jump instantly. The touchpad still scrolls as before.
      Wheel over the volume slider still steps the volume by one notch.
* [ ] **Notification-area icon** (maybe under the ^ overflow): the CrateTune logo. Right
      click: menu with the song, play / pause, next, previous, mini player, show window,
      quit — each works. Left click: the window comes to the front.
* [ ] **⋯ menu** at the left of the top bar: Settings, Keyboard shortcuts, Check for
      Updates, About, Quit all work; shortcuts shown with Ctrl.
* [ ] **Mini player** (Ctrl+Shift+M): stays above other windows (click another app); drag
      it by its top strip; × closes it; its position comes back next time.
* [ ] **Dates**: History / Stats days match your local date (not UTC) for plays near
      midnight.
* [ ] **Animations**: Windows Settings → Accessibility → Visual effects → Animation effects
      off, restart the app: Settings → Appearance → Motion → System behaves as Reduced.
* [ ] **Media keys / SMTC**, **fonts (Segoe UI, CJK titles)**, **high DPI** (125 % /
      150 % scaling, moving between monitors): see 5.8.

### 5.11 Top bar and window controls (Windows; also worth a look on Linux)

The app draws its own title bar (`titlebar.rs`). Check at 100 %, 125 % and 150 % display
scaling, and after moving the window to a monitor with other scaling.

* [ ] **Layout:** one bar across the top: ⋯, ← →, Home + search centred, the account
      circle, then minimise / maximise / close flush in the top-right corner (46 × 32 each).
      No native title bar above it, no gap or 1 px line at the top, nothing cut off when
      maximised.
* [ ] **Buttons:** minimise minimises; maximise maximises and its icon turns into restore
      (two squares), which restores; close quits (as the old ×). Hover: a light square on
      minimise / maximise, **red** on close with a white ×. Pressing and dragging off a
      button before releasing does nothing.
* [ ] **Snap layouts (Windows 11):** hover the maximise button for about a second: the snap
      layout flyout appears; picking a zone snaps the window.
* [ ] **Drag:** drag the empty bar (left of Home, right of search, below the caption buttons)
      to move the window; drag to the top of the screen maximises, to a side snaps (Aero
      Snap); dragging a maximised window down restores it. Win+← / → / ↑ / ↓ still work.
* [ ] **Double-click** the empty bar: maximises; again: restores. Double-clicking a button
      or the search field does **not** maximise.
* [ ] **Right-click** the empty bar: the system window menu (Restore, Move, Size, Minimise,
      Maximise, Close); Alt+Space opens it too.
* [ ] **Resize** from the top edge (a thin band, also above the caption buttons) and the
      other edges / corners as before.
* [ ] **Clicks still work** on ⋯, ← →, Home, the search field (typing, suggestions,
      Enter), and the account circle (menu: Sign in, or your name / Account settings / Sync
      now / Sign out).
* [ ] **Inactive window:** the caption glyphs dim when another app is focused.
* [ ] **Glyphs:** the caption icons look like other Windows 11 apps' (Segoe Fluent Icons;
      Windows 10: Segoe MDL2 Assets), crisp at every scaling.
* [ ] **Now Playing** (Ctrl+L) opens below the bar; the caption buttons and the drag still
      work while it's open. **F11** (full screen): the bar has no caption buttons, Esc / F11
      leave full screen.
* [ ] **Ctrl+F** focuses the search field; in Settings it focuses the settings search first,
      Ctrl+F again the music search. Ctrl+[ / Ctrl+] go back / forward.
* [ ] **System title bar:** Settings → Appearance → Window → **Use the system title bar** →
      **Restart now**: the normal Windows title bar is back with the old header (← →,
      search, ⋯ on the right). Switch it off and restart again.
* [ ] **Linux** (GNOME / KDE, Wayland and X11): with client-side decorations there are
      round minimise / maximise / close buttons on the right (only those the desktop
      supports), dragging the bar moves the window, double-click maximises, right-click
      opens the window menu, the edges resize. With server-side decorations (KDE / X11
      without a compositor) the desktop's own title bar shows above the bar and the bar has
      no buttons of its own.

### 5.12 Sideways rows: arrows, wheel, drag (Windows mouse; also Linux)

Rows that scroll sideways (`components/hrow.rs`): Home's shelves and quick picks, the
category chips when they don't fit, artist / album / Explore / Charts / mood pages, Stats.
Use a normal wheel mouse (not a touchpad) with smooth scrolling on (the default).

* [ ] **Arrows:** a shelf whose cards don't fit has **‹ ›** at the right of its title (after
      "See all"). › glides about one window-width and stops with a card at the left edge;
      ‹ goes back. ‹ is dimmed at the start, › at the end; a shelf that fits has neither.
      With Settings → Appearance → Motion **Off** they jump instead of gliding.
* [ ] **Shift + wheel** over a row scrolls that row sideways (smoothly, ~1 card per notch)
      and not the page. A **tilt wheel** (or a horizontal wheel, e.g. MX Master's thumb
      wheel) does the same without Shift.
* [ ] **Plain wheel** over a row scrolls the **page**, as before; the row doesn't move.
* [ ] **Drag:** press on a card and drag sideways: the row follows the pointer with a
      closed-hand cursor; releasing doesn't open or play the card. A click without moving
      (or moving only a few pixels) still opens the album / plays the song.
* [ ] **Scrollbar:** hovering a row shows a thin bar under it; dragging the bar's thumb
      scrolls the row (and doesn't start a drag of the cards).
* [ ] **Chips:** in a narrow window, Home's category chips scroll the same way; hovering
      them shows round ‹ › on their ends.
* [ ] **Position kept:** scroll a shelf to the middle, scroll the page far down and back:
      the shelf is still where you left it.
* [ ] **Touchpad** (if the laptop has one): a two-finger sideways swipe over a row scrolls
      it; a vertical swipe scrolls the page.

## 6. Performance

Three scripts, all on release builds with a throw-away settings folder:

| Script | Where | Network | What it checks |
|---|---|---|---|
| `scripts/perf-windows.ps1` | Windows desktop | yes | CPU, memory, frame rate and draw cost (6.1) |
| `scripts/perf-macos.sh` | Mac with an awake display | yes | the same, plus a draw budget for 180 Hz (6.2) |
| `scripts/perf-frames.sh` | CI (macOS, Windows), any desktop | no | draw cost must not grow down a long page (6.3) |

### 6.1 Windows

`scripts/perf-windows.ps1` runs a release build through three scripted sessions (Home:
idle, two draw benchmarks, 40 mouse-wheel notches; a song played muted with the
Equalizer bar; Now Playing open) and prints how heavy the app is and how smoothly it
draws. It needs internet (Home feed, one song) and uses a throw-away settings folder.

```powershell
cargo build --release -p music-gpui
pwsh scripts\perf-windows.ps1                                   # default settings, with checks
pwsh scripts\perf-windows.ps1 -Settings "$env:APPDATA\cratetune\config\settings.json" -NoChecks
pwsh scripts\perf-windows.ps1 -Exe path\to\cratetune.exe -NoChecks   # e.g. an installed release
```

`-Report out.md` also writes the table to a file; the sessions' logs stay in
`%TEMP%\cratetune-perf`. It exits 1 when a check fails (run it with default settings
for that; your own look and window size can legitimately be slower):

* **Scrolling ≥ 90 % of the display's refresh rate** (capped at 120 Hz): wheel frames
  per second while scrolling Home (`MUSIC_TIMINGS`, see below).
* **Scrolled-down draw ≤ 1.5 × top-of-Home draw**: a full frame must not get more
  expensive the further the endless feed is scrolled (page sections far outside the
  viewport aren't built, `components/cull.rs`).
* **Idle CPU ≤ 5 % of one core** on Home with nothing playing.

What the numbers mean:

* **Full window draw** (`@bench`): CPU to build, lay out and paint the whole window
  once, without presenting. At 180 Hz a frame has ~5.5 ms (less in practice, since
  Windows composes before the refresh); a frame that misses waits for the next one, so
  scrolling then runs at half the refresh rate (steadily, not stuttering).
* **Scrolling**: `app N renders/s, interval p50 / p95 / max` lines of `MUSIC_TIMINGS=1`
  while wheel notches glide. An even interval (p95 close to p50) is smooth; mixed 2- and
  3-refresh intervals are the stutter.
* **Bar**: the Equalizer / Wavy styles redraw on a ~40 ms timer by design (~22 fps), to
  keep CPU low while playing.
* CPU "% of one core" is the process's CPU time per second; Task Manager shows the same
  divided by the number of logical processors ("% of the machine").

**Measured** (2026-10-30, Ryzen 7 6800H, 16 threads, RTX 3050 Laptop driving a
1920 × 1080 180 Hz display, Windows 11; release builds; one song muted):

| | 0.2.1 | after culling |
|---|---|---|
| Full window draw, top of Home (default look, 1200 × 768) | 9.7–10.7 ms | 6.1–6.8 ms |
| Scrolling Home (default look, 1200 × 768) | ~100 fps, uneven | 165–180 fps |
| Scrolling Home (maximized, dynamic theme, tint, font 1.125) | 60–73 fps, uneven | 90–170 fps, even |
| Idle on Home (nothing playing) | 3–4 % of one core | 2–4 % of one core (≈ 0.2 % of the machine) |
| Playing, Equalizer bar + glow, Home | 9–13 % of one core | 6–12 % of one core (< 1 % of the machine) |
| Now Playing open (lyrics animating) | ~26 % of one core | about the same (not changed) |
| Memory | 190–270 MB private | 155–250 MB private |

The maximized range depends on how much the feed shows (it differs per run): when a
frame needs more than ~4 ms the window draws every other refresh (90 fps at 180 Hz).
What is left per frame is GPUI building and laying out the visible elements (memcpy into
its element arena, layout, the Windows heap); the GPU sat at ~14 %. Profiled with
[samply](https://github.com/mstange/samply) (`samply record` needs an elevated prompt on
Windows) on a build with `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` and
`CARGO_PROFILE_RELEASE_STRIP=none`. A different allocator (mimalloc) made no measurable
difference and used ~100 MB more.

### 6.2 macOS

`scripts/perf-macos.sh` is the macOS counterpart of `perf-windows.ps1`: the same three
sessions, numbers and checks. It needs internet and a display that is awake (it runs under
`caffeinate`; the window is kept above others with `MUSIC_ON_TOP=1`, since macOS sends a
covered window no frames). Settings go to a temporary folder (`MUSIC_CONFIG_DIR`,
`MUSIC_SECRET_STORE=file`), volume is 0, there is no menu-bar item (`MUSIC_TRAY=0`), and the
folder is deleted at the end.

```sh
scripts/perf-macos.sh                                     # builds release, default settings, checks
scripts/perf-macos.sh --binary /Applications/CrateTune.app/Contents/MacOS/cratetune --no-checks
scripts/perf-macos.sh --settings ~/Library/Application\ Support/cratetune/settings.json --no-checks
```

`--report out.md` also writes the table to a file, `--keep` keeps the sessions' logs. The
wheel glide that Windows and Linux have is turned on for the scrolling session
(`MUSIC_SMOOTH_WHEEL=1`), so `@wheel` notches produce frames as on Windows. CPU is the
process's CPU time per second from `ps` (100 % = one core), averaged over the phase;
memory is `footprint` (what Activity Monitor shows as Memory), not the resident size. The
display's refresh rate comes from `system_profiler SPDisplaysDataType` (the main display).

Checks: the four of 6.1 with the same thresholds, plus **Home draw p50 ≤ 6 ms** (top and
scrolled down). At 180 Hz a frame has 5.5 ms; an M-series Mac draws Home in ~3.2 ms, so
6 ms still passes on slower Macs (and noisy runs) but fails if a change makes a frame about
twice as expensive.

**Measured** (2026-10-30, M4 MacBook Air, 10 cores, built-in 60 Hz display, macOS 26.6;
release builds of this branch before and after merging 0.2.2 + the sideways rows, default
settings; runs where the window got its frames):

| Measure | Result |
|---|---|
| Idle on Home (nothing playing): CPU | 0.5–1.0 % of one core |
| Idle on Home: memory (footprint) | 144–185 MB |
| Full window draw, top of Home | p50 3.0–3.2 ms, p95 3.8–4.0 ms |
| Full window draw, scrolled down Home | p50 3.4–3.6 ms, p95 4.0–4.1 ms |
| Scrolling Home (wheel glide) | 60–61 fps (display 60 Hz), interval p95 16.8–17.3 ms |
| Playing, Equalizer bar, Home: bar | 20–22 fps, interval p95 51 ms |
| Playing, Home: CPU / memory | 7–8 % of one core, 192–203 MB |
| Now Playing open while playing | 9–10 app renders/s (lyrics and visualizer redraw as cached regions), 2–3.6 % of one core, 170–194 MB |

If a frame rate says **n/a** (and the draws are slower, ~5 ms, with a low footprint), the
window got no display-link frames for that session: it was covered (e.g. by another
always-on-top window, such as a second copy of this script), on another Space, or the
display slept. Run it again with the window unobstructed; the draw benchmarks don't need
frames, the scrolling, bar and Now Playing rates do.

### 6.3 Frame-time check (offline; CI)

`scripts/perf-frames.sh` needs no network and runs in CI as the step **perf: frame-time**
of the macOS and Windows jobs (Linux runners have no display or GPU). It opens Home with
`MUSIC_FIXTURES` pointing at the recorded InnerTube responses
(`crates/music-core/tests/fixtures/innertube`): Home, then the recorded continuation 20
times (each page with its own ids), every shelf filled to 12 distinct cards (as wide as a
real shelf), generated artwork (~60 different images, within the artwork memory budget),
and an HTTP agent that can't reach the network. It times full window draws (`@bench`: build, layout and paint, not presenting)
near the top (one screen down, while the feed is still short), scrolls to the end of the
feed (~60 shelves) and times them again:

```sh
scripts/perf-frames.sh                         # builds release first
scripts/perf-frames.sh --binary target/release/music-gpui --keep
MUSIC_NO_CULL=1 scripts/perf-frames.sh         # without culling: must FAIL
```

It fails when

* **the end-of-feed draw p50 > 1.5 × the near-top p50.** Runner speed cancels out. This
  is the regression the Windows stutter was: every section was built on every frame, so a
  frame cost more the longer the page got (`components/cull.rs` skips sections far outside
  the viewport). The reference is one screen down rather than the very top, where the
  title and chips fill a third of the window: there both views show only shelves, while
  against the very top the end drew 1.2–1.4x slower on the CI runners just for having more
  shelves in view, too close to 1.5;
* **either p50 is above the ceiling** (`--max-ms`, default 25; CI: 30 ms), only for gross
  slowdowns: about 3x the slowest culled draw the runners measured;
* the feed didn't load to its end, Home wasn't up before the first bench, or a bench
  didn't run (a broken setup is a failure, not a pass).

A failed run is repeated with a fresh app, up to three runs (`--attempts`), and the check
fails only if every run fails: a shared runner (or a desktop busy with builds) can be slow
for a few seconds, which skews one bench but not three runs in a row, while a frame that
grows with the page fails every run by a wide margin.

The draws run back to back (1 ms apart, after 60 warm-up draws): spaced 16 ms apart, macOS
moves the UI thread between efficiency and performance cores and the same build measured
1.4–5 ms from one run to the next.

**Calibration** (final setup: p50s near the top → at the end of the feed, and their
ratio; seven app runs per CI runner, three to five on the Mac):

| Machine | Culling on | `MUSIC_NO_CULL=1` |
|---|---|---|
| GitHub `macos-14` (M1 VM) | 5.8–8.4 → 4.8–7.2 ms, 0.64–1.16x | 8.9–11.2 → 55–61 ms, 4.9–6.9x |
| GitHub `windows-2022` (no GPU) | 9.7–10.2 → 6.8–8.6 ms, 0.68–0.87x | 16.5–16.6 → 99–100 ms, 6.0–6.1x |
| M4 MacBook Air (other builds running) | 2.1–4.1 → 1.9–4.0 ms, 0.73–1.11x | 2.8–3.3 → 23–25 ms, 7.2–8.9x |

The near-top bench is the slower one with culling on (it has the chips, the first shelf
and the feed header within half a screen above it), so culling passes with a wide margin
and its loss fails by 5–9x. Earlier setups were too close for comfort: compared with the
very top, the end drew 1.2–1.4x slower on the runners; with every page bringing its own
artwork (~700 images, beyond the artwork memory budget) up to 1.8x on Windows (a separate
cost, probably the artwork cache evicting; worth a look on long sessions); and with a
skipped section built from two elements instead of one, the end grew with the length of
the feed. If the check starts failing for an unrelated change, look at the step summary
first: a ratio near 1 with a high p50 is a slow runner (raise the ceiling), a ratio above
1.5 is a frame that grows with the page.

## 7. Reporting a problem

For each failed check, send:

* the checklist item (e.g. "5.4 step 1");
* what you saw (a screenshot helps);
* the console output, best with `$env:MUSIC_TRACE = "1"`. Copy the window's text, or
  run `.\target\release\music-gpui.exe 2>&1 | Tee-Object log.txt`;
* Windows version (`winver`) and whether it was the `cargo run` build, the zip or the
  installer.
