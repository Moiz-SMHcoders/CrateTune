# music-macos — native desktop YouTube Music client

A lightweight Rust desktop client (macOS, Windows, Linux) in the spirit of the
Android app ArchiveTune: a UI-agnostic core (`crates/music-core`) and a GPUI
app (`crates/music-gpui`, see [music-gpui](#music-gpui-gpui--gpui-component)).

**License:** GPL-3.0 (see `LICENSE`), because features are ported from
ArchiveTune (GPL-3.0, © Rukamori and contributors). Code ported from
Hummingbird is Apache-2.0 and listed in `crates/music-core/THIRD_PARTY.md`.
This is an independent project: it is not ArchiveTune and uses none of its
name or branding.

## Decision: GPUI + gpui-component

The UI framework was picked by building the same spikes in three stacks. `main`
now carries the winner; the other branches are kept for reference.

| Branch | UI stack | Outcome |
|---|---|---|
| `main` (= `ui/gpui-component`) | GPUI (`gpui-pre` 0.3.10) + gpui-component 0.8.1 | **Chosen** |
| `ui/slint` | Slint 1.18 (FemtoVG) | Built everything incl. karaoke lyrics, but word layout relies on Slint's internal text engine (Parley) and the accessibility tree is rebuilt every animated frame |
| `ui/hummingbird` | Fork of Hummingbird (GPUI, `gpui-unofficial`) | Works by converting YouTube tracks to tagged local files; 38.5 MB binary, fork upkeep. Used as a donor instead (EQ, media controls, scrobbling ported into music-core) |

Deciding test — Now Playing with word-by-word karaoke lyrics (release, macOS arm64, 60 Hz):

| | Slint | GPUI |
|---|---|---|
| CPU while lyrics animate | 79 % avg (19 % with accessibility compiled out) | 17 % avg |
| Frame rate | 53–61 fps (steady 60 without accessibility) | ~61 fps, worst gap 17.8 ms |
| RSS while animating | ~139 MB | ~104 MB |
| Idle, screen closed | 0 % | ~0 % |
| Binary | 17.7 MB | 21.8 MB |

Accepted risks of GPUI: a pinned pre-release snapshot (Zed deprioritises
non-Zed users), thin accessibility, no transforms/blur/masks on arbitrary
elements (worked around with custom painting and `anim.rs`). The core stays
UI-agnostic so the UI could be swapped again.

## Shared core (`crates/music-core`)

Blocking, thread-safe, no async runtime, so every UI toolkit can drive it from
its own executor.

* `InnerTube` — anonymous YouTube Music (`WEB_REMIX`) song search.
* `StreamResolver` trait + `IosClientResolver` — video id → audio URL (AAC itag 140).
* `Player` — audio thread with a pollable `PlaybackState`, loudness normalization
  from YouTube's per-format `loudnessDb`, `pause`/`resume`/`toggle_pause`/`seek`.
  Decoding is Symphonia 0.6 (`TrackDecoder`, a seekable `rodio::Source`), so seeking
  works inside truncated previews too; rodio/cpal only mix and output.
* `equalizer` — parametric EQ (port of Hummingbird's), applied live:
  `player.set_equalizer(EqPreset::Bass.settings())`, `player.clear_equalizer()`.
* `media_controls` — macOS Now Playing / media keys, Windows SMTC, Linux MPRIS
  (feature `media-controls`, default on).
* `scrobble` — Last.fm + ListenBrainz (`ScrobbleTracker` applies Last.fm's rules to
  `PlaybackState` snapshots; `Scrobbler` sends on a worker thread).
* `discord` — Discord Rich Presence (feature `discord`, default on).
* `fetch_lyrics` — LRCLIB synced lyrics.

Media controls, scrobbling and Discord all hang off the poll the UI already does:

```rust
// once, on the UI thread after the event loop/main window exists
// (macOS: main thread; Windows: set `config.hwnd` to the main window's HWND)
let mut controls = MediaControls::for_player(
    MediaControlsConfig::new("music_macos", "Music"),
    player.clone(),
    |event| { /* MediaControlEvent::Next / Previous → UI queue */ },
)?;
let mut scrobbler = Scrobbler::new(vec![
    Box::new(LastFm::new(api_key, api_secret).with_session(session_key)),
    Box::new(ListenBrainz::new(user_token)),
]);
let mut discord = DiscordPresence::new(DiscordConfig::new(discord_app_id));

// every ~250 ms tick
let state = player.state();
controls.update(&state);   // only talks to the OS when something changed
scrobbler.update(&state);  // never blocks; requests run on a worker thread
discord.update(&state);
```

Credentials are always supplied at runtime; `examples/scrobble.rs` runs the Last.fm
auth flow (`get_token` → `auth_url` → `get_session`). Threading details are in the
`media_controls` module docs. Ported code is listed in
[`crates/music-core/THIRD_PARTY.md`](crates/music-core/THIRD_PARTY.md) (Apache-2.0).

```sh
cargo run --release -p music-core --example resolve -- "daft punk"  # headless: search → resolve → download → seek to 30 s → decode
cargo run --release -p music-core --example cli -- "daft punk"      # plays ~8 s through the speakers (seeks, Bass EQ)
```

### Known limitation: previews only (PoToken)

Without a PoToken, googlevideo serves only the first 1 MiB of a stream and
answers 403 afterwards, so every spike plays roughly the first minute of a
track (`PlaybackState::preview_only` is set). Full playback needs a PoToken
provider behind `StreamResolver` — ArchiveTune does this with YouTube's
BotGuard challenge in a hidden WebView. That decision is deliberately left open.

## Spike spec (identical in every UI branch)

1. Window ~1100×720, dark theme.
2. Search field → `InnerTube::search_songs` on a background thread.
3. Virtualized results list: artwork (`Track::artwork_url(120)`), title,
   artists · album, duration. Click a row to play it.
4. Now-playing bar: artwork, title/artists, play/pause, progress bar with
   elapsed/total time and click-to-seek, "preview" badge when `preview_only`.
5. UI polls `Player::state()` every ~250 ms; no other cross-thread plumbing.

## How to compare

Build each branch with `cargo build --release` and record:

* Release binary size (and bundled app size where applicable).
* Idle RSS after launch, RSS with 20 results and artwork loaded, RSS while playing
  (`ps -o rss= -p <pid>` or Activity Monitor "Memory").
* CPU while playing with the window visible vs. hidden.
* Cold start to first frame, clean build time, scroll smoothness.

## music-gpui (GPUI + gpui-component)

The GPUI client (`crates/music-gpui`), picked over Slint after the benchmark rounds below.

```sh
cargo run --release -p music-gpui
# no audio device / sandbox: fake playback from 20 s that "ends" at 80 s like a preview
MUSIC_DEMO_CLOCK=1 cargo run --release -p music-gpui
# dev-only offscreen screenshots (GPUI test-support; separate, slower build)
MUSIC_QUERY="daft punk" MUSIC_GPUI_SCREENSHOT="9:/tmp/search.png,9.5:@settings=eq,11:/tmp/eq.png" \
  cargo run --release -p music-gpui --features screenshot
```

| ![Home](docs/screenshots/shell-home.png) | ![Search](docs/screenshots/shell-search.png) |
|---|---|
| ![Queue panel](docs/screenshots/shell-queue.png) | ![Settings](docs/screenshots/shell-settings.png) |
| ![Dynamic accent](docs/screenshots/shell-dynamic-accent.png) | ![Pure black](docs/screenshots/shell-pure-black.png) |
| ![Equalizer section](docs/screenshots/shell-settings-eq.png) | ![Mini player](docs/screenshots/shell-mini-player.png) |

### Structure

```
src/
  main.rs            app init, window (restored frame), mod list
  app.rs             MusicApp: root layout (sidebar | header + page | queue panel, bar,
                     Now Playing overlay), navigation, search field, settings/theme glue,
                     update_app / read_app (how pages + components reach the app)
  playback.rs        play / queue edits / transport / volume / 250 ms poll / media keys
  actions.rs         shortcuts + macOS menu bar (global action handlers)
  router.rs          Route enum + Router (back/forward history, per-entry scroll offsets)
  pages/             one file per page; mod.rs: PageView, NAV (sidebar), build()
  components/        track_row (hover actions + context menu), card, shelf/grid/section
                     header/empty state (mod.rs), artwork (shared cache), sidebar,
                     player_bar, queue_panel
  settings/          file.rs (settings.json model), panel.rs (SettingsPanel entity: state,
                     persistence, secrets, section list), one file per section, mod.rs
                     (SectionId + SECTIONS registry), widgets.rs
  queue.rs           queue model (pure, tested)
  theme.rs           dark / pure black / dynamic accent, font scale, AppColors
  mini_player.rs     always-on-top mini player window
  tray.rs            menu-bar item (macOS)
  now_playing.rs, karaoke.rs, anim.rs, backdrop.rs, clock.rs   Now Playing + lyrics
  services.rs, secrets.rs, script.rs, util.rs, icons.rs
```

### Extension points

* **Add a page:** add a `Route` variant (`router.rs`: `title`, `parse`), create
  `pages/<name>.rs` with a struct implementing `Render` and a `build(..) -> PageView`
  (pass the page's main `ScrollHandle` so back/forward restores the scroll position),
  add one arm to `pages::build`, and optionally one `NavItem` line to `pages::NAV`.
  Pages call into the app with `update_app(cx, |app, cx| app.play_list(..))`,
  `app.navigate(route, window, cx)`, `app.queue_add(..)`, and read shared state with
  `read_app(cx, |app, _| ..)`. Build UI from `components::{TrackRow, Card, shelf, grid,
  section_header, empty_state, page_scroll}`; images go through `components::artwork`.
* **Add a settings section:** add a `SectionId` variant (+ `key`), create
  `settings/<name>.rs` with `pub(super) fn render(panel, window, cx) -> AnyElement`, and
  register it in `settings::SECTIONS`. Persisted values go into `settings/file.rs`
  (every field has a default); change them through a `SettingsPanel` method that calls
  `changed()` and emits a `SettingsEvent` if the app must react.
* **Hooks waiting for core features:** `MusicApp::start_radio`, `add_to_playlist`,
  `toggle_like` (context menu entries, currently a notification), the stub pages
  (Home feed, Explore, Charts, Moods, New releases, Library tabs, Artist / Album /
  Playlist, Stats, Downloads, Local files) and the placeholder settings sections
  (Playback extras, Lyrics, Content & region, Network, Privacy, Storage, Account).
  `Track::artist_refs` / `album_id` already drive "Go to artist / album".

### Features

* **Shell:** sidebar (Home, Explore, Charts, Moods & genres, New releases, Library,
  History, Stats, Downloads, Local files, Settings), header with back/forward and the
  search field, routed content, player bar, queue panel, Now Playing overlay. Window
  title follows the page. Back/forward keep up to 64 entries; the 8 nearest keep
  their page alive (state + scroll), older ones are rebuilt and get their scroll offset
  back. Typing in the search field replaces the current search entry (no history spam).
* **Search** page: debounced, `InnerTube::search_songs`, virtualized rows with artwork.
  Rows: click plays the list from that row; hover shows play next / add to queue / more;
  right click (or "…") opens play, play next, add to queue, start radio, add to playlist,
  like, go to artist (one entry per artist with an id), go to album (disabled without ids).
* **Queue** (`queue.rs`): play next, add to queue, remove, move (up/down buttons or drag
  and drop in the panel), clear, shuffle (current track first; turning it off restores
  the original order including tracks added meanwhile), repeat off / all / one (repeat-one
  replays on end, "next" still skips), history of played tracks. Shuffle and repeat are
  saved. "Previous" restarts past 3 s; the end of a track (including a ~60 s preview)
  plays the next one. Queue panel: now playing, up next, history (click to play again).
* **Home** shows this session's tracks and up next as shelves until the home feed exists;
  **History** lists this session's plays.
* **Settings page** with sections: Appearance (theme, text size), Playback (shuffle,
  repeat + planned options), Equalizer, Lyrics, Content & region, Network, Privacy,
  Storage (file locations), Account (YouTube sign-in, planned), Scrobbling & Discord,
  About (version, GPL-3.0 notice, ArchiveTune and Hummingbird credits). The old sheet
  is gone; the Now Playing gear opens the page.
* **Themes** (`theme.rs`): dark (gpui-component's), pure black, dynamic (accent = the most
  common vibrant hue of the current artwork, lightened to ≥ 4.5:1 against the
  background, black or white text on it; grey art keeps the previous accent), text size
  88–138 %. Applied to gpui-component's `Theme` and our `AppColors`.
* **Artwork cache** (`components/artwork.rs`): one cache for every view and both windows,
  48 MiB LRU of encoded bytes (evicting also drops GPUI's decoded copy), 4 dedicated
  fetch threads taking the newest request first, failed URLs retried after 30 s. Now
  Playing's 544 px art comes from it too (blur + accent for the last 8 tracks).
* **Mini player** (⇧⌘M, View menu, bar button): floating 400×124 window, artwork, title,
  previous / play / next, progress; stays visible over other apps (macOS panel with
  `hidesOnDeactivate` off); position saved.
* **Menu-bar item** (macOS, `tray-icon`): current track, play/pause, next, previous,
  mini player, show window, quit. It coexists with GPUI's run loop (the status item is
  just AppKit; menu events are drained by the poll). Not built on Linux/Windows.
* **Window frame** saved (content size, position, maximized) and restored if on screen.
* **Now Playing**, **OS media controls**, **Equalizer**, **Scrobbling & Discord**,
  **volume**: unchanged from the previous round (details in the history below).

### Keyboard shortcuts and menus

| Keys | Action |
|---|---|
| Space | play / pause (not while typing) |
| ⌘→ / ⌘← | next / previous (restart past 3 s) |
| ⌘↑ / ⌘↓ | volume ±5 % |
| ⌘F | search field |
| ⌘, | settings |
| ⌘L | Now Playing |
| ⌥⌘Q | queue panel |
| ⇧⌘M | mini player |
| ⌘S / ⌘R | shuffle / cycle repeat |
| ⌘[ / ⌘] (also mouse back/forward buttons) | back / forward |
| ⌘1 / ⌘2 / ⌘3 / ⌘Y | Home / Explore / Library / History |
| Esc | close Now Playing, else the queue panel, else back |
| ⌘H, ⌥⌘H, ⌘M, ⌘Q | hide, hide others, minimize, quit |

The menu bar (Music, Edit, View, Controls, Window) carries the same actions with their
shortcuts. All shell actions are global listeners that defer to the main window's app,
so keys, menus, the mini player and scripts share one path. Text inputs keep their own
bindings (Space, ⌘←/→, ⌘↑/↓ edit text while the search field is focused).

### Settings file and secrets

`settings.json` (volume, EQ, Last.fm API key + user name, ListenBrainz user name, Discord
switch + id, theme + text size, shuffle + repeat, main window and mini player frames) lives in the platform config dir — macOS
`~/Library/Application Support/music-macos/`, Linux `~/.config/music-macos/`, Windows
`%APPDATA%\music-macos\config\` — or `$MUSIC_CONFIG_DIR`. It is written 500 ms after the last
change and on quit, atomically (temp file + rename), mode 0600 in a 0700 directory. Missing
fields fall back to defaults, unknown ones are ignored, out-of-range values are clamped, and a
file that isn't valid JSON is moved to `settings.json.bad`; an unknown theme / repeat value
(from a newer version) falls back to the default instead.

The Last.fm shared secret and session key and the ListenBrainz token go to the OS keychain
(`keyring` 4: macOS Keychain, Windows Credential Manager, Secret Service), service
`music-macos`, one entry each. If the keychain fails (or `MUSIC_SECRET_STORE=file`), they are
stored in the `secrets` object of `settings.json` (still 0600) and moved back into the keychain
the next time it accepts a write; the Storage and Scrobbling sections show where secrets are kept. The keychain
is only read for accounts the settings say exist, on a worker thread (macOS may show an
"allow access" prompt — for unsigned dev builds it does so after every rebuild, because the
item's ACL is tied to the binary). Secrets are never logged.

### Environment hooks

| Variable | Effect |
|---|---|
| `MUSIC_QUERY=…` | pre-fill and run a search (`MUSIC_GPUI_QUERY` still works) |
| `MUSIC_AUTOOPEN=1` | play the first hit and open Now Playing (`MUSIC_GPUI_AUTOPLAY=1`: play only) |
| `MUSIC_ROUTE=library:albums` | open a page at startup (`home`, `explore`, `search:q`, `settings:eq`, `artist:ID`, …) |
| `MUSIC_OPEN_SETTINGS=1\|<section>` | open the settings page (`eq`, `integrations`/`accounts`, `appearance`, `about`, …) |
| `MUSIC_DEMO_CLOCK=1` | demo clock instead of audio (also used automatically when `Player::new` fails) |
| `MUSIC_VOLUME=0.0` | volume for this session (muted real-device tests; ⌘↑/↓ and `@volume` still save) |
| `MUSIC_EQ_PRESET=Bass` | apply an EQ preset at startup (saved like a click) |
| `MUSIC_CONFIG_DIR=…` / `MUSIC_SECRET_STORE=file` | other settings dir / skip the keychain |
| `MUSIC_TRAY=0` | no menu-bar item |
| `MUSIC_TRACE=1` | log playback transitions, seeks, saves, route changes, artwork downloads; `MUSIC_TIMINGS=1`: renders/s + worst frame gap |
| `MUSIC_SCRIPT="5:@next,9:@seek=75,30:@quit"` | timed actions (any build) |
| `MUSIC_GPUI_SCREENSHOT="12:a.png,13:@settings,14:b.png,15:mini:m.png"` | offscreen shots + actions, quits after the last (feature `screenshot`; `mini:` shoots the mini player) |

Actions (`script.rs` has the table): input `@key=cmd-right` (a real keystroke through
GPUI's key dispatch), `@click=X;Y @rclick=X;Y @move=X;Y @mouse-back @mouse-forward`
(synthetic mouse events at window coordinates: hit-testing, hover, context menus);
playback `@play=N @next @prev @seek=S @toggle @open @close @volume=V`; navigation
`@nav=ROUTE @back @forward @search=Q`; queue `@play-next=N @enqueue=N @queue[=on|off]
@queue-move=A:B @queue-remove=N @queue-jump=N @queue-clear @shuffle=on|off
@repeat=off|all|one`; windows/appearance `@mini[=on|off] @theme=dark|black|dynamic
@font-scale=1.125 @resize=W;H`; settings `@settings[=section] @close-settings
@eq=Preset|on|off @field=lastfm-key|lastfm-secret|lb-token|discord-id:VALUE
@lastfm-connect @lastfm-finish @lastfm-disconnect @lastfm-clear @lb-validate
@lb-disconnect @discord=on|off`; `@quit`.

Screenshot builds draw both windows every 200 ms and right before each shot: while the
screen is locked macOS delivers no display-link frames, so nothing would redraw (and
lazily started artwork downloads would never begin).

### Measurements (app shell round)

macOS arm64, release (thin LTO), demo clock unless noted, sampled 10 s after launch
(`vmmap --summary` physical footprint = Activity Monitor "Memory", plus `ps` RSS);
"before" is the previous commit built from the same tree state, measured back to back:

| Scenario | Footprint before → after | RSS before → after |
|---|---|---|
| Idle, Home, nothing playing | 43.0 → 47.2 MiB (46.6 with `MUSIC_TRAY=0`) | 76 → 103 MiB |
| Search results + playing, bar updating | 50.2 → 54.3 MiB | 87 → 114 MiB |
| Same with real audio (muted) | — | 93 → 120 MiB |

The private memory cost of the shell is ~4 MiB (menu-bar item ~0.6 MiB); the larger RSS
step is mostly shared, clean pages (more of the 23.8 MiB binary, AppKit menus / status
item). Release binary 24,952,976 B (23.8 MiB, +1.2 MiB). Incremental release rebuild of
the crate: ~1.5 min; the `screenshot` build is separate (~3 min incremental).

**Not measured this round: CPU.** The machine's screen was locked for the whole session
(`CGSSessionScreenIsLocked`), so macOS delivered no display-link frames: `MUSIC_TIMINGS`
printed no renders, and every scenario sampled 0.0 % CPU (with audio too, i.e. the
process was throttled). Animation CPU therefore can't be compared to the previous
17–20 % from here. The Now Playing render path is unchanged (the shell isn't built while
the screen covers the window, frames are only requested while something moves), and the
browse UI renders only on the 250 ms poll or input; re-run the scenarios below with an
unlocked screen to confirm.

**Verified (synthetic input through GPUI's normal dispatch, `@key` / `@click` / `@rclick`):**
Space play/pause (and Space typed into the focused search field is text, not a toggle),
⌘→/⌘← next/previous-restart, ⌘↑/⌘↓ volume, ⌘L/Esc Now Playing, ⌘[ back, mouse back /
forward buttons, ⌥⌘Q queue panel, ⇧⌘M mini player open/close (repeatedly), ⌘1 Home,
⌘F + typing → search page, ⌘S/⌘R saved to settings, sidebar click, track-row click plays,
right click opens the context menu (per-artist "Go to …" entries). Queue move/remove/
clear, shuffle off restoring the order, repeat persisted, window frame restored without
drift. **Real device (muted):** search → autoplay, EQ preset changes while playing, seek
0→30→0 s, previous restarts, Discord switch + id saved, Last.fm key/secret flow, preview
end (`Ended` at 59.9 s) → next track loads and plays.

**Not verified:** physical mouse/keyboard and drag-and-drop in the queue panel (no
synthetic drag), clicking the menu-bar item (it is created; its commands go through the
same methods as the bar), the macOS menu bar itself (same actions as the shortcuts),
⌘, via script (a comma can't be written in `MUSIC_SCRIPT`), the mini player staying above
other apps (`setHidesOnDeactivate:` is called; not observable with a locked screen),
Linux/Windows.

### Measurements (previous round: queue, media controls, EQ, scrobbling)

macOS arm64, 60 Hz, release (thin LTO), `ps -o %cpu=,rss=` once a second for 20 s after an 8 s
warm-up, `MUSIC_QUERY="daft punk instant crush"`:

| Scenario | CPU % min / avg / max | RSS MiB min / avg / max |
|---|---|---|
| Now Playing animating, demo clock (`MUSIC_AUTOOPEN=1 MUSIC_DEMO_CLOCK=1`) | 11.2 / 17.7 / 23.1 | 110.9 / 112.4 / 113.4 |
| same, second run | 11.1 / 19.0 / 23.2 | 110.5 / 111.4 / 112.2 |
| Now Playing animating, **real audio, muted** (`MUSIC_VOLUME=0.0`) | 14.6 / 19.8 / 22.9 | 115.4 / 115.5 / 115.5 |
| Browse UI, real audio playing (muted), bar updating | 0.7 / 2.0 / 3.4 | 111.2 |
| Idle, nothing playing, window focused (caret blinking) | 0.2 / 0.7 / 1.5 | 87.5 / 87.6 / 87.6 |

61–62 renders/s with a worst frame gap of 17.7 ms while animating — unchanged from the lyrics
round (17.3 % / 16.8 % CPU then). RSS while playing is ~6–10 MiB higher than that round's
~102–106 MiB (media controls with artwork, scrobble/Discord/settings code); idle is lower
(88 vs 99 MiB). Release binary 23,661,248 B (22.6 MiB, +0.8 MiB). Incremental release rebuild
after touching the crate: 32–42 s here. The dependency graph grew by 17 crates (keyring + its
stores, `directories`).

**Real-device check** (`Player::new` works on this machine; always `MUSIC_VOLUME=0.0`, nothing
audible): position advances, seek 0→30 s and back works, rapid EQ changes (Bass → Treble → off
→ Loudness) during playback cause no errors, "previous" at 5 s restarts, and when a ~60 s
preview reaches its end (`Ended` at 59.9 s) the next result loads and plays. Seeking *past* a
preview's downloaded data snaps back to ~0 s (music-core decoder behaviour; not handled in the UI).

**Not verified:** real mouse/keyboard input (all interaction went through the script actions,
which call the same handlers as the buttons), OS media keys / Control Center (code path compiles
and registers; no key events could be injected), a complete Last.fm auth or actual scrobbles /
ListenBrainz listens (only the error paths with bogus credentials hit the real APIs), Discord
Rich Presence against a running Discord, Linux and Windows (including the HWND and keychain
backends).

### History: benchmark rounds (condensed)

**Round 1 — search spike.** Search, virtual list, artwork, bar with progress slider, built from
gpui-component almost without custom drawing (one ~800-line file, two trivial compile errors).
Crates: `gpui-component =0.8.1`, `gpui-kit-assets =0.8.1` and the GPUI snapshot it pins,
`gpui-pre =0.3.10` + `gpui-pre-platform =0.3.10` (aliased `gpui` / `gpui_platform`; features
`font-kit` — needed for glyphs on macOS —, `runtime_shaders` — no full Xcode —, `wayland`,
`x11`). Measured: 20.9 MiB binary, ≈10–11 min clean build (427 crates), idle RSS ≈88 MiB,
≈102 MiB with 20 results, 0.3–1 % CPU idle. Hard: confusing crate topology (the platform crate
and `runtime_shaders` only show up in `gpui-base`'s manifest), no examples in the tarballs,
build cost, `Slider` exposes no dragging state, `render_to_image` needs `test-support`.

**Round 2 — Now Playing & lyrics.** Slide-up screen with blurred backdrop (CPU blur, 4.5–14 ms
per track, cross-fade), artwork scaled with GPUI's `with_spring`, play/pause icon morph via
`svg().with_transformation`, LRCLIB lyrics as a custom `KaraokeLine` element (`shape_text` +
`paint_glyph` + `with_content_mask`: partial-word sweep with a 10-slice soft edge that survives
wrapping, sung words lift, inactive lines painted at 0.9 scale), Apple-Music-style staggered
scrolling from `ScrollHandle` child bounds, `SmoothClock` extrapolating the 250 ms polls, frames
requested only while something moves, `inactive_frame_interval: None` to avoid GPUI's 30 fps
throttle for unfocused windows. Measured 17.3 / 16.8 % avg CPU at 61.6 renders/s (worst gap
17.8 ms), ~102–106 MiB, 0–1 % idle. Learned: blocking work on GPUI's background executor
starves (artwork downloads delayed LRCLIB by >15 s → dedicated threads); value tweens readable
from state need a helper (`anim.rs`). Missing for an ArchiveTune-grade port: transforms on any
subtree, layer opacity, blur/backdrop filters, alpha/gradient masks, gradient text, shared
element transitions, a pausable animation clock, a documented non-blocking HTTP/image pipeline.
Screenshots: [karaoke sweep](docs/screenshots/gpui-karaoke-sweep-zoom.png),
[backdrop](docs/screenshots/gpui-now-playing-backdrop.png),
[open transition](docs/screenshots/gpui-now-playing-transition.png),
[pause/close](docs/screenshots/gpui-now-playing-pause-close.png).

**Round 3 — this one** (queue, media controls, EQ, scrobbling, Discord, persistence, volume).
Easy: `Sheet`, `Switch`, vertical `Slider`, masked `Input` with a reveal toggle and `Button`
selected states covered the settings UI; `Window` implements `raw-window-handle`; extra Lucide
icons can be embedded one by one with `icon_assets!`. Gotchas: `InputState::set_value` doesn't
emit `InputEvent::Change` (programmatic edits must be synced by hand), a vertical `Slider` must be
sized itself (`.h_full()` inside a sized box), not just its parent, and gpui-component's sheet leaves a title-bar gap
unless `Theme::sheet.margin_top` is zeroed. No changes to `music-core`.
