//! A WebSocket over a blocking `TcpStream` with a short read timeout, so one thread
//! can both read and write (poll-style), without an async runtime.

use std::io::ErrorKind;
use std::net::TcpStream;
use std::time::Duration;

use tungstenite::protocol::CloseFrame;
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::{Message as WsMessage, WebSocket};

use super::messages::Message;

/// Read timeout of the poll loop (how long a quiet `recv` blocks).
pub(super) const POLL: Duration = Duration::from_millis(40);

pub(super) enum Recv {
    Message(Message),
    /// Nothing arrived within [`POLL`] (or a non-text / undecodable frame).
    Idle,
    Closed(Option<String>),
}

pub(super) struct Conn {
    ws: WebSocket<TcpStream>,
    closed: bool,
}

impl Conn {
    pub(super) fn new(ws: WebSocket<TcpStream>) -> Self {
        Self { ws, closed: false }
    }

    pub(super) fn set_timeout(&mut self, timeout: Duration) {
        let _ = self.ws.get_mut().set_read_timeout(Some(timeout));
    }

    /// `false` once the connection is gone.
    pub(super) fn send(&mut self, message: &Message) -> bool {
        self.send_text(message.encode())
    }

    pub(super) fn send_text(&mut self, text: String) -> bool {
        if self.closed {
            return false;
        }
        match self.ws.send(WsMessage::text(text)) {
            Ok(()) => true,
            Err(tungstenite::Error::Io(e)) if is_timeout(e.kind()) => true,
            Err(_) => {
                self.closed = true;
                false
            }
        }
    }

    pub(super) fn recv(&mut self) -> Recv {
        if self.closed {
            return Recv::Closed(None);
        }
        match self.ws.read() {
            Ok(WsMessage::Text(text)) => match Message::decode(text.as_str()) {
                Some(m) => Recv::Message(m),
                None => Recv::Idle,
            },
            Ok(WsMessage::Close(frame)) => {
                self.closed = true;
                let _ = self.ws.flush();
                Recv::Closed(
                    frame
                        .map(|f| f.reason.as_str().to_owned())
                        .filter(|r| !r.is_empty()),
                )
            }
            Ok(_) => Recv::Idle,
            Err(tungstenite::Error::Io(e)) if is_timeout(e.kind()) => {
                // Push out anything a previous write left buffered.
                let _ = self.ws.flush();
                Recv::Idle
            }
            Err(e) => {
                self.closed = true;
                Recv::Closed(match e {
                    tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed => {
                        None
                    }
                    // A peer that vanished without a close frame (app quit, network gone).
                    tungstenite::Error::Protocol(_) | tungstenite::Error::Io(_) => {
                        Some("Connection lost".into())
                    }
                    other => Some(other.to_string()),
                })
            }
        }
    }

    pub(super) fn close(&mut self, reason: &str) {
        if self.closed {
            return;
        }
        self.closed = true;
        let _ = self.ws.close(Some(CloseFrame {
            code: CloseCode::Normal,
            reason: reason.to_owned().into(),
        }));
        // Give the close frame a moment to go out.
        for _ in 0..5 {
            match self.ws.read() {
                Err(tungstenite::Error::Io(e)) if is_timeout(e.kind()) => continue,
                _ => break,
            }
        }
    }
}

fn is_timeout(kind: ErrorKind) -> bool {
    matches!(kind, ErrorKind::WouldBlock | ErrorKind::TimedOut)
}
