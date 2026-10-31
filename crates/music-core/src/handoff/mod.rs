//! Device hand-off: carry on listening on another of your devices, Spotify-style,
//! without anyone else's servers (phase 2 of `docs/plans/cross-device-resume.md`).
//!
//! Each installation has a device key (an Ed25519 key kept in the keychain); its
//! public half is the device id. Two devices pair once with a short code
//! ([`pairing`]) and from then on only accept each other. They talk over iroh: QUIC
//! connections dialled by device id, found on the LAN by mDNS, otherwise through
//! the relay and direct addresses learnt at pairing ([`node`]). The relay is our own
//! `iroh-relay` (`deploy/iroh-relay/`) or none; iroh's public relays and its DNS
//! address lookup are never used.
//!
//! The protocol ([`messages`]) is a handful of requests: what are you playing,
//! pause, take over (send what you play, then pause), play this here. What is sent is
//! the state Music Together's `RoomState` carries: queue, index, position, playing,
//! repeat, shuffle, plus the device name.

pub mod messages;
pub mod node;
pub mod pairing;

pub use messages::{PROTOCOL_VERSION, PeerInfo, Snapshot};
pub use node::{
    Action, Event, HandoffNode, NodeConfig, PairError, PairedDevice, PairingOffer, now_unix_ms,
};
pub use pairing::{PairInput, PairingLink, display_code, normalize_code};

/// A new device key (hex; stored as a secret, never logged).
pub fn new_device_key() -> String {
    let key = iroh::SecretKey::generate();
    hex(&key.to_bytes())
}

/// The key's bytes, if it is a valid stored key.
pub fn parse_device_key(stored: &str) -> Option<[u8; 32]> {
    let stored = stored.trim();
    if stored.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(stored.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

/// The device id (hex public key) of a stored key.
pub fn device_id(key: &[u8; 32]) -> String {
    iroh::SecretKey::from_bytes(key).public().to_string()
}

/// A device id shortened for display (`3f2a…9c01`).
pub fn short_id(id: &str) -> String {
    if id.len() > 12 && id.is_ascii() {
        format!("{}…{}", &id[..4], &id[id.len() - 4..])
    } else {
        id.to_owned()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::together::TogetherTrack;

    #[track_caller]
    fn wait_for<T>(node: &HandoffNode, mut f: impl FnMut(Event) -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            for event in node.poll_events() {
                if let Some(v) = f(event) {
                    return v;
                }
            }
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn config(name: &str, paired: Vec<PairedDevice>) -> (NodeConfig, String) {
        let key = parse_device_key(&new_device_key()).unwrap();
        let id = device_id(&key);
        (
            NodeConfig {
                secret_key: key,
                name: name.into(),
                // No relay, no mDNS: localhost only (the tests have no network).
                relay_url: None,
                paired,
                local_discovery: false,
                service_name: "cratetune-test".into(),
                port: 0,
            },
            id,
        )
    }

    /// Where a node listens, as a paired device record (localhost).
    fn record(node: &HandoffNode, name: &str) -> PairedDevice {
        let (_, addrs) = node.reachable();
        PairedDevice {
            id: node.id(),
            name: name.into(),
            relay_url: None,
            addrs: addrs
                .into_iter()
                .filter(|a| a.is_ipv4())
                .map(|a| std::net::SocketAddr::from(([127, 0, 0, 1], a.port())))
                .collect(),
            paired_at_unix_ms: 1,
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

    #[test]
    fn device_keys() {
        let key = new_device_key();
        assert_eq!(key.len(), 64);
        let bytes = parse_device_key(&key).unwrap();
        assert_eq!(device_id(&bytes), device_id(&bytes));
        assert_eq!(device_id(&bytes).len(), 64);
        assert_ne!(device_id(&bytes), key, "the id is the public half");
        assert_ne!(new_device_key(), key);
        assert_eq!(parse_device_key("zz"), None);
        assert_eq!(parse_device_key(&"g".repeat(64)), None);
        assert_eq!(short_id(&"ab".repeat(32)), "abab…abab");
    }

    /// Two devices paired with each other on localhost (relay disabled): one asks
    /// what the other plays, takes over (the other is told to pause), sends
    /// playback back ("play on"), pauses it; an unpaired device is turned away.
    #[test]
    fn take_over_between_two_devices() {
        let (cfg_a, _) = config("MacBook", Vec::new());
        let (cfg_b, _) = config("Desktop", Vec::new());
        let a = HandoffNode::start(cfg_a).unwrap();
        let b = HandoffNode::start(cfg_b).unwrap();
        a.set_paired(&[record(&b, "Desktop")]);
        b.set_paired(&[record(&a, "MacBook")]);

        // Nothing playing yet.
        a.query(&b.id());
        let status = wait_for(&a, |e| match e {
            Event::Status { result, .. } => Some(result),
            _ => None,
        });
        assert_eq!(status, Ok(None));

        // B plays; A asks, then takes over.
        b.set_snapshot(Some(Snapshot {
            queue: vec![track("aaaaaaaaaaa"), track("bbbbbbbbbbb")],
            current_index: 1,
            is_playing: true,
            position_ms: 60_000,
            repeat_mode: 2,
            ..Snapshot::default()
        }));
        a.query(&b.id());
        let (id, status) = wait_for(&a, |e| match e {
            Event::Status { id, result } => Some((id, result)),
            _ => None,
        });
        assert_eq!(id, b.id());
        let snapshot = status.unwrap().unwrap();
        assert_eq!(snapshot.device_name, "Desktop");
        assert_eq!(snapshot.current().unwrap().id, "bbbbbbbbbbb");
        assert!(snapshot.position_ms >= 60_000 && snapshot.last_played_unix_ms > 0);

        a.take_over(&b.id());
        let taken = wait_for(&a, |e| match e {
            Event::TookOver { snapshot, .. } => Some(snapshot),
            Event::Failed { error, .. } => panic!("{error}"),
            _ => None,
        });
        assert_eq!(taken.queue.len(), 2);
        assert_eq!(taken.repeat_mode, 2);
        assert!(taken.position_ms >= 60_000);
        let from = wait_for(&b, |e| match e {
            Event::PauseRequested { from } => Some(from),
            _ => None,
        });
        assert_eq!(from, a.id());

        // A sends it back: B is asked to play it, A hears it went through.
        a.play_on(&b.id(), taken.clone());
        wait_for(&a, |e| matches!(e, Event::Sent { .. }).then_some(()));
        let (from, played) = wait_for(&b, |e| match e {
            Event::PlayRequested { from, snapshot } => Some((from, snapshot)),
            _ => None,
        });
        assert_eq!(from, a.id());
        assert_eq!(played, taken);
        a.pause(&b.id());
        wait_for(&b, |e| {
            matches!(e, Event::PauseRequested { .. }).then_some(())
        });

        // The names / addresses each learnt arrive as updates (B learnt A's name was
        // already right; nothing else to check beyond "no errors").
        a.poll_events();

        // A third device B doesn't know: refused, whatever it asks.
        let (cfg_c, _) = config("Stranger", Vec::new());
        let c = HandoffNode::start(cfg_c).unwrap();
        c.set_paired(&[record(&b, "Desktop")]);
        c.take_over(&b.id());
        let error = wait_for(&c, |e| match e {
            Event::Failed { action, error, .. } => Some((action, error)),
            Event::TookOver { .. } => panic!("an unpaired device took over"),
            _ => None,
        });
        assert_eq!(error.0, Action::TakeOver);
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            b.poll_events()
                .iter()
                .all(|e| !matches!(e, Event::PauseRequested { .. })),
            "B paused for a stranger"
        );

        // Unpairing tells the other side.
        b.unpair(&a.id());
        let gone = wait_for(&a, |e| match e {
            Event::Unpaired { id } => Some(id),
            _ => None,
        });
        assert_eq!(gone, b.id());
    }

    /// Pairing with the code and link one device shows: both remember each other
    /// and can then talk; a wrong code fails without pairing, and the shower stops
    /// after too many tries.
    #[test]
    fn pairing_by_code_round_trip() {
        let (cfg_a, _) = config("MacBook", Vec::new());
        let (cfg_b, _) = config("Desktop", Vec::new());
        let a = HandoffNode::start(cfg_a).unwrap();
        let b = HandoffNode::start(cfg_b).unwrap();
        let localhost = |link: &PairingLink| PairingLink {
            addrs: record(&a, "").addrs,
            ..link.clone()
        };

        // Not showing a code: nothing to pair with.
        let offer = a.start_pairing();
        a.cancel_pairing();
        b.pair(PairInput::Link(localhost(&offer.link)));
        let error = wait_for(&b, |e| match e {
            Event::PairFailed(e) => Some(e),
            Event::Paired(_) => panic!("paired without a code shown"),
            _ => None,
        });
        assert_eq!(error, PairError::NotPairing);

        // A wrong code.
        let offer = a.start_pairing();
        assert_eq!(offer.code.len(), pairing::CODE_LEN);
        assert_eq!(offer.link.id, a.id());
        let mut wrong = localhost(&offer.link);
        wrong.code = if offer.code == "AAAAAAAA" {
            "BBBBBBBB".into()
        } else {
            "AAAAAAAA".into()
        };
        b.pair(PairInput::Link(wrong.clone()));
        let error = wait_for(&b, |e| match e {
            Event::PairFailed(e) => Some(e),
            Event::Paired(_) => panic!("paired with a wrong code"),
            _ => None,
        });
        assert_eq!(error, PairError::WrongCode);

        // The right one: both sides pair.
        b.pair(PairInput::parse(&localhost(&offer.link).to_link()).unwrap());
        let on_b = wait_for(&b, |e| match e {
            Event::Paired(d) => Some(d),
            Event::PairFailed(e) => panic!("{e:?}"),
            _ => None,
        });
        let on_a = wait_for(&a, |e| match e {
            Event::Paired(d) => Some(d),
            _ => None,
        });
        assert_eq!(on_b.id, a.id());
        assert_eq!(on_b.name, "MacBook");
        assert_eq!(on_a.id, b.id());
        assert_eq!(on_a.name, "Desktop");
        assert!(!on_a.addrs.is_empty());
        // The code is used up.
        b.pair(PairInput::Link(localhost(&offer.link)));
        assert_eq!(
            wait_for(&b, |e| match e {
                Event::PairFailed(e) => Some(e),
                _ => None,
            }),
            PairError::NotPairing
        );
        // Paired: they talk (with the addresses pairing gave; localhost here).
        a.set_paired(&[record(&b, &on_a.name)]);
        b.set_paired(&[record(&a, &on_b.name)]);
        a.query(&b.id());
        assert_eq!(
            wait_for(&a, |e| match e {
                Event::Status { result, .. } => Some(result),
                _ => None,
            }),
            Ok(None)
        );

        // Too many wrong codes: the shower gives up and says so.
        let (cfg_c, _) = config("Laptop", Vec::new());
        let c = HandoffNode::start(cfg_c).unwrap();
        let offer = a.start_pairing();
        let mut wrong = localhost(&offer.link);
        wrong.code = if offer.code == "AAAAAAAA" {
            "BBBBBBBB".into()
        } else {
            "AAAAAAAA".into()
        };
        for _ in 0..pairing::MAX_ATTEMPTS {
            c.pair(PairInput::Link(wrong.clone()));
            wait_for(&c, |e| match e {
                Event::PairFailed(e) => Some(e),
                _ => None,
            });
        }
        wait_for(&a, |e| matches!(e, Event::PairingEnded).then_some(()));
        c.pair(PairInput::Link(localhost(&offer.link)));
        assert_eq!(
            wait_for(&c, |e| match e {
                Event::PairFailed(e) => Some(e),
                Event::Paired(_) => panic!("paired after too many tries"),
                _ => None,
            }),
            PairError::NotPairing
        );
    }

    /// Through a relay only (no direct addresses known): run an `iroh-relay` (e.g.
    /// `iroh-relay --dev`, or `deploy/iroh-relay/`) and
    /// `MUSIC_TEST_IROH_RELAY=http://127.0.0.1:3340 cargo test -- --ignored via_a_relay`.
    #[test]
    #[ignore]
    fn take_over_via_a_relay() {
        let relay = std::env::var("MUSIC_TEST_IROH_RELAY").expect("MUSIC_TEST_IROH_RELAY");
        let (mut cfg_a, _) = config("A", Vec::new());
        let (mut cfg_b, _) = config("B", Vec::new());
        cfg_a.relay_url = Some(relay.clone());
        cfg_b.relay_url = Some(relay.clone());
        let a = HandoffNode::start(cfg_a).unwrap();
        let b = HandoffNode::start(cfg_b).unwrap();
        // Wait until both sit on the relay.
        let deadline = Instant::now() + Duration::from_secs(20);
        while a.reachable().0.is_none() || b.reachable().0.is_none() {
            assert!(Instant::now() < deadline, "not connected to the relay");
            std::thread::sleep(Duration::from_millis(50));
        }
        let only_relay = |node: &HandoffNode| PairedDevice {
            id: node.id(),
            name: "x".into(),
            relay_url: node.reachable().0,
            addrs: Vec::new(),
            paired_at_unix_ms: 1,
        };
        a.set_paired(&[only_relay(&b)]);
        b.set_paired(&[only_relay(&a)]);
        b.set_snapshot(Some(Snapshot {
            queue: vec![track("aaaaaaaaaaa")],
            is_playing: true,
            position_ms: 1_000,
            ..Snapshot::default()
        }));
        a.take_over(&b.id());
        let taken = wait_for(&a, |e| match e {
            Event::TookOver { snapshot, .. } => Some(snapshot),
            Event::Failed { error, .. } => panic!("{error}"),
            _ => None,
        });
        assert_eq!(taken.queue[0].id, "aaaaaaaaaaa");
        wait_for(&b, |e| {
            matches!(e, Event::PauseRequested { .. }).then_some(())
        });
    }

    #[test]
    fn a_bad_relay_url_is_an_error() {
        let (mut cfg, _) = config("X", Vec::new());
        cfg.relay_url = Some("not a url".into());
        assert!(HandoffNode::start(cfg).is_err());
    }
}
