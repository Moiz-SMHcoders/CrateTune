// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/together/
// (protocol, server, client, clock, playback sync, links), © Rukamori and ArchiveTune
// contributors, GPL-3.0. Only the LAN mode: no online relay.
//! Music Together: listen with others on the local network.
//!
//! The host runs a [`TogetherServer`] (WebSocket on a TCP port, default
//! [`DEFAULT_PORT`]); guests connect with a [`JoinInfo`] (link / address + session
//! key) through [`TogetherClient`]. The host app broadcasts a [`RoomState`] (queue,
//! current index, playing, position stamped with the server clock) every
//! [`sync::BROADCAST_INTERVAL_MS`] and on changes; guests follow it with
//! [`sync::GuestSync`] (clock offset from heartbeats, drift threshold, echo
//! suppression). Guests may suggest songs ([`AddTrackMode`]) and, if the host
//! allows it, control playback ([`ControlAction`]); the host can require approval
//! before anyone joins, and kick / ban.
//!
//! Everything is blocking std threads + `tungstenite`; no async runtime.

mod client;
mod conn;
pub mod link;
pub mod messages;
mod server;
pub mod sync;

pub use client::{ClientEvent, TogetherClient};
pub use link::{JoinInfo, local_ip};
pub use messages::{
    AddTrackMode, ControlAction, Message, PROTOCOL_VERSION, Participant, Role, RoomSettings,
    RoomState, TogetherTrack, queue_hash,
};
pub use server::{HOST_ID, ServerEvent, TogetherServer};
pub use sync::{ClockSnapshot, GuestSync, LocalPlayback, SyncPlan, TogetherClock};

pub const DEFAULT_PORT: u16 = 42117;

/// A random id / key (URL-safe, 22 characters).
pub fn new_id() -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(crate::spotify::random_bytes(16))
}

/// Only YouTube video ids travel through a session (no local files).
pub fn is_shareable_id(id: &str) -> bool {
    crate::sponsorblock::is_video_id(id)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn wait_for<T>(mut f: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(v) = f() {
                return v;
            }
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn track(id: &str) -> TogetherTrack {
        TogetherTrack {
            id: id.into(),
            title: format!("Song {id}"),
            artists: vec!["Artist".into()],
            duration_sec: 200,
            thumbnail_url: None,
        }
    }

    fn join(server: &TogetherServer, key: &str) -> JoinInfo {
        JoinInfo {
            host: "127.0.0.1".into(),
            port: server.port(),
            session_id: server.session_id().into(),
            session_key: key.into(),
        }
    }

    /// Host + two guests over real sockets on localhost.
    #[test]
    fn session_end_to_end() {
        let mut server = TogetherServer::start(
            0,
            new_id(),
            new_id(),
            "Host".into(),
            RoomSettings {
                allow_guests_to_add_tracks: true,
                allow_guests_to_control_playback: false,
                require_host_approval_to_join: false,
            },
        )
        .unwrap();
        let key = server.session_key().to_owned();

        // Wrong key: refused.
        let bad = TogetherClient::connect(&join(&server, "nope"), "Mallory", "m").unwrap();
        let events = wait_for(|| {
            let e = bad.poll_events();
            e.iter()
                .any(|e| matches!(e, ClientEvent::Disconnected { .. }))
                .then_some(e)
        });
        assert!(events.iter().any(
            |e| matches!(e, ClientEvent::ServerError { code: Some(c), .. } if c == "invalid_session")
        ));

        let guest = TogetherClient::connect(&join(&server, &key), "  Ann  ", "ann").unwrap();
        let joined = wait_for(|| {
            server.poll_events().into_iter().find_map(|e| match e {
                ServerEvent::Joined(p) => Some(p),
                _ => None,
            })
        });
        assert_eq!(joined.name, "Ann");
        assert_eq!(server.participants().len(), 2);
        assert!(server.participants()[0].is_host);

        // Room state reaches the guest, stamped and hashed.
        let sent = server.broadcast_room_state(RoomState {
            queue: vec![track("aaaaaaaaaaa"), track("bbbbbbbbbbb")],
            current_index: 1,
            is_playing: true,
            position_ms: 12_000,
            ..RoomState::default()
        });
        let state = wait_for(|| {
            guest.poll_events().into_iter().find_map(|e| match e {
                ClientEvent::RoomState(s) => Some(s),
                _ => None,
            })
        });
        assert_eq!(state, sent);
        assert_eq!(state.queue_hash, queue_hash(&state.queue));
        assert_eq!(state.participants.len(), 2);

        // The clock converges: offset ≈ server − client (both started ~together here).
        let clock = wait_for(|| Some(guest.clock()).filter(|c| c.samples >= 3));
        let expected = server.elapsed_ms() as i64 - guest.elapsed_ms() as i64;
        assert!(
            (clock.offset_ms - expected).abs() < 50,
            "{clock:?} vs {expected}"
        );
        // The host's position "now": stamped position + the state's age on the host clock.
        let age = server.elapsed_ms() - state.sent_at_elapsed_realtime_ms;
        let target = sync::target_position_ms(&state, Some(clock), guest.elapsed_ms());
        assert!(target.abs_diff(12_000 + age) < 50, "{target} vs {age}");

        // Suggestions are relayed; control is refused by the room settings.
        assert!(guest.request_add_track(track("ccccccccccc"), AddTrackMode::PlayNext));
        assert!(guest.request_control(ControlAction::SkipNext));
        let add = wait_for(|| {
            server.poll_events().into_iter().find_map(|e| match e {
                ServerEvent::AddTrack {
                    track,
                    mode,
                    participant,
                } => Some((track, mode, participant)),
                ServerEvent::Control { .. } => panic!("control should be refused"),
                _ => None,
            })
        });
        assert_eq!(add.0.id, "ccccccccccc");
        assert_eq!(add.1, AddTrackMode::PlayNext);
        assert_eq!(add.2.name, "Ann");
        wait_for(|| {
            guest.poll_events().into_iter().find(|e| {
                matches!(e, ClientEvent::ServerError { code: Some(c), .. } if c == "not_allowed")
            })
        });
        server.update_settings(RoomSettings {
            allow_guests_to_control_playback: true,
            require_host_approval_to_join: true,
            ..server.settings()
        });
        assert!(guest.request_control(ControlAction::SeekTo { position_ms: 5_000 }));
        let action = wait_for(|| {
            server.poll_events().into_iter().find_map(|e| match e {
                ServerEvent::Control { action, .. } => Some(action),
                _ => None,
            })
        });
        assert_eq!(action, ControlAction::SeekTo { position_ms: 5_000 });

        // Approval: the second guest waits, sees no queue, then gets in.
        let bob = TogetherClient::connect(&join(&server, &key), "Bob", "bob").unwrap();
        let pending = wait_for(|| {
            server.poll_events().into_iter().find_map(|e| match e {
                ServerEvent::JoinRequested(p) => Some(p),
                _ => None,
            })
        });
        assert!(pending.is_pending);
        server.broadcast_room_state(sent.clone());
        let redacted = wait_for(|| {
            bob.poll_events().into_iter().find_map(|e| match e {
                ClientEvent::RoomState(s) => Some(s),
                _ => None,
            })
        });
        assert!(redacted.queue.is_empty() && redacted.participants.len() == 3);
        server.approve(&pending.id, true);
        wait_for(|| {
            bob.poll_events()
                .into_iter()
                .find(|e| matches!(e, ClientEvent::JoinDecision { approved: true }))
        });

        // Ban Bob: he is told, and can't come back.
        server.kick(&pending.id, None, true);
        wait_for(|| {
            bob.poll_events()
                .into_iter()
                .find(|e| matches!(e, ClientEvent::Kicked { banned: true, .. }))
        });
        let again = TogetherClient::connect(&join(&server, &key), "Bob", "bob").unwrap();
        wait_for(|| {
            again.poll_events().into_iter().find(
                |e| matches!(e, ClientEvent::ServerError { code: Some(c), .. } if c == "banned"),
            )
        });

        // Ann leaves; then the host ends the session.
        guest.leave();
        let left = wait_for(|| {
            server.poll_events().into_iter().find_map(|e| match e {
                ServerEvent::Left {
                    participant,
                    reason,
                } if participant.name == "Ann" => Some(reason),
                _ => None,
            })
        });
        assert_eq!(left, "Left");
        let carol = TogetherClient::connect(&join(&server, &key), "Carol", "carol").unwrap();
        wait_for(|| {
            server
                .poll_events()
                .into_iter()
                .find(|e| matches!(e, ServerEvent::JoinRequested(_)))
        });
        server.stop();
        let reason = wait_for(|| {
            carol.poll_events().into_iter().find_map(|e| match e {
                ClientEvent::Disconnected { reason } => Some(reason),
                _ => None,
            })
        });
        assert_eq!(reason.as_deref(), Some("Session ended"));
    }
}
