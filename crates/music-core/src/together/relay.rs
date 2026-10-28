//! The host's side of a relayed session (see the `music-relay` crate).
//!
//! The relay knows rooms only by session id + a hash of the key. The host keeps
//! one WebSocket to it and every guest is tunnelled through that connection as a
//! numbered virtual connection: the relay announces it (`relay_open`), forwards
//! its text frames verbatim (`relay_data`) and its end (`relay_close`). All the
//! session logic — key check, approval, heartbeats, requests — still runs in the
//! host's [`super::TogetherServer`], exactly as for a guest on the LAN; guests
//! can't tell the difference (they speak the normal protocol to the relay URL).
//!
//! A host transfer hands the room's secret to the new host, which claims the room
//! with it: guests that reconnect afterwards are routed to the new host, while
//! connections still open to the old host stay with it until it leaves.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::conn::{Conn, Link, POLL, Recv, RecvText};
use super::messages::{Message, PROTOCOL_VERSION};
use super::server::{Inner, ServerEvent, serve_link};
use crate::{Error, Result};

/// Relay ↔ host envelopes (one JSON object per text frame, like the session protocol).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum RelayFrame {
    /// Host → relay, first frame: open (or, with the room's secret, take over) a room.
    RelayHost {
        protocol_version: u32,
        session_id: String,
        /// Lowercase hex SHA-256 of the session key; guests' keys are checked against it.
        key_hash: String,
        /// Lowercase hex SHA-256 of the room secret (needed to take the room over).
        secret_hash: String,
    },
    RelayWelcome {
        session_id: String,
        #[serde(default)]
        max_guests: u32,
    },
    RelayError {
        message: String,
        #[serde(default)]
        code: Option<String>,
    },
    /// A guest connected (and sent a `client_hello` for this room).
    RelayOpen { conn: u64 },
    /// A text frame from / for guest `conn`.
    RelayData { conn: u64, data: String },
    /// Guest `conn` is gone (relay → host) / close it (host → relay).
    RelayClose {
        conn: u64,
        #[serde(default)]
        reason: Option<String>,
    },
}

impl RelayFrame {
    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn decode(text: &str) -> Option<Self> {
        serde_json::from_str(text).ok()
    }
}

/// Lowercase hex SHA-256 (how keys and secrets are presented to the relay).
pub fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(super) enum RelayOut {
    Data { conn: u64, text: String },
    Close { conn: u64, reason: String },
}

enum LinkIn {
    Text(String),
    Closed(Option<String>),
}

/// A guest behind the relay, as the server's per-guest loop sees it.
struct VirtualLink {
    conn: u64,
    incoming: Receiver<LinkIn>,
    out: Sender<RelayOut>,
    closed: bool,
}

impl Link for VirtualLink {
    fn send_text(&mut self, text: String) -> bool {
        !self.closed
            && self
                .out
                .send(RelayOut::Data {
                    conn: self.conn,
                    text,
                })
                .is_ok()
    }

    fn recv(&mut self) -> Recv {
        if self.closed {
            return Recv::Closed(None);
        }
        match self.incoming.recv_timeout(POLL) {
            Ok(LinkIn::Text(text)) => Message::decode(&text).map_or(Recv::Idle, Recv::Message),
            Ok(LinkIn::Closed(reason)) => {
                self.closed = true;
                Recv::Closed(reason)
            }
            Err(RecvTimeoutError::Timeout) => Recv::Idle,
            Err(RecvTimeoutError::Disconnected) => {
                self.closed = true;
                Recv::Closed(Some("Relay connection lost".into()))
            }
        }
    }

    fn close(&mut self, reason: &str) {
        if !self.closed {
            self.closed = true;
            let _ = self.out.send(RelayOut::Close {
                conn: self.conn,
                reason: reason.to_owned(),
            });
        }
    }
}

/// Connects to the relay and registers the room; returns the connection once
/// the relay welcomed it.
pub(super) fn register(
    url: &str,
    session_id: &str,
    session_key: &str,
    secret: &str,
    timeout: Duration,
) -> Result<Conn> {
    let mut conn = Conn::connect(url, timeout)?;
    let hello = RelayFrame::RelayHost {
        protocol_version: PROTOCOL_VERSION,
        session_id: session_id.to_owned(),
        key_hash: sha256_hex(session_key),
        secret_hash: sha256_hex(secret),
    };
    if !conn.send_text(hello.encode()) {
        return Err(Error::Parse("relay closed the connection".into()));
    }
    conn.set_timeout(POLL);
    let deadline = Instant::now() + timeout;
    loop {
        match conn.recv_text() {
            RecvText::Text(text) => match RelayFrame::decode(&text) {
                Some(RelayFrame::RelayWelcome {
                    session_id: sid, ..
                }) if sid == session_id => {
                    return Ok(conn);
                }
                Some(RelayFrame::RelayError { message, .. }) => {
                    return Err(Error::Parse(format!("relay: {message}")));
                }
                _ => {}
            },
            RecvText::Idle => {}
            RecvText::Closed(reason) => {
                return Err(Error::Parse(format!(
                    "relay closed the connection{}",
                    reason.map(|r| format!(": {r}")).unwrap_or_default()
                )));
            }
        }
        if Instant::now() >= deadline {
            return Err(Error::Parse("the relay did not answer".into()));
        }
    }
}

/// The relay connection's thread: spawns a guest loop per virtual connection and
/// moves frames between them and the relay.
pub(super) fn run(mut conn: Conn, inner: Arc<Inner>) {
    let (out_tx, out_rx) = channel::<RelayOut>();
    let mut links: HashMap<u64, Sender<LinkIn>> = HashMap::new();
    let mut shutdown_at: Option<Instant> = None;
    loop {
        while let Ok(out) = out_rx.try_recv() {
            let frame = match out {
                RelayOut::Data { conn, text } => RelayFrame::RelayData { conn, data: text },
                RelayOut::Close { conn: id, reason } => {
                    links.remove(&id);
                    RelayFrame::RelayClose {
                        conn: id,
                        reason: Some(reason),
                    }
                }
            };
            conn.send_text(frame.encode());
        }
        if inner.shutdown.load(Ordering::Relaxed) {
            // Let the guest loops send their goodbyes, then hang up.
            let at = *shutdown_at.get_or_insert_with(Instant::now);
            if links.is_empty() || at.elapsed() > Duration::from_millis(400) {
                conn.close("Session ended");
                return;
            }
        }
        match conn.recv_text() {
            RecvText::Text(text) => match RelayFrame::decode(&text) {
                Some(RelayFrame::RelayOpen { conn: id }) => {
                    let (tx, rx) = channel();
                    links.insert(id, tx);
                    let link = VirtualLink {
                        conn: id,
                        incoming: rx,
                        out: out_tx.clone(),
                        closed: false,
                    };
                    let inner = inner.clone();
                    let _ = std::thread::Builder::new()
                        .name("together-guest".into())
                        .spawn(move || {
                            if let Err(e) = serve_link(link, &inner) {
                                inner.emit(ServerEvent::Error(format!("relay guest {id}: {e}")));
                            }
                        });
                }
                Some(RelayFrame::RelayData { conn: id, data }) => {
                    if let Some(tx) = links.get(&id) {
                        let _ = tx.send(LinkIn::Text(data));
                    }
                }
                Some(RelayFrame::RelayClose { conn: id, reason }) => {
                    if let Some(tx) = links.remove(&id) {
                        let _ = tx.send(LinkIn::Closed(reason));
                    }
                }
                Some(RelayFrame::RelayError { message, .. }) => {
                    inner.emit(ServerEvent::Error(format!("relay: {message}")));
                }
                _ => {}
            },
            RecvText::Idle => {}
            RecvText::Closed(reason) => {
                for (_, tx) in links.drain() {
                    let _ = tx.send(LinkIn::Closed(Some("Relay connection lost".into())));
                }
                if !inner.shutdown.load(Ordering::Relaxed) {
                    inner.emit(ServerEvent::Stopped {
                        reason: format!(
                            "Lost the connection to the relay{}",
                            reason.map(|r| format!(" ({r})")).unwrap_or_default()
                        ),
                    });
                }
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_wire_format() {
        let frame = RelayFrame::RelayData {
            conn: 7,
            data: r#"{"type":"heartbeat_ping"}"#.into(),
        };
        assert_eq!(
            frame.encode(),
            r#"{"type":"relay_data","conn":7,"data":"{\"type\":\"heartbeat_ping\"}"}"#
        );
        assert_eq!(RelayFrame::decode(&frame.encode()), Some(frame));
        let host = RelayFrame::RelayHost {
            protocol_version: 1,
            session_id: "s".into(),
            key_hash: sha256_hex("k"),
            secret_hash: sha256_hex("x"),
        };
        assert!(host.encode().starts_with(
            r#"{"type":"relay_host","protocolVersion":1,"sessionId":"s","keyHash":"8254c329"#
        ));
        assert_eq!(
            RelayFrame::decode(r#"{"type":"relay_close","conn":3}"#),
            Some(RelayFrame::RelayClose {
                conn: 3,
                reason: None
            })
        );
    }
}
