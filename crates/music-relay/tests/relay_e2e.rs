//! Relay + host + two guests on localhost, over real sockets: joining through the
//! relay (wrong key, full room), room state, clock, suggestions, the health
//! endpoint, a host transfer the relay re-routes, and the end of the session;
//! room codes (a guest joins by code, the code survives the transfer and goes
//! with the room, the host key never comes back, look-ups are rate-limited).

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use music_core::together::online::resolve_code;
use music_core::together::relay::sha256_hex;
use music_core::together::{
    AddTrackMode, ClientEvent, CodeError, HostOptions, JoinInfo, RoomSettings, RoomState,
    ServerEvent, TogetherClient, TogetherServer, TogetherTrack, display_code, new_id,
};
use music_relay::{Config, Relay};

#[track_caller]
fn wait_for<T>(mut f: impl FnMut() -> Option<T>) -> T {
    // Generous: on Windows a connection attempt to a port nobody listens on takes ~2 s
    // to fail, and a closing link waits up to 5 s for its peer's reply.
    let deadline = Instant::now() + Duration::from_secs(30);
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

fn joined(server: &TogetherServer) -> String {
    wait_for(|| {
        server.poll_events().into_iter().find_map(|e| match e {
            ServerEvent::Joined(p) => Some(p.name),
            ServerEvent::JoinRequested(p) => panic!("{} should not wait", p.name),
            _ => None,
        })
    })
}

fn refused(client: &TogetherClient) -> String {
    wait_for(|| {
        client.poll_events().into_iter().find_map(|e| match e {
            ClientEvent::ServerError { code, .. } => code,
            _ => None,
        })
    })
}

const T: Duration = Duration::from_secs(5);

/// A raw `POST` to the relay: (status, body).
fn post(relay: &Relay, path: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(relay.addr()).unwrap();
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: relay\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).unwrap();
    let status = out
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = out.split_once("\r\n\r\n").map_or("", |(_, b)| b).to_owned();
    (status, body)
}

fn health(relay: &Relay) -> String {
    let mut stream = TcpStream::connect(relay.addr()).unwrap();
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: relay\r\n\r\n")
        .unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).unwrap();
    out
}

#[test]
fn relay_host_two_guests_and_a_transfer() {
    let relay = Relay::start(Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        max_guests_per_room: 2,
        ..Config::default()
    })
    .unwrap();
    let url = format!("ws://{}", relay.addr());
    let (sid, key) = (new_id(), new_id());
    let settings = RoomSettings {
        allow_guests_to_add_tracks: true,
        allow_guests_to_control_playback: false,
        require_host_approval_to_join: true,
    };
    let mut host = TogetherServer::start_relay(
        &url,
        sid.clone(),
        key.clone(),
        "Hana".into(),
        settings,
        HostOptions::default(),
    )
    .unwrap();
    assert_eq!(host.port(), 0);
    assert_eq!(host.relay_url(), Some(format!("{url}/").as_str()));
    // A second host can't take the session without its secret.
    assert!(
        TogetherServer::start_relay(
            &url,
            sid.clone(),
            key.clone(),
            "Mallory".into(),
            settings,
            HostOptions::default(),
        )
        .is_err()
    );
    let join = JoinInfo::via_relay(&url, sid.clone(), key.clone()).unwrap();
    assert!(JoinInfo::parse(&join.to_link()).is_some_and(|j| j.is_relay()));

    // Wrong key: the relay refuses it before the host hears of it.
    let bad = JoinInfo {
        session_key: "nope".into(),
        ..join.clone()
    };
    let mallory = TogetherClient::connect(&bad, "Mallory", "m").unwrap();
    assert_eq!(refused(&mallory), "invalid_session");

    // Two guests through the relay; approval runs on the host as on the LAN.
    // Ann has the link; Bob types the room code.
    let code = host.register_code(T).unwrap();
    assert_eq!(host.register_code(T).unwrap(), code, "one code per room");
    let by_code = resolve_code(&url, &display_code(&code).to_lowercase(), T).unwrap();
    assert_eq!(by_code, join, "the code leads to the same session");
    let ann = TogetherClient::connect(&join, "Ann", "ann-client").unwrap();
    let bob = TogetherClient::connect(&by_code, "Bob", "bob-client").unwrap();
    // Both requests can arrive in one poll: keep every one seen.
    let mut requests = Vec::new();
    wait_for(|| {
        requests.extend(host.poll_events().into_iter().filter_map(|e| match e {
            ServerEvent::JoinRequested(p) => Some(p),
            _ => None,
        }));
        (requests.len() >= 2).then_some(())
    });
    for p in &requests {
        host.approve(&p.id, true);
    }
    let ann_id = wait_for(|| ann.participant_id());
    wait_for(|| bob.participant_id());
    wait_for(|| (relay.stats().guests == 2).then_some(()));
    assert_eq!(relay.stats().rooms, 1);
    // The room is full for a third.
    let carol = TogetherClient::connect(&join, "Carol", "carol").unwrap();
    assert_eq!(refused(&carol), "room_full");
    let page = health(&relay);
    assert!(page.starts_with("HTTP/1.1 200 OK"), "{page}");
    assert!(
        page.contains(r#""status":"ok""#) && page.contains(r#""rooms":1"#),
        "{page}"
    );

    // Room state, clock (through the relay, on the host's clock), suggestions.
    let state = RoomState {
        queue: vec![track("aaaaaaaaaaa"), track("bbbbbbbbbbb")],
        current_index: 0,
        is_playing: true,
        position_ms: 9_000,
        ..RoomState::default()
    };
    let sent = host.broadcast_room_state(state.clone());
    for guest in [&ann, &bob] {
        let got = wait_for(|| {
            guest.poll_events().into_iter().find_map(|e| match e {
                ClientEvent::RoomState(s) => Some(s),
                _ => None,
            })
        });
        assert_eq!(got, sent);
    }
    let clock = wait_for(|| Some(bob.clock()).filter(|c| c.samples >= 3));
    let expected = host.elapsed_ms() as i64 - bob.elapsed_ms() as i64;
    assert!(
        (clock.offset_ms - expected).abs() < 80,
        "{clock:?} vs {expected}"
    );
    assert!(bob.request_add_track(track("ccccccccccc"), AddTrackMode::AddToQueue));
    let suggested = wait_for(|| {
        host.poll_events().into_iter().find_map(|e| match e {
            ServerEvent::AddTrack {
                track, participant, ..
            } => Some((track.id, participant.name)),
            _ => None,
        })
    });
    assert_eq!(suggested, ("ccccccccccc".into(), "Bob".into()));

    // Host transfer: Ann claims the room on the relay with its secret.
    assert!(host.transfer_host(&ann_id, state, Some("hana-client")));
    let (offered, trusted, banned, secret) = wait_for(|| {
        ann.poll_events().into_iter().find_map(|e| match e {
            ClientEvent::HostOffer {
                state,
                trusted_client_ids,
                banned_client_ids,
                relay_secret,
            } => Some((state?, trusted_client_ids, banned_client_ids, relay_secret)),
            _ => None,
        })
    });
    assert!(secret.is_some());
    let mut new_host = TogetherServer::start_relay(
        &url,
        sid.clone(),
        key.clone(),
        "Ann".into(),
        offered.settings,
        HostOptions {
            trusted_client_ids: trusted,
            banned_client_ids: banned,
            relay_secret: secret,
        },
    )
    .unwrap();
    assert!(ann.answer_host_offer(Ok(None)));
    let moved = wait_for(|| {
        host.poll_events().into_iter().find_map(|e| match e {
            ServerEvent::HostTransferred { join, .. } => Some(join),
            ServerEvent::TransferFailed { reason, .. } => panic!("{reason}"),
            _ => None,
        })
    });
    assert_eq!(moved, join, "the same relay link keeps working");
    // The room keeps its code with the new host.
    assert_eq!(new_host.register_code(T).unwrap(), code);
    assert_eq!(relay.stats().codes, 1);
    ann.leave();
    // Bob is re-routed to Ann without approval.
    wait_for(|| {
        bob.poll_events()
            .into_iter()
            .find(|e| matches!(e, ClientEvent::Reconnected))
    });
    assert_eq!(joined(&new_host), "Bob");
    // The old host ends its side and comes back as a guest.
    host.stop();
    let hana = TogetherClient::connect(&moved, "Hana", "hana-client").unwrap();
    assert_eq!(joined(&new_host), "Hana");
    let sent = new_host.broadcast_room_state(offered);
    let got = wait_for(|| {
        bob.poll_events().into_iter().find_map(|e| match e {
            ClientEvent::RoomState(s)
                if s.sent_at_elapsed_realtime_ms == sent.sent_at_elapsed_realtime_ms =>
            {
                Some(s)
            }
            _ => None,
        })
    });
    assert_eq!(got.position_ms, 9_000);
    assert_eq!(relay.stats().rooms, 1);

    // The new host ends the session: everyone is told, the room is gone.
    new_host.stop();
    for guest in [&bob, &hana] {
        let reason = wait_for(|| {
            guest.poll_events().into_iter().find_map(|e| match e {
                ClientEvent::Disconnected { reason } => Some(reason),
                _ => None,
            })
        });
        assert_eq!(reason.as_deref(), Some("Session ended"));
    }
    wait_for(|| (relay.stats().rooms == 0).then_some(()));
    // The code went with the room.
    assert_eq!(relay.stats().codes, 0);
    assert_eq!(resolve_code(&url, &code, T), Err(CodeError::NotFound));
}

#[test]
fn room_codes_keep_the_host_key_and_are_rate_limited() {
    let relay = Relay::start(Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        resolve_per_minute: 6,
        ..Config::default()
    })
    .unwrap();
    let url = format!("ws://{}", relay.addr());
    let (sid, key, secret) = (new_id(), new_id(), new_id());
    let mut host = TogetherServer::start_relay(
        &url,
        sid.clone(),
        key.clone(),
        "Hana".into(),
        RoomSettings::default(),
        HostOptions {
            relay_secret: Some(secret.clone()),
            ..HostOptions::default()
        },
    )
    .unwrap();
    // Only the room's host (guest key + host key) gets a code.
    let create = |guest: &str, host_key: &str| {
        post(
            &relay,
            "/v1/together/sessions",
            &format!(r#"{{"sessionId":"{sid}","guestKey":"{guest}","hostKey":"{host_key}"}}"#),
        )
    };
    assert_eq!(create(&key, "wrong").0, 403);
    assert_eq!(create("wrong", &secret).0, 403);
    assert_eq!(
        post(
            &relay,
            "/v1/together/sessions",
            r#"{"sessionId":"nope","guestKey":"a","hostKey":"b"}"#
        )
        .0,
        403
    );
    assert_eq!(post(&relay, "/v1/together/sessions", "{}").0, 400);
    assert_eq!(relay.stats().codes, 0);
    let (status, created) = create(&key, &secret);
    assert_eq!(status, 200, "{created}");
    let code = host.register_code(T).unwrap();
    assert!(created.contains(&code), "{created}");
    assert_eq!(code.len(), 8);
    assert!(!created.contains(&secret) && !created.contains(&sha256_hex(&secret)));

    // Resolving gives the guest key and the WebSocket path, never the host key.
    // (Attempt 1.)
    let (status, resolved) = post(
        &relay,
        "/v1/together/sessions/resolve",
        &format!(r#"{{"code":"{}"}}"#, display_code(&code).to_lowercase()),
    );
    assert_eq!(status, 200, "{resolved}");
    let value: serde_json::Value = serde_json::from_str(&resolved).unwrap();
    assert_eq!(value["sessionId"], sid.as_str());
    assert_eq!(value["guestKey"], key.as_str());
    assert_eq!(value["wsUrl"], "/");
    assert!(value.get("hostKey").is_none());
    assert!(!resolved.contains(&secret) && !resolved.contains(&sha256_hex(&secret)));

    // A guest joins with the code alone (attempt 2); the host still decides.
    let join = resolve_code(&url, &code, T).unwrap();
    let ann = TogetherClient::connect(&join, "Ann", "ann").unwrap();
    assert_eq!(joined(&host), "Ann");
    wait_for(|| ann.participant_id());

    // Wrong and malformed codes (attempts 3–5), the method and the size.
    let other = if code.starts_with('A') {
        "BBBBBBBB"
    } else {
        "AAAAAAAA"
    };
    assert_eq!(resolve_code(&url, other, T), Err(CodeError::NotFound));
    let (status, body) = post(&relay, "/v1/together/sessions/resolve", r#"{"code":"x"}"#);
    assert_eq!((status, body.contains("bad_code")), (400, true), "{body}");
    let (status, _) = post(&relay, "/v1/together/sessions/resolve", "{}");
    assert_eq!(status, 400);
    let mut stream = TcpStream::connect(relay.addr()).unwrap();
    stream
        .write_all(b"GET /v1/together/sessions/resolve HTTP/1.1\r\nHost: relay\r\n\r\n")
        .unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).unwrap();
    assert!(out.starts_with("HTTP/1.1 405"), "{out}");
    let big = format!(r#"{{"code":"{}"}}"#, "A".repeat(5000));
    assert_eq!(post(&relay, "/v1/together/sessions/resolve", &big).0, 413);

    // Attempt 6 is the last this minute; then even the right code is refused.
    assert!(resolve_code(&url, &code, T).is_ok());
    match resolve_code(&url, &code, T) {
        Err(CodeError::RateLimited(Some(secs))) => assert!((1..=10).contains(&secs), "{secs}"),
        other => panic!("expected a rate limit, got {other:?}"),
    }
    let (status, body) = post(
        &relay,
        "/v1/together/sessions/resolve",
        &format!(r#"{{"code":"{code}"}}"#),
    );
    assert_eq!(status, 429, "{body}");
    // The host's own requests aren't limited.
    assert_eq!(host.register_code(T).unwrap(), code);

    // The room ends: its code is gone.
    host.stop();
    wait_for(|| (relay.stats().rooms == 0).then_some(()));
    assert_eq!(relay.stats().codes, 0);
    assert_eq!(create(&key, &secret).0, 403);
    drop(ann);
}
