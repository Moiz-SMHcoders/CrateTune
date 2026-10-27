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

## GPUI + gpui-component spike notes

```sh
cargo run --release -p music-gpui
# measurement helpers: pre-fill + run a search, optionally auto-play the first hit
MUSIC_GPUI_QUERY="daft punk" MUSIC_GPUI_AUTOPLAY=1 cargo run --release -p music-gpui
# dev-only offscreen screenshot (enables GPUI test-support; separate, slower build)
MUSIC_GPUI_QUERY="daft punk" MUSIC_GPUI_SCREENSHOT=/tmp/shot.png \
  cargo run --release -p music-gpui --features screenshot
```

**Dependencies.** `gpui-component =0.8.1` (GPUI Kit's styled layer, on `gpui-base 0.8.1`),
`gpui-kit-assets =0.8.1` (Lucide icons; only the 101 default icons are embedded), and the
exact GPUI snapshot gpui-component pins: **`gpui-pre =0.3.10`** (aliased as `gpui`) plus
**`gpui-pre-platform =0.3.10`** (aliased as `gpui_platform`). This is *not* the official
`gpui` crate nor `gpui-unofficial`: `gpui-pre` is a published snapshot of Zed's GPUI in which
the core is platform-free and the native backends (`gpui-pre-macos`/`-apple`/`-linux`/
`-windows`) come via `gpui-pre-platform::application()`. Platform features used:
`font-kit` (without it macOS renders no glyphs), `runtime_shaders` (compiles Metal shaders at
startup, so no `xcrun metal`/full Xcode needed), `wayland` + `x11` (Linux only).

**What's used from gpui-component.** `Root` (window root), `Theme::change(ThemeMode::Dark)`,
`Input`/`InputState` (search with prefix icon + clear button; debounced on change, immediate
on Enter), `v_virtual_list` + `VirtualListScrollHandle` + `vertical_scrollbar` (results),
`Button` (primary, round, `loading` state), `Slider`/`SliderState` (progress; `Change` events
scrub the elapsed label, `Release` seeks), `Tag::warning` ("Preview" badge), `Spinner`,
`IconName`. Artwork: `music_core::fetch_bytes` on the background executor →
`gpui::Image::from_bytes` → `img()`, cached per video id (no HTTP client wired into GPUI).
Playback: a `spawn_in` loop polls `Player::state()` every 250 ms and calls `cx.notify()` only
when the snapshot changed; `Player::new` runs on the background executor and a failure is
shown in the now-playing bar while search keeps working.

**Measured** (macOS arm64, Rust 1.99, `CARGO_BUILD_JOBS=4`, other builds running in parallel):

| | |
|---|---|
| Release binary (stripped, thin LTO) | 20.9 MiB (21,870,832 B) |
| Clean release build | ≈ 10–11 min (≈ 6.7 min deps + ≈ 4 min for the final crate) |
| Rebuild after editing `main.rs` | 2.5–4 min (thin LTO + monomorphising GPUI generics) |
| `cargo clippy` from cold | ≈ 4 min |
| Idle RSS, 8 s after launch | 86–106 MiB (88 MB typical) |
| RSS with 20 results + artwork | ≈ 102 MiB |
| CPU idle / with results, window visible | 0.3–1 % |
| Crates in the macOS dependency graph | 427 (music-core alone: 77) |
| `target/` after release + clippy + screenshot builds | 3.0 GB |

RSS while playing and CPU while playing were **not measured**: the sandbox the spike was built
in cannot open the audio device ("Error opening the stream with the OS"), so the UI shows
"Audio unavailable: …" in the now-playing bar. Search, virtualized list, artwork and the
error path were verified from an offscreen screenshot; play/pause, the progress slider,
click/drag-to-seek and the Preview badge compile and are wired up but were not exercised
against real audio.

**Easy.** The component set covers this spec almost entirely (input with icon/clear, virtual
list, slider with change/release events, tags, loading buttons, spinner, scrollbars, a
coherent dark theme), so the app is one ~800-line file with almost no custom drawing.
GPUI's model — one entity, `cx.background_spawn` for blocking calls, `cx.notify()` — fits a
blocking, poll-based core well; replacing the stored `Task` cancels a stale search for free.
It compiled with only two trivial errors on the first try once the source was read.

**Hard / surprising.**
* Crate topology is confusing: gpui-component's README talks about the umbrella `gpui-kit`
  crate, the core `gpui-pre` has no platform code, and the platform crate (`gpui-pre-platform`)
  only appears as a dev-dependency of gpui-component. Without reading `gpui-base`'s
  `Cargo.toml` you won't find it (or the `runtime_shaders` feature, which lives there, not on
  `gpui-pre`).
* No examples ship in the crates.io tarballs; the API had to be read from source (some files
  are huge, e.g. `gpui-base`'s input state is ~12k lines).
* Build cost is the main downside: 427 crates, ~10 min clean, multi-minute incremental
  release rebuilds, a 21 MiB binary.
* `Slider` doesn't expose its dragging state, so the app tracks "scrubbing" itself to keep
  the 250 ms poll from yanking the thumb; resetting the max when the duration changes means
  rebuilding the `SliderState`.
* `Window::render_to_image` (handy for headless verification) is gated behind GPUI's
  `test-support` feature, so screenshots need a second build of the whole GPUI stack.

**Limitations.** Seeking inside a ~60 s preview depends on music-core (preview audio is decoded
unseekable, so seeks may be ignored). No volume control, queue, keyboard shortcuts or
artwork cache eviction. Only built and run on macOS; Linux/Windows backends are enabled in
`Cargo.toml` but untested. No changes to music-core.
