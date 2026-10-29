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
* [ ] **⋯ menu** at the right of the header: Settings, Keyboard shortcuts, Check for
      Updates, About, Quit all work; shortcuts shown with Ctrl.
* [ ] **Mini player** (Ctrl+Shift+M): stays above other windows (click another app); drag
      it by its top strip; × closes it; its position comes back next time.
* [ ] **Dates**: History / Stats days match your local date (not UTC) for plays near
      midnight.
* [ ] **Animations**: Windows Settings → Accessibility → Visual effects → Animation effects
      off, restart the app: Settings → Appearance → Motion → System behaves as Reduced.
* [ ] **Media keys / SMTC**, **fonts (Segoe UI, CJK titles)**, **high DPI** (125 % /
      150 % scaling, moving between monitors): see 5.8.

## 6. Reporting a problem

For each failed check, send:

* the checklist item (e.g. "5.4 step 1");
* what you saw (a screenshot helps);
* the console output, best with `$env:MUSIC_TRACE = "1"`. Copy the window's text, or
  run `.\target\release\music-gpui.exe 2>&1 | Tee-Object log.txt`;
* Windows version (`winver`) and whether it was the `cargo run` build, the zip or the
  installer.
