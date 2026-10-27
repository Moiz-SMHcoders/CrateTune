# Third-party code in music-core

Parts of music-core are ported from **Hummingbird**, a native Rust music player
(<https://git.mailliw.org/hummingbird/hummingbird>, GitHub mirror
<https://github.com/hummingbird-player/hummingbird>), licensed under the
Apache License 2.0. Upstream commit: `e15e8ba0ef6ff5445294030d373e3c8ccc7d780c`.

Copyright Hummingbird contributors, Apache-2.0. All ported files are modified;
each carries a header naming its upstream source. A copy of the license is at
<https://www.apache.org/licenses/LICENSE-2.0>.

| music-core file | Upstream file(s) | Changes |
|---|---|---|
| `src/equalizer.rs` | `src/playback/dsp/equalizer.rs`, `src/settings/equalizer.rs` | Settings types merged in without serde; presets + 10-band graphic layout; interleaved-`f32` live stage (`EqControl`/`EqStage`) for the decoder; deterministic test noise instead of `rand`. DSP unchanged. |
| `src/media_controls/mod.rs` | `src/services/controllers.rs` | GPUI/tokio/async-trait removed; one `Backend` trait driven by `PlaybackState` snapshots (`MediaControls::update` diffs and only publishes changes); events go to a callback, with a `Player` router. |
| `src/media_controls/macos.rs` | `src/services/controllers/macos.rs` | Callback instead of the playback bridge; playback rate published; artwork from URL (fetched on a thread) or bytes without leaking the `NSImage`; stop command; handlers removed on drop. |
| `src/media_controls/windows.rs` | `src/services/controllers/windows.rs` | Callback; blocking `join()` instead of `.await`; artwork from URL; no shuffle/repeat; error instead of panic without an `HWND`. |
| `src/media_controls/mpris.rs` | `src/services/controllers/mpris.rs` | No tokio (zbus' own executor + `block_on`); std `Mutex`; callback; track ids; artwork URLs passed through; no shuffle/repeat; configurable bus name/identity. |
| `src/scrobble/mod.rs` | `src/services/mmb.rs`, play-time bookkeeping in `src/services/mmb/{lastfm,listenbrainz}.rs` | Async trait replaced by a pure `ScrobbleTracker` over `PlaybackState` snapshots (wall-clock-bounded play time, replays, now-playing on resume) and a `Scrobbler` worker thread. |
| `src/scrobble/lastfm.rs` | `src/services/mmb/lastfm.rs`, `lastfm/client.rs`, `lastfm/types.rs` | Blocking `ureq`; `serde_json::Value` instead of serde derives; API errors surfaced; runtime credentials instead of env vars; documented auth flow. |
| `src/scrobble/listenbrainz.rs` | `src/services/mmb/listenbrainz.rs`, `listenbrainz/client.rs`, `listenbrainz/types.rs` | Blocking `ureq`; `json!` payloads; YouTube Music `origin_url`/`music_service`; configurable client name and API root. |
| `src/discord.rs` | `src/services/mmb/discord.rs` | Snapshot-driven `PresenceTracker`; IPC on a worker thread with update coalescing (≤1 per 4 s) instead of async + `watch`; caller-supplied application id/name; YouTube Music artwork URL. |

## ArchiveTune (feat/playback)

Playback-engine behaviour is ported from **ArchiveTune**
(<https://github.com/rukamori/ArchiveTune>, commit `c7f0e243c637e039bce4b92fecdf7179804114fb`), © Rukamori and ArchiveTune contributors,
GPL-3.0. Files carry a header naming their upstream source.

| music-core file | Upstream file(s) | What was ported |
|---|---|---|
| `src/sleep.rs` | `playback/SleepTimer.kt` | Sleep timer model: a duration timer or "pause when the song ends", re-arming replaces the previous mode, clearing after it fires; driven by engine ticks instead of coroutines. |
| `src/deck.rs` | `playback/CrossfadeHandoffPolicy.kt`, `playback/MusicService.kt` (`scheduleCrossfade`, `effectiveCrossfadeDuration`, `applyCrossfadeVolumes`) | Equal-power crossfade gains (cos/sin quarter circle); crossfade timing (minimum 500 ms, 150 ms end guard, shortened when started late); mixed sample-accurately in one source instead of two ExoPlayers. |
| `src/player.rs` | `playback/MusicService.kt` (`skipOnError`, `CROSSFADE_PREPARE_AHEAD_MS`), `playback/preload/NextStreamPreloader.kt` | Auto-skip to the next item on load errors with a cap on consecutive skips; preparing the next stream ahead of the end (30 s) and replacing the preload target when the queue changes. |

Paths are relative to `app/src/main/kotlin/moe/rukamori/archivetune/`. All Kotlin/Android
specifics (Media3, coroutines, DataStore) are replaced by the blocking engine thread.

Libraries used by the engine (not ported code): `opus-decoder` (MIT OR Apache-2.0,
pure-Rust Opus), `libsonic` (Apache-2.0, Rust port of Bill Cox's Sonic, used for speed/pitch).
