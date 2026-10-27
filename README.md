# music-macos — desktop YouTube Music client spikes

Three UI spikes on top of one shared Rust core, to pick a UI framework by
measurement instead of by opinion. All three target macOS, Windows and Linux.

| Branch | UI stack | Idea |
|---|---|---|
| `main` | — | Shared core only (`crates/music-core`) |
| `ui/slint` | Slint 1.x | Stable, declarative, accessible |
| `ui/gpui-component` | GPUI (`gpui-pre`) + gpui-component | Zed's GPU UI + 60-widget kit |
| `ui/hummingbird` | Hummingbird (GPUI, `gpui-unofficial`) | Fork an existing native GPUI music player and add YouTube Music to it |

## Shared core (`crates/music-core`)

Blocking, thread-safe, no async runtime, so every UI toolkit can drive it from
its own executor.

* `InnerTube` — anonymous YouTube Music (`WEB_REMIX`) song search.
* `StreamResolver` trait + `IosClientResolver` — video id → audio URL (AAC itag 140).
* `Player` — audio thread (rodio → Symphonia + cpal) with a pollable
  `PlaybackState`, loudness normalization from YouTube's per-format `loudnessDb`.

```sh
cargo run --release -p music-core --example resolve -- "daft punk"  # headless: search → resolve → download → decode
cargo run --release -p music-core --example cli -- "daft punk"      # plays ~8 s through the speakers
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

## Slint spike notes

```sh
cargo run --release -p music-slint                  # empty window, type a query + Enter
cargo run --release -p music-slint -- daft punk     # searches on launch (for scripted RSS runs)
```

Code: `crates/music-slint/ui/app.slint` (all UI, compiled by `slint-build` in
`build.rs`, style `fluent-dark`) + `src/main.rs` (~290 lines of glue).

**Dependencies:** `slint 1.18.1` / `slint-build 1.18.1`, `image 0.25.10`
(`jpeg`, `png`, `webp` only).

**Renderer/backends:** `default-features = false` + `std`, `compat-1-2`,
`backend-winit`, `renderer-femtovg`, `accessibility`. No Qt backend, no software
renderer, no Skia, no system tray. FemtoVG is pure Rust on OpenGL (CGL on macOS:
deprecated by Apple but works), antialiased text/paths, and needs no native
toolchain beyond the CLT. Skia looks marginally better but adds roughly 10 MB of
prebuilt C++ (or a clang/ninja/python source build), and its RAM cost was not
measured here;
`renderer-femtovg-wgpu` would use Metal but pulls in wgpu. `accessibility`
(AccessKit) costs only about 0.4 MB (15.6 MB without vs 16.0 MB with), so it stays.

**Measured** (MacBook, macOS 26, Rust 1.99, `CARGO_BUILD_JOBS=4`, other builds
running in parallel, so timings are pessimistic):

| | |
|---|---|
| Release binary (stripped, thin LTO) | **16.0 MB** (core-only `cli` example: 3.8 MB) |
| Clean release build | ~10 min (~7.5 min deps + ~2.5 min for this crate) |
| Incremental release rebuild after a `.slint`/`.rs` edit | 1.5 to 2.5 min (thin LTO + generated code) |
| Idle RSS, 8 s after launch | 89 to 103 MB |
| RSS with 20 results + artwork | 96 to 115 MB |
| RSS while playing | not measured (sandbox has no audio device) |

The window was tiled by a window manager to 1470×922 instead of 1100×720, which
slightly inflates RSS (larger GL surface).

**What works:** search on a background thread (stale responses are dropped),
virtualized `ListView` with 44 px artwork, title, artists · album, and
duration, plus the current-track highlight. Artwork goes through a 4-thread
fetch+decode pool, then becomes a `SharedPixelBuffer` and is turned into a
`slint::Image` on the UI thread, cached per video id. The now-playing bar has
artwork, title/artists, a `PREVIEW` badge, play/pause with a loading spinner,
elapsed/total, and click-to-seek. A 250 ms `slint::Timer` polls `Player::state()`.
If `Player::new` fails, the error appears in the bar and search keeps working.
Verified visually via a window capture. Playback, seek and play/pause could not
be exercised (no audio device in the sandbox).

**Easy:** the declarative layout is fast to write and type-checked at build time.
`invoke_from_event_loop` + `Weak` make the threading straightforward.
`ListView` virtualizes out of the box. Dark std-widgets need one line
(`with_style("fluent-dark")`). Clippy was clean on the first try.

**Hard / limitations hit:**
* `slint::Image` is `!Send`, and so is any `Rc` you capture, so state touched from
  `invoke_from_event_loop` closures (track list, image cache) ends up in a
  UI-thread `thread_local!`. That works, but it is the least elegant part.
* `Image::load_from_data` exists (with `image-default-formats`), but it returns
  a `!Send` `Image`, so it would decode on the UI thread. The app decodes with
  `image` on worker threads into a `Send` `SharedPixelBuffer` instead.
* No built-in way to scroll a `ListView` to the top from Rust. The workaround is
  a fresh `ModelRc` per search plus `changed results => { list.content-y = 0; }`
  in `.slint` (`viewport-y` is deprecated in 1.18).
* The compiler rejects `a || b && c` without parentheses. That is a good error,
  but it's only reported at `build.rs` time, after a long dependency build.
* Builds are heavy: the release build of the generated code plus thin LTO
  dominates the edit/compile loop. Use a debug build, or `slint-viewer` with
  live preview, for UI iteration.
* No custom fonts or icons are bundled. The play/pause glyphs are `Path`s and
  the artwork placeholder is a "♪" from the system font.
