// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/together/
// (protocol, server, client, clock, playback sync, links, host transfer),
// © Rukamori and ArchiveTune contributors, GPL-3.0. The relay mode is our own
// (ArchiveTune's online mode uses its own servers; nothing of it is used here).
//! Music Together: listen with others on the local network or through a relay.
//!
//! The host runs a [`TogetherServer`] (WebSocket on a TCP port, default
//! [`DEFAULT_PORT`], or registered with a self-hosted relay server through
//! [`TogetherServer::start_relay`], see [`relay`]); guests connect with a
//! [`JoinInfo`] (link / address + session key) through [`TogetherClient`]. The
//! host app broadcasts a [`RoomState`] (queue, current index, playing, position
//! stamped with the server clock) every [`sync::BROADCAST_INTERVAL_MS`] and on
//! changes; guests follow it with [`sync::GuestSync`] (clock offset from
//! heartbeats, drift threshold, echo suppression). Guests may suggest songs
//! ([`AddTrackMode`]) and, if the host allows it, control playback
//! ([`ControlAction`]); the host can require approval before anyone joins, kick /
//! ban, and hand the session to a guest ([`TogetherServer::transfer_host`]: the
//! guest starts its own server with the room's state, everyone reconnects to it).
//!
//! Everything is blocking std threads + `tungstenite`; no async runtime.

mod client;
mod conn;
pub mod link;
pub mod messages;
pub mod online;
pub mod relay;
mod server;
pub mod sync;

pub use client::{ClientEvent, TogetherClient};
pub use link::{JoinInfo, JoinRequest, local_ip, relay_url};
pub use messages::{
    AddTrackMode, ControlAction, Message, PROTOCOL_VERSION, Participant, Role, RoomSettings,
    RoomState, SuggestionState, TogetherTrack, queue_hash,
};
pub use online::{CodeError, RelaySource, display_code, normalize_code};
pub use server::{HOST_ID, HostOptions, ServerEvent, Suggestion, TogetherServer};
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

    #[track_caller]
    fn wait_for<T>(mut f: impl FnMut() -> Option<T>) -> T {
        // Generous: on Windows a connection attempt to a port nobody listens on
        // (the old host, while a guest follows the transfer) takes ~2 s to fail.
        let deadline = Instant::now() + Duration::from_secs(20);
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
            url: None,
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
                require_approval_for_suggestions: false,
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
            bob.poll_events().into_iter().find_map(|e| match e {
                ClientEvent::Kicked { banned: true, .. } => Some(()),
                ClientEvent::Disconnected { reason } => {
                    panic!("disconnected without the ban: {reason:?}")
                }
                _ => None,
            })
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

    /// "Suggestions need my approval": a guest's song waits for the host, the guest
    /// hears "pending" and then the decision; without the setting it goes straight in
    /// (and the guest hears "approved").
    #[test]
    fn suggestions_wait_for_the_host() {
        let server = TogetherServer::start(
            0,
            new_id(),
            new_id(),
            "Host".into(),
            RoomSettings {
                require_approval_for_suggestions: true,
                ..RoomSettings::default()
            },
        )
        .unwrap();
        let key = server.session_key().to_owned();
        let guest = TogetherClient::connect(&join(&server, &key), "Ann", "ann").unwrap();
        wait_for(|| {
            server
                .poll_events()
                .into_iter()
                .find(|e| matches!(e, ServerEvent::Joined(_)))
        });
        let status = |want: SuggestionState| {
            wait_for(|| {
                guest.poll_events().into_iter().find_map(|e| match e {
                    ClientEvent::SuggestionStatus {
                        track_id, state, ..
                    } if state == want => Some(track_id),
                    _ => None,
                })
            })
        };

        // Two suggestions wait; nothing is added yet.
        assert!(guest.request_add_track(track("aaaaaaaaaaa"), AddTrackMode::AddToQueue));
        assert!(guest.request_add_track(track("bbbbbbbbbbb"), AddTrackMode::PlayNext));
        let mut pending = Vec::new();
        wait_for(|| {
            for e in server.poll_events() {
                match e {
                    ServerEvent::SuggestionPending(s) => pending.push(s),
                    ServerEvent::AddTrack { .. } => panic!("added without approval"),
                    _ => {}
                }
            }
            (pending.len() == 2).then_some(())
        });
        assert_eq!(status(SuggestionState::Pending), "aaaaaaaaaaa");
        assert_eq!(server.suggestions().len(), 2);
        assert_eq!(pending[0].participant.name, "Ann");
        assert_eq!(pending[1].mode, AddTrackMode::PlayNext);

        // Approve one, reject the other; each is decided once.
        let approved = server.decide_suggestion(&pending[0].id, true).unwrap();
        assert_eq!(approved.track.id, "aaaaaaaaaaa");
        assert_eq!(status(SuggestionState::Approved), "aaaaaaaaaaa");
        assert!(server.decide_suggestion(&pending[0].id, false).is_none());
        server.decide_suggestion(&pending[1].id, false).unwrap();
        assert_eq!(status(SuggestionState::Rejected), "bbbbbbbbbbb");
        assert!(server.suggestions().is_empty());

        // Approval off: straight in, reported as approved.
        server.update_settings(RoomSettings::default());
        assert!(guest.request_add_track(track("ccccccccccc"), AddTrackMode::AddToQueue));
        let added = wait_for(|| {
            server.poll_events().into_iter().find_map(|e| match e {
                ServerEvent::AddTrack { track, .. } => Some(track),
                ServerEvent::SuggestionPending(_) => panic!("approval is off"),
                _ => None,
            })
        });
        assert_eq!(added.id, "ccccccccccc");
        assert_eq!(status(SuggestionState::Approved), "ccccccccccc");

        // A guest can't flood the host: past the cap, suggestions are turned down.
        server.update_settings(RoomSettings {
            require_approval_for_suggestions: true,
            ..RoomSettings::default()
        });
        for i in 0..=server::MAX_PENDING_PER_GUEST {
            let id = format!("{:0>11}", i);
            assert!(guest.request_add_track(track(&id), AddTrackMode::AddToQueue));
        }
        assert_eq!(
            status(SuggestionState::Rejected),
            format!("{:0>11}", server::MAX_PENDING_PER_GUEST)
        );
        assert_eq!(server.suggestions().len(), server::MAX_PENDING_PER_GUEST);
    }

    /// Host → guest hand-over on the LAN: Ann takes over, Bob and the old host
    /// reconnect to her (no approval needed for them), the state carries over.
    #[test]
    fn host_transfer_on_the_lan() {
        let mut old = TogetherServer::start(
            0,
            new_id(),
            new_id(),
            "Hana".into(),
            RoomSettings {
                require_host_approval_to_join: true,
                ..RoomSettings::default()
            },
        )
        .unwrap();
        let key = old.session_key().to_owned();
        let ann = TogetherClient::connect(&join(&old, &key), "Ann", "ann-client").unwrap();
        let bob = TogetherClient::connect(&join(&old, &key), "Bob", "bob-client").unwrap();
        // Both requests can arrive in one poll: keep every one seen.
        let mut requests = Vec::new();
        wait_for(|| {
            requests.extend(old.poll_events().into_iter().filter_map(|e| match e {
                ServerEvent::JoinRequested(p) => Some(p),
                _ => None,
            }));
            (requests.len() >= 2).then_some(())
        });
        for p in &requests {
            old.approve(&p.id, true);
        }
        let ann_id = wait_for(|| ann.participant_id());
        wait_for(|| bob.participant_id());
        // Banned before the transfer: stays banned.
        let eve = TogetherClient::connect(&join(&old, &key), "Eve", "eve-client").unwrap();
        let eve_p = wait_for(|| {
            old.poll_events().into_iter().find_map(|e| match e {
                ServerEvent::JoinRequested(p) => Some(p),
                _ => None,
            })
        });
        old.kick(&eve_p.id, None, true);
        drop(eve);

        let state = RoomState {
            queue: vec![track("aaaaaaaaaaa"), track("bbbbbbbbbbb")],
            current_index: 1,
            is_playing: true,
            position_ms: 42_000,
            repeat_mode: 2,
            ..RoomState::default()
        };
        assert!(!old.transfer_host("nobody", state.clone(), None));
        assert!(old.transfer_host(&ann_id, state, Some("hana-client")));
        assert!(old.transfer_pending());

        // Ann gets the offer with the room and the guest lists, and starts a server.
        let (offered, trusted, banned) = wait_for(|| {
            ann.poll_events().into_iter().find_map(|e| match e {
                ClientEvent::HostOffer {
                    state,
                    trusted_client_ids,
                    banned_client_ids,
                    relay_secret,
                } => {
                    assert!(relay_secret.is_none());
                    Some((state.unwrap(), trusted_client_ids, banned_client_ids))
                }
                _ => None,
            })
        });
        assert_eq!(offered.position_ms, 42_000);
        assert_eq!(offered.current().unwrap().id, "bbbbbbbbbbb");
        assert_eq!(offered.repeat_mode, 2);
        assert!(offered.settings.require_host_approval_to_join);
        for id in ["ann-client", "bob-client", "hana-client"] {
            assert!(trusted.contains(&id.to_owned()), "{trusted:?}");
        }
        assert_eq!(banned, vec!["eve-client".to_owned()]);
        let new = TogetherServer::start_with(
            0,
            old.session_id().into(),
            key.clone(),
            "Ann".into(),
            offered.settings,
            HostOptions {
                trusted_client_ids: trusted,
                banned_client_ids: banned,
                relay_secret: None,
            },
        )
        .unwrap();
        assert!(ann.answer_host_offer(Ok(Some(("127.0.0.1".into(), new.port())))));

        // The old host learns where the session went.
        let (who, moved) = wait_for(|| {
            old.poll_events().into_iter().find_map(|e| match e {
                ServerEvent::HostTransferred { participant, join } => Some((participant, join)),
                ServerEvent::TransferFailed { reason, .. } => panic!("{reason}"),
                _ => None,
            })
        });
        assert_eq!(who.name, "Ann");
        assert_eq!((moved.host.as_str(), moved.port), ("127.0.0.1", new.port()));
        assert!(!old.transfer_pending());

        // Bob follows on his own, straight in (trusted). `HostChanged` and
        // `Reconnected` can arrive in the same poll, so keep every event seen.
        let mut seen = Vec::new();
        wait_for(|| {
            seen.extend(bob.poll_events());
            seen.iter()
                .any(|e| matches!(e, ClientEvent::Reconnected))
                .then_some(())
        });
        let changed = seen
            .iter()
            .find_map(|e| match e {
                ClientEvent::HostChanged { name, .. } => Some(name.clone()),
                _ => None,
            })
            .expect("HostChanged before Reconnected");
        assert_eq!(changed.as_deref(), Some("Ann"));
        let joined = wait_for(|| {
            new.poll_events().into_iter().find_map(|e| match e {
                ServerEvent::Joined(p) => Some(p),
                ServerEvent::JoinRequested(p) => panic!("{} should be trusted", p.name),
                _ => None,
            })
        });
        assert_eq!(joined.name, "Bob");
        assert_eq!(bob.join_info().port, new.port());
        // Ann's old client doesn't reconnect; the old host ends and rejoins as a guest.
        ann.leave();
        old.stop();
        let hana = TogetherClient::connect(&moved, "Hana", "hana-client").unwrap();
        let joined = wait_for(|| {
            new.poll_events().into_iter().find_map(|e| match e {
                ServerEvent::Joined(p) => Some(p),
                ServerEvent::JoinRequested(p) => panic!("{} should be trusted", p.name),
                _ => None,
            })
        });
        assert_eq!(joined.name, "Hana");
        // Bob gets the new host's room state and its clock.
        let sent = new.broadcast_room_state(offered.clone());
        let got = wait_for(|| {
            bob.poll_events().into_iter().find_map(|e| match e {
                ClientEvent::RoomState(s) => Some(s),
                _ => None,
            })
        });
        assert_eq!(got.queue, sent.queue);
        wait_for(|| Some(bob.clock()).filter(|c| c.samples >= 2));
        // Eve stays banned.
        let eve = TogetherClient::connect(&moved, "Eve", "eve-client").unwrap();
        wait_for(|| {
            eve.poll_events().into_iter().find(
                |e| matches!(e, ClientEvent::ServerError { code: Some(c), .. } if c == "banned"),
            )
        });
        drop(hana);
    }
}
