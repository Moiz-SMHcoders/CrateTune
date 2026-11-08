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
//! On top of the protocol (ours; ArchiveTune peers skip what they don't know, see
//! [`messages`]): session modes ([`SessionMode`]: Host, Friends where everyone
//! controls, DJ where everyone adds), stamped requests so doubled skips collapse and
//! the newest seek wins, queue edits by guests, shared-queue attribution, reactions,
//! activity notes, and Friends sessions that outlive the device they run on
//! ([`session`]).
//!
//! Everything is blocking std threads + `tungstenite`; no async runtime.

mod client;
mod conn;
pub mod link;
pub mod messages;
pub mod online;
pub mod relay;
mod server;
pub mod session;
pub mod sync;

use std::sync::OnceLock;

pub use client::{ClientEvent, ClientOptions, TogetherClient};
pub use link::{JoinInfo, JoinRequest, local_ip, relay_url};
pub use messages::{
    AddTrackMode, ClearPart, ControlAction, IntentMeta, Message, NoteKind, PROTOCOL_VERSION,
    Participant, QueueOp, REACTIONS, Role, RoomSettings, RoomState, SessionMode, Successor,
    SuggestionState, TogetherTrack, queue_hash,
};
pub use online::{CodeError, RelaySource, display_code, normalize_code};
pub use server::{
    HOST_ID, HOST_LEFT, HostOptions, SESSION_ENDED, ServerEvent, Suggestion, TogetherServer,
};
pub use session::{Failover, NoteFeed, Permissions, Playing, RateLimit, Sequencer, Stamp};
pub use sync::{
    ClockSnapshot, GuestSync, LeavePlan, LocalPlayback, SyncPlan, TogetherClock, leave_plan,
};

pub const DEFAULT_PORT: u16 = 42117;

static EVENT_HOOK: OnceLock<fn()> = OnceLock::new();

/// Called (on a session thread) whenever a server or client has a new event, e.g. to
/// wake whatever polls them, so a guest's skip is applied (and its room state
/// followed) without waiting for the next poll. Set once; later calls are ignored.
pub fn set_event_hook(hook: fn()) {
    let _ = EVENT_HOOK.set(hook);
}

fn wake() {
    if let Some(hook) = EVENT_HOOK.get() {
        hook();
    }
}

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
    use crate::together::session;

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
            ..TogetherTrack::default()
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
                mode: SessionMode::Host,
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
                ClientEvent::Disconnected { reason, .. } => {
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
                ClientEvent::Disconnected { reason, .. } => Some(reason),
                _ => None,
            })
        });
        assert_eq!(reason.as_deref(), Some("Session ended"));
    }

    /// "Keep playing here when I leave": the host ends the session; the guest still
    /// has the room as last broadcast and carries on with its queue from the host's
    /// position (extrapolated with the clock); with the setting off it stops.
    #[test]
    fn a_guest_keeps_playing_when_the_host_ends_the_session() {
        let mut server = TogetherServer::start(
            0,
            new_id(),
            new_id(),
            "Phone".into(),
            RoomSettings::default(),
        )
        .unwrap();
        let key = server.session_key().to_owned();
        let guest = TogetherClient::connect(&join(&server, &key), "Mac", "mac").unwrap();
        wait_for(|| {
            server
                .poll_events()
                .into_iter()
                .find(|e| matches!(e, ServerEvent::Joined(_)))
        });
        server.broadcast_room_state(RoomState {
            queue: vec![
                track("aaaaaaaaaaa"),
                track("bbbbbbbbbbb"),
                track("ccccccccccc"),
            ],
            current_index: 1,
            is_playing: true,
            position_ms: 90_000,
            repeat_mode: 1,
            ..RoomState::default()
        });
        let mut room = None;
        let mut ended = false;
        wait_for(|| {
            for event in guest.poll_events() {
                match event {
                    ClientEvent::RoomState(state) => room = Some(state),
                    ClientEvent::Disconnected { .. } => ended = true,
                    _ => {}
                }
            }
            (room.is_some() && guest.clock().samples >= 2).then_some(())
        });
        server.stop();
        wait_for(|| {
            ended |= guest
                .poll_events()
                .iter()
                .any(|e| matches!(e, ClientEvent::Disconnected { .. }));
            ended.then_some(())
        });
        // The guest was following: same queue, same track, a little behind.
        let local = LocalPlayback {
            queue_ids: vec![
                "aaaaaaaaaaa".into(),
                "bbbbbbbbbbb".into(),
                "ccccccccccc".into(),
            ],
            current_id: Some("bbbbbbbbbbb".into()),
            playing: true,
            loading: false,
            position_ms: 90_000,
        };
        let plan = leave_plan(
            true,
            &local,
            room.as_ref(),
            Some(guest.clock()),
            guest.elapsed_ms(),
        );
        let LeavePlan::Continue {
            replace_queue,
            play_index,
            playing,
            repeat_mode,
            ..
        } = plan
        else {
            panic!("{plan:?}")
        };
        assert!(!replace_queue && play_index.is_none() && playing);
        assert_eq!(repeat_mode, 1);
        // Joined late (nothing loaded yet): start the room's track at its position.
        let fresh = LocalPlayback::default();
        let LeavePlan::Continue {
            replace_queue,
            play_index,
            seek_ms,
            ..
        } = leave_plan(
            true,
            &fresh,
            room.as_ref(),
            Some(guest.clock()),
            guest.elapsed_ms(),
        )
        else {
            panic!()
        };
        assert!(replace_queue);
        assert_eq!(play_index, Some(1));
        assert!(seek_ms.is_some_and(|ms| ms >= 90_000), "{seek_ms:?}");
        // The setting off: stop.
        assert_eq!(
            leave_plan(false, &local, room.as_ref(), Some(guest.clock()), 0),
            LeavePlan::Pause
        );
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
        // The guest can only send once it has its participant id (the welcome), which
        // may arrive after the server already counts it as joined.
        wait_for(|| {
            guest
                .poll_events()
                .into_iter()
                .find(|e| matches!(e, ClientEvent::Welcome { .. }))
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

    /// This app's guest (our additions), with a LAN standby address.
    fn ours(
        server: &TogetherServer,
        key: &str,
        name: &str,
        standby: Option<u16>,
    ) -> TogetherClient {
        TogetherClient::connect_with(
            &join(server, key),
            ClientOptions::ours(
                name,
                &format!("{name}-client"),
                standby.map(|p| ("127.0.0.1".to_owned(), p)),
            ),
        )
        .unwrap()
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// Every event `client` has, until `f` finds what it wants (keeping the rest).
    #[track_caller]
    fn wait_event<T>(
        client: &TogetherClient,
        seen: &mut Vec<ClientEvent>,
        mut f: impl FnMut(&ClientEvent) -> Option<T>,
    ) -> T {
        wait_for(|| {
            seen.extend(client.poll_events());
            seen.iter().find_map(&mut f)
        })
    }

    #[track_caller]
    fn wait_server<T>(server: &TogetherServer, mut f: impl FnMut(ServerEvent) -> Option<T>) -> T {
        wait_for(|| server.poll_events().into_iter().find_map(&mut f))
    }

    fn friends_room() -> RoomSettings {
        SessionMode::Friends.apply(RoomSettings {
            require_host_approval_to_join: true,
            ..RoomSettings::default()
        })
    }

    /// The room as the device a Friends session runs on has it mid-song.
    fn mid_song() -> RoomState {
        let mut queue = vec![
            track("aaaaaaaaaaa"),
            track("bbbbbbbbbbb"),
            track("ccccccccccc"),
        ];
        for (i, t) in queue.iter_mut().enumerate() {
            t.uid = Some(10 + i as u64);
        }
        queue[2].queued = true;
        queue[2].added_by = Some("Cat".into());
        RoomState {
            queue,
            current_index: 1,
            is_playing: true,
            position_ms: 61_000,
            repeat_mode: 2,
            context_name: Some("Album".into()),
            ..RoomState::default()
        }
    }

    /// Ann's device runs a Friends session with Bob and Cat (both this app); Ann admits
    /// them, and both hear the roster: Bob, then Cat.
    fn friends_session(
        standby: bool,
    ) -> (
        TogetherServer,
        TogetherClient,
        TogetherClient,
        Vec<ClientEvent>,
        Vec<ClientEvent>,
    ) {
        let server =
            TogetherServer::start(0, new_id(), new_id(), "Ann".into(), friends_room()).unwrap();
        let key = server.session_key().to_owned();
        let bob = ours(&server, &key, "Bob", standby.then(free_port));
        let p = wait_server(&server, |e| match e {
            ServerEvent::JoinRequested(p) => Some(p),
            _ => None,
        });
        server.approve(&p.id, true);
        let cat = ours(&server, &key, "Cat", standby.then(free_port));
        let p = wait_server(&server, |e| match e {
            ServerEvent::JoinRequested(p) => Some(p),
            _ => None,
        });
        server.approve(&p.id, true);
        let (mut bob_seen, mut cat_seen) = (Vec::new(), Vec::new());
        // The roster goes out with the room states (once both said hello).
        let roster_of = |seen: &mut Vec<ClientEvent>, client: &TogetherClient| loop {
            server.broadcast_room_state(mid_song());
            std::thread::sleep(Duration::from_millis(30));
            seen.extend(client.poll_events());
            let found = seen.iter().rev().find_map(|e| match e {
                ClientEvent::Roster { successors, .. } if successors.len() == 2 => {
                    Some(successors.clone())
                }
                _ => None,
            });
            if let Some(found) = found {
                break found;
            }
        };
        let roster = roster_of(&mut bob_seen, &bob);
        assert_eq!(
            roster.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["Bob", "Cat"]
        );
        assert_eq!(roster[0].client_id, "Bob-client");
        assert_eq!(roster[0].host.is_some(), standby);
        roster_of(&mut cat_seen, &cat);
        (server, bob, cat, bob_seen, cat_seen)
    }

    /// Friends mode: Ann leaves mid-song; the session goes on with Bob (the first in
    /// line): Cat reconnects to him, and the queue (slots, who queued what), the track,
    /// the position and playing / paused survive; Ann's device just leaves.
    #[test]
    fn a_friends_session_is_handed_over_when_its_device_leaves() {
        let (mut ann, bob, cat, mut bob_seen, mut cat_seen) = friends_session(false);
        let successors = ann.successors();
        let state = mid_song();
        assert!(ann.transfer_host(&successors[0].participant_id, state, None));
        let (offered, trusted, banned) = wait_event(&bob, &mut bob_seen, |e| match e {
            ClientEvent::HostOffer {
                state,
                trusted_client_ids,
                banned_client_ids,
                ..
            } => Some((
                state.clone().unwrap(),
                trusted_client_ids.clone(),
                banned_client_ids.clone(),
            )),
            _ => None,
        });
        assert_eq!(offered.settings.mode, SessionMode::Friends);
        let new = TogetherServer::start_with(
            0,
            ann.session_id().into(),
            ann.session_key().into(),
            "Bob".into(),
            offered.settings,
            HostOptions {
                trusted_client_ids: trusted,
                banned_client_ids: banned,
                relay_secret: None,
            },
        )
        .unwrap();
        assert!(bob.answer_host_offer(Ok(Some(("127.0.0.1".into(), new.port())))));
        // Ann's device learns it went on, and stops (it doesn't rejoin).
        wait_server(&ann, |e| match e {
            ServerEvent::HostTransferred { participant, .. } => Some(participant),
            ServerEvent::TransferFailed { reason, .. } => panic!("{reason}"),
            _ => None,
        });
        ann.stop();
        bob.leave();
        // Cat follows on her own, straight in (trusted although approval is on).
        wait_event(&cat, &mut cat_seen, |e| {
            matches!(e, ClientEvent::Reconnected).then_some(())
        });
        let joined = wait_server(&new, |e| match e {
            ServerEvent::Joined(p) => Some(p),
            ServerEvent::JoinRequested(p) => panic!("{} should be trusted", p.name),
            _ => None,
        });
        assert_eq!(joined.name, "Cat");
        // Bob's device sends the room as it was handed over (its player was in step).
        let sent = new.broadcast_room_state(offered.clone());
        cat_seen.clear();
        let got = wait_event(&cat, &mut cat_seen, |e| match e {
            ClientEvent::RoomState(s)
                if s.sent_at_elapsed_realtime_ms == sent.sent_at_elapsed_realtime_ms =>
            {
                Some(s.clone())
            }
            _ => None,
        });
        assert_eq!(got.queue, mid_song().queue);
        assert_eq!(got.queue[2].added_by.as_deref(), Some("Cat"));
        assert_eq!(got.queue[2].uid, Some(12));
        assert_eq!(
            (got.current_index, got.is_playing, got.repeat_mode),
            (1, true, 2)
        );
        assert!(got.position_ms >= 61_000, "{}", got.position_ms);
        assert_eq!(got.context_name.as_deref(), Some("Album"));
        assert_eq!(got.settings.mode, SessionMode::Friends);
        // Nobody else to hand it to: an empty roster for Cat (Bob runs it; Cat is
        // the only guest, so she is the only successor).
        let roster = wait_event(&cat, &mut cat_seen, |e| match e {
            ClientEvent::Roster { successors, .. } => Some(successors.clone()),
            _ => None,
        });
        assert_eq!(
            roster.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["Cat"]
        );
    }

    /// Friends mode: Ann's device vanishes mid-song (no goodbye). Bob (first in the
    /// roster) takes over at his standby address with the room as he last had it; Cat
    /// finds him by the roster and is let straight in; the room goes on as it was.
    #[test]
    fn a_friends_session_fails_over_when_its_device_vanishes() {
        let (ann, bob, cat, mut bob_seen, mut cat_seen) = friends_session(true);
        let roster = |seen: &[ClientEvent]| {
            seen.iter()
                .rev()
                .find_map(|e| match e {
                    ClientEvent::Roster {
                        successors,
                        trusted_client_ids,
                        banned_client_ids,
                        relay_secret,
                    } => Some((
                        successors.clone(),
                        trusted_client_ids.clone(),
                        banned_client_ids.clone(),
                        relay_secret.clone(),
                    )),
                    _ => None,
                })
                .unwrap()
        };
        let (successors, trusted, banned, secret) = roster(&bob_seen);
        let last = |seen: &[ClientEvent]| {
            seen.iter()
                .rev()
                .find_map(|e| match e {
                    ClientEvent::RoomState(s) => Some(s.clone()),
                    _ => None,
                })
                .unwrap()
        };
        let bob_room = last(&bob_seen);
        let session = join(&ann, ann.session_key());
        ann.abandon();
        // Both hear the device is gone (not that the session ended).
        for (client, seen) in [(&bob, &mut bob_seen), (&cat, &mut cat_seen)] {
            let lost = wait_event(client, seen, |e| match e {
                ClientEvent::Disconnected { lost, .. } => Some(*lost),
                _ => None,
            });
            assert!(lost, "the session should be carried on");
        }
        // Bob is first in line: he takes over where he said he would.
        assert_eq!(
            session::failover(&successors, "Bob-client", &session),
            Failover::TakeOver
        );
        let port = successors[0].port.unwrap();
        let new = TogetherServer::start_with(
            port,
            session.session_id.clone(),
            session.session_key.clone(),
            "Bob".into(),
            bob_room.settings,
            HostOptions {
                trusted_client_ids: trusted,
                banned_client_ids: banned,
                relay_secret: secret,
            },
        )
        .unwrap();
        // Cat follows the roster to Bob.
        let Failover::Follow(candidates) =
            session::failover(&roster(&cat_seen).0, "Cat-client", &session)
        else {
            panic!("Cat should follow Bob")
        };
        assert_eq!(candidates[0].port, port);
        let cat2 = TogetherClient::connect_with(
            &candidates[0],
            ClientOptions::ours("Cat", "Cat-client", None),
        )
        .unwrap();
        let joined = wait_server(&new, |e| match e {
            ServerEvent::Joined(p) => Some(p),
            ServerEvent::JoinRequested(p) => panic!("{} should be trusted", p.name),
            _ => None,
        });
        assert_eq!(joined.name, "Cat");
        // The room goes on from where it was: the position extrapolated by Bob's clock.
        let clock = bob.clock();
        let at = sync::target_position_ms(&bob_room, Some(clock), bob.elapsed_ms());
        assert!(at >= 61_000);
        new.broadcast_room_state(RoomState {
            position_ms: at,
            ..bob_room.clone()
        });
        let mut seen = Vec::new();
        let got = wait_event(&cat2, &mut seen, |e| match e {
            ClientEvent::RoomState(s) => Some(s.clone()),
            _ => None,
        });
        assert_eq!(got.queue, mid_song().queue);
        assert!(got.is_playing && got.current_index == 1);
        assert!(got.position_ms >= 61_000);
        drop((bob, cat));
    }

    /// Doubled requests over the wire: Bob and Cat both skip song "b" and both seek
    /// it; the host's sequencer skips once and keeps the newest seek. A plain protocol
    /// guest (as ArchiveTune's) in the same room still controls (no stamps).
    #[test]
    fn simultaneous_skips_collapse_and_the_newest_seek_wins() {
        let (ann, bob, cat, _, _) = friends_session(false);
        let plain = TogetherClient::connect(&join(&ann, ann.session_key()), "Dee", "dee").unwrap();
        let p = wait_server(&ann, |e| match e {
            ServerEvent::JoinRequested(p) => Some(p),
            _ => None,
        });
        ann.approve(&p.id, true);
        wait_for(|| plain.participant_id());
        // Not in the roster: an old guest can't take over.
        assert!(ann.successors().iter().all(|s| s.name != "Dee"));
        let b_meta = bob.intent(Some("bbbbbbbbbbb".into()), Some(11));
        let c_meta = cat.intent(Some("bbbbbbbbbbb".into()), Some(11));
        assert!(bob.request_control_with(ControlAction::SkipNext, Some(b_meta)));
        assert!(cat.request_control_with(ControlAction::SkipNext, Some(c_meta)));
        let seek_old = IntentMeta {
            at_ms: 100,
            ..bob.intent(Some("ccccccccccc".into()), None)
        };
        let seek_new = IntentMeta {
            at_ms: 200,
            ..cat.intent(Some("ccccccccccc".into()), None)
        };
        let mut events = Vec::new();
        wait_for(|| {
            events.extend(ann.poll_events().into_iter().filter_map(|e| match e {
                ServerEvent::Control {
                    action,
                    intent,
                    participant,
                } => Some((participant.name, action, intent)),
                _ => None,
            }));
            (events.len() >= 2).then_some(())
        });
        // The host plays "b"; the first skip moves it on to "c", the second is dropped.
        let mut seq = Sequencer::default();
        let mut playing = Playing {
            track_id: Some("bbbbbbbbbbb".into()),
            uid: Some(11),
        };
        let mut skips = 0;
        for (_, action, intent) in &events {
            if seq.admit(action, intent.as_ref(), &playing, ann.elapsed_ms()) {
                skips += 1;
                playing = Playing {
                    track_id: Some("ccccccccccc".into()),
                    uid: Some(12),
                };
            }
        }
        assert_eq!(skips, 1, "{events:?}");
        // Cat's seek (asked later) arrives first; Bob's older one after it is dropped.
        assert!(cat.request_control_with(
            ControlAction::SeekTo {
                position_ms: 90_000
            },
            Some(seek_new)
        ));
        let first = wait_server(&ann, |e| match e {
            ServerEvent::Control { action, intent, .. } => Some((action, intent)),
            _ => None,
        });
        assert!(bob.request_control_with(
            ControlAction::SeekTo {
                position_ms: 30_000
            },
            Some(seek_old)
        ));
        let second = wait_server(&ann, |e| match e {
            ServerEvent::Control { action, intent, .. } => Some((action, intent)),
            _ => None,
        });
        assert!(seq.admit(&first.0, first.1.as_ref(), &playing, ann.elapsed_ms()));
        assert!(!seq.admit(&second.0, second.1.as_ref(), &playing, ann.elapsed_ms()));
        // The plain guest controls without stamps (allowed by the Friends room).
        assert!(plain.request_control(ControlAction::Pause));
        let (action, intent) = wait_server(&ann, |e| match e {
            ServerEvent::Control {
                action,
                intent,
                participant,
            } if participant.name == "Dee" => Some((action, intent)),
            _ => None,
        });
        assert_eq!((action, intent), (ControlAction::Pause, None));
    }

    /// Who may do what, enforced by the server per mode: queue edits and mode switches
    /// only in a Friends session (a jump also with playback control); switching the
    /// mode changes it for everyone, and older guests see the `allow_guests_*` it implies.
    #[test]
    fn modes_decide_what_guests_may_do() {
        let server = TogetherServer::start(
            0,
            new_id(),
            new_id(),
            "Ann".into(),
            SessionMode::Host.apply(RoomSettings::default()),
        )
        .unwrap();
        let key = server.session_key().to_owned();
        let bob = ours(&server, &key, "Bob", None);
        let plain = TogetherClient::connect(&join(&server, &key), "Dee", "dee").unwrap();
        wait_for(|| bob.participant_id());
        wait_for(|| plain.participant_id());
        let mut seen = Vec::new();
        let refused = |client: &TogetherClient, seen: &mut Vec<ClientEvent>| {
            wait_event(client, seen, |e| match e {
                ClientEvent::ServerError { code: Some(c), .. } if c == "not_allowed" => Some(()),
                _ => None,
            });
            seen.clear();
        };
        // Host mode, no control: no queue edits, no jumps, no mode switch, no skips.
        assert!(bob.request_queue(QueueOp::Remove { uid: 1 }, None));
        refused(&bob, &mut seen);
        assert!(bob.request_queue(QueueOp::Jump { uid: 1 }, None));
        refused(&bob, &mut seen);
        assert!(bob.request_mode(SessionMode::Friends));
        refused(&bob, &mut seen);
        assert!(bob.request_control(ControlAction::SkipNext));
        refused(&bob, &mut seen);
        // With control: a jump (playback) but still no queue edits.
        server.update_settings(RoomSettings {
            allow_guests_to_control_playback: true,
            ..server.settings()
        });
        assert!(bob.request_queue(QueueOp::Jump { uid: 7 }, None));
        let op = wait_server(&server, |e| match e {
            ServerEvent::Queue { op, .. } => Some(op),
            _ => None,
        });
        assert_eq!(op, QueueOp::Jump { uid: 7 });
        // DJ: songs yes (approval when asked for), control no.
        server.update_settings(SessionMode::Dj.apply(server.settings()));
        assert!(bob.request_control(ControlAction::Play));
        refused(&bob, &mut seen);
        assert!(bob.request_add_track(track("ddddddddddd"), AddTrackMode::AddToQueue));
        wait_server(&server, |e| {
            matches!(e, ServerEvent::AddTrack { .. }).then_some(())
        });
        // Friends: everything; the mode is on the wire for this app, and an older guest
        // sees "may control, may add".
        server.update_settings(SessionMode::Friends.apply(server.settings()));
        server.broadcast_room_state(mid_song());
        let room = wait_event(&plain, &mut seen, |e| match e {
            ClientEvent::RoomState(s) => Some(s.clone()),
            _ => None,
        });
        assert!(
            room.settings.allow_guests_to_control_playback
                && room.settings.allow_guests_to_add_tracks
        );
        assert_eq!(room.settings.mode, SessionMode::Friends);
        seen.clear();
        assert!(bob.request_queue(
            QueueOp::Move {
                uid: 12,
                to_uid: 10
            },
            None
        ));
        assert!(bob.request_mode(SessionMode::Dj));
        assert!(bob.request_kick("someone", false));
        let mut got = Vec::new();
        wait_for(|| {
            got.extend(server.poll_events().into_iter().filter_map(|e| match e {
                ServerEvent::Queue { op, .. } => Some(format!("{op:?}")),
                ServerEvent::ModeRequest { mode, .. } => Some(format!("{mode:?}")),
                ServerEvent::KickRequest { target_id, .. } => Some(target_id),
                _ => None,
            }));
            (got.len() >= 3).then_some(())
        });
        assert_eq!(got, ["Move { uid: 12, to_uid: 10 }", "Dj", "someone"]);
    }

    /// Reactions: rate-limited per person on the host, passed on with the name to this
    /// app's other guests (never back to the sender, never to older guests); notes
    /// reach this app's guests.
    #[test]
    fn reactions_and_notes_reach_the_others_within_limits() {
        let (ann, bob, cat, _, _) = friends_session(false);
        let plain = TogetherClient::connect(&join(&ann, ann.session_key()), "Dee", "dee").unwrap();
        let p = wait_server(&ann, |e| match e {
            ServerEvent::JoinRequested(p) => Some(p),
            _ => None,
        });
        ann.approve(&p.id, true);
        wait_for(|| plain.participant_id());
        for _ in 0..8 {
            assert!(bob.send_reaction("fire", Some("bbbbbbbbbbb")));
        }
        assert!(bob.send_reaction("not-a-reaction", None));
        // The host shows four (the burst), and so does Cat.
        let mut host_got = 0;
        let mut cat_got = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            host_got += ann
                .poll_events()
                .into_iter()
                .filter(|e| matches!(e, ServerEvent::Reaction { .. }))
                .count();
            cat_got.extend(cat.poll_events().into_iter().filter_map(|e| match e {
                ClientEvent::Reaction { name, emoji, .. } => Some((name, emoji)),
                _ => None,
            }));
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(host_got, 4);
        assert_eq!(cat_got.len(), 4);
        assert_eq!(cat_got[0], ("Bob".to_owned(), "fire".to_owned()));
        assert!(
            bob.poll_events()
                .iter()
                .all(|e| !matches!(e, ClientEvent::Reaction { .. }))
        );
        // The host's own reaction and a note reach Bob and Cat, not the older guest.
        ann.broadcast_reaction("heart", None);
        ann.broadcast_note(&ann.host_participant(), NoteKind::Skipped, 1, None);
        let mut seen = Vec::new();
        let note = wait_event(&cat, &mut seen, |e| match e {
            ClientEvent::Note { name, note, .. } => Some((name.clone(), *note)),
            _ => None,
        });
        assert_eq!(note, ("Ann".to_owned(), NoteKind::Skipped));
        assert!(
            seen.iter()
                .any(|e| matches!(e, ClientEvent::Reaction { emoji, .. } if emoji == "heart"))
        );
        std::thread::sleep(Duration::from_millis(200));
        assert!(plain.poll_events().iter().all(|e| !matches!(
            e,
            ClientEvent::Reaction { .. } | ClientEvent::Note { .. } | ClientEvent::Roster { .. }
        )));
    }

    /// A late joiner lands where the room is now: the host stamps its position, the
    /// guest's clock converges, and the target is the position plus the state's age.
    #[test]
    fn a_late_joiner_lands_at_the_rooms_position() {
        let server =
            TogetherServer::start(0, new_id(), new_id(), "Ann".into(), RoomSettings::default())
                .unwrap();
        let key = server.session_key().to_owned();
        // The host has been playing since "start" (position = time since).
        let start = Instant::now();
        let position = || 40_000 + start.elapsed().as_millis() as u64;
        std::thread::sleep(Duration::from_millis(600));
        let late = ours(&server, &key, "Late", None);
        wait_for(|| late.participant_id());
        let mut room = None;
        let mut seen = Vec::new();
        wait_for(|| {
            server.broadcast_room_state(RoomState {
                position_ms: position(),
                ..mid_song()
            });
            std::thread::sleep(Duration::from_millis(50));
            seen.extend(late.poll_events());
            room = seen.iter().rev().find_map(|e| match e {
                ClientEvent::RoomState(s) => Some(s.clone()),
                _ => None,
            });
            (room.is_some() && late.clock().samples >= 3).then_some(())
        });
        // A moment later (the state ages), the guest aims at the host's position now.
        std::thread::sleep(Duration::from_millis(300));
        let target = sync::target_position_ms(
            room.as_ref().unwrap(),
            Some(late.clock()),
            late.elapsed_ms(),
        );
        let now = position();
        assert!(target.abs_diff(now) < 120, "target {target} vs host {now}");
        assert!(target >= 40_600);
    }
}
