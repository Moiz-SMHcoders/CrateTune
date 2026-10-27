# Vendored: Hummingbird

This directory is a **modified** copy of [Hummingbird](https://git.mailliw.org/hummingbird/hummingbird)
(GitHub mirror: <https://github.com/hummingbird-player/hummingbird>), a native music player
written in Rust + GPUI, licensed under Apache-2.0 (see `LICENSE`).

| | |
|---|---|
| Upstream | https://git.mailliw.org/hummingbird/hummingbird.git (`main`) |
| Commit | `e15e8ba0ef6ff5445294030d373e3c8ccc7d780c` ("fix: remember scroll positions in navigation history (#461)") |
| Commit date | 2026-10-20 |
| Vendored on | 2026-10-27 (plain files via `git archive`, upstream `.git` dropped) |

## Local modifications

Build/vendoring changes:

* `Cargo.toml`: empty `[workspace]` so this stays its own Cargo workspace (it needs its own
  `[patch.crates-io]` and profiles); the music-macos root `Cargo.toml` excludes `apps/hummingbird`.
* `Cargo.toml`: `runtime_shaders` feature on `gpui_platform`, so it builds with only the Xcode
  Command Line Tools (no `xcrun metal`).
* `rust-toolchain.toml`: upstream pins `stable`; pinned to `1.99` (same as the music-macos
  workspace) because the locally installed `stable` was too old.
* `.cargo/config.toml`: sets `HUMMINGBIRD_VERSION_ID`, because `build.rs` runs `git rev-parse`
  for dev-channel builds and there is no `.git` here.

YouTube Music integration (all other changes) is described in the root `README.md`
("Hummingbird fork spike notes"). New files are `src/youtube.rs` and
`src/ui/library/youtube_view.rs`; edits to upstream files are kept small and marked where useful.
