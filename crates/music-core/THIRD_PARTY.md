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

## ArchiveTune (feat/library)

Parts of music-core are ported from **ArchiveTune**, an Android YouTube Music client
(© Rukamori and ArchiveTune contributors), licensed under the GNU General Public License
v3.0 — the reason this project is GPL-3.0. Reference checkout: `archivetune-ref`
(`app/src/main/kotlin/moe/rukamori/archivetune/…`, Room schemas in `app/schemas/`).
Each ported file carries a header naming its upstream source; all were rewritten from
Kotlin/Room/Android APIs into blocking Rust over SQLite (`rusqlite`) and `lofty`.

| music-core file | Upstream file(s) | Changes |
|---|---|---|
| `src/library/schema.rs` | `db/entities/*.kt`, `app/schemas/` | Condensed table set (song, artist, album, maps, playlist + entries with `setVideoId`, play events, search history, lyrics, key/value, local files) as one `user_version` migration; no formats/podcasts/top-mix/aliases tables. |
| `src/library/mod.rs` | `db/MusicDatabase.kt`, `db/LocalMusicIdentity.kt` | Single mutex-guarded connection, `user_version` migrations, name normalization without NFKC, key/value store, saved queue. |
| `src/library/songs.rs` | `db/DatabaseDao.kt`, `db/entities/{SongEntity,ArtistEntity,AlbumEntity}.kt` | Like/library semantics of `toggleLike`/`toggleLibrary`; upserts from `Track`; sorted/filtered listings and local search as dynamic SQL; synthetic `LA:`/`LB:` ids for name-only artists/albums. |
| `src/library/playlists.rs` | `db/entities/{PlaylistEntity,PlaylistSongMap}.kt`, `db/DatabaseDao.kt` | Local + mirrored playlists, contiguous positions with insert/move/remove. |
| `src/library/history.rs` | `db/DatabaseDao.kt`, `db/entities/{Event,SearchHistory,LyricsEntity}.kt` | Play events with per-song aggregates recomputed on delete; day grouping in SQL with a caller-chosen time zone. |
| `src/library/stats.rs` | `db/DatabaseDao.kt` (`mostPlayed*`, `listeningBy*`, `listeningTotals`), `viewmodels/YearInMusicViewModel.kt`, `ui/screens/{StatsScreen,YearInMusicScreen}.kt` | Half-open ranges, explicit `TimeZone` (local or fixed offset), month/day breakdowns, top hour/busiest day; no artist blocking. |
| `src/library/backup.rs` | `backup/BackupArchiveRepository.kt` | SQLite online backup API instead of copying DB/WAL files; JSON manifest + caller's settings JSON instead of preferences XML; validated restore into the live connection. |
| `src/local_files.rs` | `localmedia/{LocalSongScanner,LocalAudioMetadataReader,SupportedLocalAudio}.kt` | Directory walk instead of MediaStore; `lofty` instead of `MediaMetadataRetriever`; artwork deduplicated by content hash (not resized); ReplayGain + embedded lyrics; mtime/size incremental rescan. |

`src/downloads.rs` follows the split between user downloads and the player cache in
ArchiveTune's `playback/DownloadUtil.kt` but contains no ported code.
