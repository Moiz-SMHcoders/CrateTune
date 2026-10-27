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

## ArchiveTune (feat/innertube)

The InnerTube (YouTube Music) layer is ported from **ArchiveTune**
(<https://github.com/rukamori/ArchiveTune>, Kotlin), © Rukamori and ArchiveTune
contributors, licensed under GPL-3.0 (this project is GPL-3.0 as well). Logic, endpoint
names, request bodies and renderer paths were ported; the code is a rewrite over
`serde_json::Value` with blocking `ureq` instead of Ktor/kotlinx-serialization. Each
file carries a header naming its upstream source.

| music-core file | Upstream file(s) (`core/src/main/kotlin/moe/rukamori/archivetune/innertube/`) | Changes |
|---|---|---|
| `src/innertube/mod.rs` | `YouTube.kt` | Blocking methods on a cloneable `InnerTube` config; results are UI-agnostic serde models; charts take a country via `formData`; no automix auto-follow (exposed as `WatchQueue::automix`). |
| `src/innertube/client.rs` | `InnerTube.kt` (`ytClient`, request bodies), `models/Context.kt`, `models/YouTubeClient.kt` | WEB_REMIX only; anonymous `visitorData` remembered per process so continuations work; no retries, PoTokens or player clients. |
| `src/innertube/parse.rs` | `pages/PageHelper.kt`, `pages/SearchPage.kt`, `pages/HomePage.kt`, `pages/RelatedPage.kt`, `pages/NextPage.kt`, `models/{Runs,Thumbnails,Continuation}.kt` | One generic item parser keyed on endpoint page/video types (not localized labels); subtitle runs split into artists/album/year/duration groups. |
| `src/innertube/pages.rs` | `YouTube.kt` (`home`, `explore`, `moodAndGenres`, `getChartsPage`, `album`, `artist`, `artistItems`, `playlist`, `search*`, `next`, `lyrics`, `related`), `pages/*Page.kt` | Generic `Shelf` model instead of per-page section types; chart countries decoded from the menu's entity keys. |
| `src/innertube/auth.rs` | `InnerTube.kt` (SAPISIDHASH header), `PlaybackAuthState.kt`, `utils/Utils.kt` (`sha1`, cookie parsing) | `Auth` struct with configurable `X-Goog-AuthUser`; secrets redacted from `Debug`. |
| `src/innertube/account.rs` | `YouTube.kt` / `InnerTube.kt` (account menu, accounts list, library, history, like, subscribe, playlist create/edit/delete), `pages/{LibraryPage,HistoryPage}.kt`, `models/body/*.kt`, `models/response/AccountMenuResponse.kt` | Library feedback tokens exposed per item; playlist edits as a `PlaylistAction` enum; no playlist cover upload. |
