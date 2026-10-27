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
