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

## Hummingbird fork spike notes

Branch `ui/hummingbird`: [Hummingbird](https://git.mailliw.org/hummingbird/hummingbird)
(Apache-2.0, Rust + GPUI via `gpui-unofficial`) vendored into `apps/hummingbird/` at upstream
`e15e8ba` (2026-10-20), plus a **YouTube Music** view. Details of the vendoring are in
`apps/hummingbird/VENDORED.md`.

### Run

```sh
cd apps/hummingbird            # its own Cargo workspace (excluded from the root one)
cargo build --release          # needs `cmake` on PATH (opusic-sys); Rust 1.99 via rust-toolchain.toml
./target/release/hummingbird   # sidebar → "YouTube Music"
DEVICE_PROVIDER=dummy ./target/release/hummingbird some.m4a   # no audio output, for headless checks
```

Builds with only the Xcode Command Line Tools thanks to GPUI's `runtime_shaders` feature.

### How YouTube tracks flow into Hummingbird's engine

1. `YouTubeView` (`src/ui/library/youtube_view.rs`, a new `ViewSwitchMessage::YouTube` in the
   library navigation + sidebar item) runs `music_core::InnerTube::search_songs` on
   Hummingbird's tokio blocking pool; thumbnails are fetched with `music_core::fetch_bytes`.
2. Click a row (or its "Add to Queue" button): `src/youtube.rs` resolves + downloads via
   `IosClientResolver`, and writes `<cache>/youtube/<videoId>.m4a` (or `<videoId>.preview.m4a`
   when the download was truncated; cache = `~/Library/Caches/org.mailliw.hummingbird` on macOS).
   Before writing, an incomplete download is trimmed to its last complete `moof`+`mdat` pair, so
   the file is valid fragmented MP4. Then lofty tags it: title, artist, album, 544 px cover art,
   a comment with the source URL, and `REPLAYGAIN_TRACK_GAIN = -4 dB − loudnessDb` (YouTube
   normalizes to ≈ −14 LUFS, ReplayGain 2 to −18 LUFS).
3. The file goes into Hummingbird's **own** queue as a plain `QueueItemData` (click = replace
   queue + play, like clicking a library track). From there it is just a local file: its
   Symphonia/fdk-aac decoder, now-playing bar + art, queue panel, EQ, ReplayGain, macOS Now
   Playing/media keys, Last.fm/ListenBrainz and Discord all apply. Re-clicking reuses the cache.
4. "Preview" badge (tooltip explains) on rows whose download was incomplete, "Loading…" while
   downloading, "Unavailable" + error tooltip on failure; the playing row is highlighted.

No decoder patch was needed: Hummingbird uses Symphonia **0.6**, whose MP4 reader opens the
truncated preview fine even when seekable (unlike rodio's Symphonia 0.5 in `music-core`), and
after trimming the partial fragment it is a well-formed file anyway (verified headlessly: tags
read back by lofty and Symphonia, 59.9 s decoded, engine plays to EOF with `DEVICE_PROVIDER=dummy`).
Caveat: the MP4 header still declares the full duration, so the progress bar shows e.g. 7:10
and the track ends ("EOF, moving to next song") after ≈60 s. music-core is unchanged.

### Measurements (Apple Silicon, 16 GB, `CARGO_BUILD_JOBS=4`, other builds running in parallel)

| | |
|---|---|
| Release binary (`opt-level="s"`, thin LTO, stripped) | 38.5 MB (upstream unmodified: 37.5 MB) |
| Clean release build | ≈10 min (605 s, load avg 20–38 from parallel builds) |
| Idle RSS after launch (empty library) | ≈110 MB at 8 s (20 MB at 2 s, 105 MB at 4 s) |
| `target/` size | 2.8 GB |

### Easy / hard

* Easy: Hummingbird's queue is path-based and media providers are pluggable, so "download to a
  tagged file + queue it" gets every engine feature for free with ~40 lines changed in upstream
  files (navigation enum, sidebar item, `cache_dir()`); the rest is two new files. Its
  `Textbox`, `LibraryViewHeader`, `nav_button`, theme tokens and `uniform_list` made the view
  look native quickly.
* Hard / friction: build environment, not code — `xcrun metal` missing (→ `runtime_shaders`),
  `cmake` needed by `opusic-sys` (used a throwaway pip-installed cmake), `build.rs` calls
  `git rev-parse` (→ `HUMMINGBIRD_VERSION_ID`), upstream pins `stable` (local stable 1.91 too old
  → pinned 1.99). `tr!` keys must be defined once (new strings land in `translations/en.json`
  automatically). lofty refused to tag the raw truncated MP4, hence the trim step.
* Not done: progressive streaming (whole ≤1 MiB preview is downloaded first), search-results
  queue context, cache eviction, preview badge in the now-playing bar itself; the `music-core`
  rodio `Player` is linked but unused (≈1 MB of the size delta, along with ureq/rustls).

### Licensing

Hummingbird is Apache-2.0: its `LICENSE` (and the icon licenses under `assets/icons/`) are kept
in `apps/hummingbird/`; upstream ships no NOTICE file. `VENDORED.md` records the upstream URL,
commit and lists every modified upstream file (Apache-2.0 §4(b) change notice).
