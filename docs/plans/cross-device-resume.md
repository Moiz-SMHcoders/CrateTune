# Plan: continue listening on another device

Goal: start a song on one device (CrateTune on a Mac, a Windows PC or Linux, or ArchiveTune
on a phone) and carry on from where you left off on another, like Spotify Connect.

Status: phase 1 (play reporting and the Continue card) is built (`feat/accounts-plus`:
`play_report.rs`, `continue_card.rs`), not yet verified against a real account. Phase 2 (Devices
over iroh) and phase 3 ("keep playing here") are built (`feat/device-handoff`:
`music_core::handoff`, `integrations/devices.rs`, `settings/devices_ui.rs`,
`together::sync::leave_plan`; relay deployment in `deploy/iroh-relay/`), verified with two app
instances on one Mac and in-process tests; across real networks and against a real ArchiveTune
host not yet (docs/BACKLOG.md). Phase 4 is a plan only.

As built (differences from the plan below): the relay is served by path on
`relay.smhcoders.click` (`/relay`, `/ping`, `/generate_204`) next to music-relay instead of a
new host name; pairing uses a SPAKE2 exchange keyed by the code (8 Crockford base32
characters), with the pairing link (device id, relay, addresses) for other networks; QUIC
address discovery is off (the relay sits behind Caddy); the launch offer lives in Home's
existing continue card; both apps must be running (no mailbox).

## What exists today

- **ArchiveTune** (Android) has no cross-device resume.
  - It keeps its queue in a local, Java-serialised file (`PersistQueue`).
  - The GMS build can back up to Google Drive.
  - It has Music Together: a JSON protocol over WebSockets, on the LAN or through its own
    hosted relay. We must not use that relay.
  - When signed in, it **reports plays to YouTube** (`registerPlayback`, the player
    response's `videostatsPlaybackUrl`), so its plays show up in the account's YouTube
    Music history.
- **CrateTune**
  - Restores its own queue and position on the same machine.
  - Has Music Together, using ArchiveTune's protocol (LAN and a self-hosted `music-relay`).
    It also supports host transfer.
  - Signs in to YouTube Music and reads the account's history.
  - Reports plays to YouTube when signed in ("Add plays to my YouTube history"), and shows a
    "Jump back in" card on Home for the newest history entry played elsewhere.

Only two things are shared with ArchiveTune without changing ArchiveTune:
- the **YouTube Music account**
- the **Music Together protocol**

Anything more exact, such as the position to the second or the whole queue, needs CrateTune
on both ends.

## Phase 1: account history (works with ArchiveTune, YouTube Music and the web). Size: medium.

1. **Report plays to YouTube when signed in**, as ArchiveTune does.
   - Ping the player response's `playbackTracking` URLs: `videostatsPlaybackUrl` at the
     start, then `videostatsWatchtimeUrl` with the played ranges.
   - Gate it behind an "Add plays to my YouTube history" setting, on when signed in.
   - Plays then appear in YouTube Music history on every device, and recommendations
     improve.
2. **Home → "Continue from another device"**: a card for the newest account-history item
   that isn't the last thing played locally.
   - It shows the title and the device type if known ("played on YouTube Music", otherwise
     just "recently").
   - Clicking **Resume** plays the song and its radio as the queue. Podcasts and long videos
     resume at YouTube's saved position.
3. **Limits**
   - Songs restart from the beginning: YouTube doesn't expose the resume position for music.
   - The queue isn't carried over.
   - It needs the same Google account on both devices.

## Phase 2: CrateTune ↔ CrateTune "Devices" (exact position and queue). Size: large.

A **Devices** button on the player bar (Spotify-style) lists your other CrateTune
installations, each showing what it's playing ("MacBook: Song · 1:23"). Each entry offers:
- **Continue here**: take over at the exact position, with the queue, shuffle and repeat;
  the other device pauses.
- **Play on …**: send playback to that device.

The state sent is the same as Together's `RoomState`: queue, index, position, playing,
repeat and shuffle.

Transport (decided 2026-10-31, after reviewing tono.fm's client): **iroh** peer-to-peer
QUIC links between paired devices.

1. **Pairing:** each installation has a device key (stored in the keychain). Devices pair
   once by exchanging keys with a short code or QR shown on one device and typed or scanned
   on the other; paired keys are remembered.
2. **Discovery and connection:** on the same network devices find each other through iroh's
   local discovery (mDNS); across networks iroh connects directly (hole punching) and falls
   back to a relay only when it must. The relay is **our own `iroh-relay`**, hosted on
   `dev.home` next to `music-relay` (e.g. `iroh.smhcoders.click`), not iroh's public ones.
   The relay only forwards encrypted bytes.
3. **Protocol:** a small versioned message set over the connection: "what are you
   playing" (the `RoomState`-like snapshot), "pause", "take over" (send the snapshot and
   pause), "play this here". Only paired devices are accepted.

3. **On launch**, if another device was playing more recently, show "Continue 'Song' from
   MacBook at 1:23?".

## Phase 3: ArchiveTune on the phone → CrateTune (exact, today's protocol). Size: small to medium.

Through Music Together, which already shares ArchiveTune's protocol. This has **not been
verified** against a real ArchiveTune build.

1. Start a Music Together session in ArchiveTune on the phone, on the same Wi-Fi network.
2. Join it from CrateTune, which then plays in sync at the same position.
3. New: **"Keep playing here when I leave"**. When CrateTune leaves the session, or the
   phone ends it, CrateTune carries on locally with the room's queue and position instead
   of stopping.

The other direction, CrateTune → ArchiveTune, depends on ArchiveTune keeping playback after
it leaves a session. We can't change that from here.

## Phase 4 (optional): one-time library import from ArchiveTune

- Read an ArchiveTune backup (the zip with its Room database) to import liked songs,
  playlists and history.
- This is migration, not resume, but it makes switching apps painless.

## Suggested order

1. Phase 1, step 1: play reporting. It's useful on its own.
2. Phase 3: verify compatibility, then add "keep playing here".
3. Phase 1, step 2: the Continue card.
4. Phase 2 with the synced folder.
5. Phase 2 with the relay mailbox, then LAN Devices.
6. Phase 4 if wanted.
