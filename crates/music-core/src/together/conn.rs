//! A WebSocket over a blocking `TcpStream` (optionally TLS, for `wss://` relays)
//! with a short read timeout, so one thread can both read and write (poll-style),
//! without an async runtime; and [`Link`], what the per-guest server loop talks
//! to (a direct connection, or a guest tunnelled through a relay).

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use tungstenite::protocol::CloseFrame;
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::{Message as WsMessage, WebSocket};

use super::messages::Message;
use crate::{Error, Result};

/// Read timeout of the poll loop (how long a quiet `recv` blocks).
pub(super) const POLL: Duration = Duration::from_millis(40);

pub(super) enum Recv {
    Message(Message),
    /// Nothing arrived within [`POLL`] (or a non-text / undecodable frame).
    Idle,
    Closed(Option<String>),
}

/// A text frame as is (relay envelopes aren't [`Message`]s).
pub(super) enum RecvText {
    Text(String),
    Idle,
    Closed(Option<String>),
}

/// One side of a session, as the server's per-guest loop sees it.
pub(super) trait Link: Send {
    /// `false` once the connection is gone.
    fn send_text(&mut self, text: String) -> bool;
    fn recv(&mut self) -> Recv;
    fn close(&mut self, reason: &str);

    fn send(&mut self, message: &Message) -> bool {
        self.send_text(message.encode())
    }
}

/// Plain TCP, or TLS for `wss://` (client side only; the relay leaves TLS to a
/// reverse proxy).
pub(super) enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Stream {
    fn tcp(&self) -> &TcpStream {
        match self {
            Self::Plain(s) => s,
            Self::Tls(s) => &s.sock,
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(s) => s.read(buf),
            Self::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(s) => s.write(buf),
            Self::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(s) => s.flush(),
            Self::Tls(s) => s.flush(),
        }
    }
}

pub(super) struct Conn {
    ws: WebSocket<Stream>,
    closed: bool,
}

impl Conn {
    pub(super) fn new(ws: WebSocket<Stream>) -> Self {
        Self { ws, closed: false }
    }

    /// Opens a WebSocket to `url` (`ws://` or `wss://`), blocking up to `timeout`
    /// for the TCP connect and each handshake read.
    pub(super) fn connect(url: &str, timeout: Duration) -> Result<Self> {
        let parsed = url::Url::parse(url).map_err(|e| Error::Parse(format!("{url}: {e}")))?;
        let tls = match parsed.scheme() {
            "ws" => false,
            "wss" => true,
            other => return Err(Error::Parse(format!("not a WebSocket URL ({other}://)"))),
        };
        let host = parsed
            .host_str()
            .ok_or_else(|| Error::Parse(format!("no host in {url}")))?
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        let port = parsed
            .port_or_known_default()
            .unwrap_or(if tls { 443 } else { 80 });
        let addrs: Vec<_> = (host.as_str(), port)
            .to_socket_addrs()
            .map_err(|e| Error::Parse(format!("can't resolve {host}: {e}")))?
            .collect();
        let mut last_error = None;
        let mut stream = None;
        for addr in addrs {
            match TcpStream::connect_timeout(&addr, timeout) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(e) => last_error = Some(e),
            }
        }
        let tcp = stream.ok_or_else(|| match last_error {
            Some(e) if e.kind() == ErrorKind::ConnectionRefused => {
                Error::Parse("connection refused (is the session still running?)".into())
            }
            Some(e) => Error::Io(e),
            None => Error::Parse(format!("no address for {host}")),
        })?;
        tcp.set_nodelay(true)?;
        tcp.set_read_timeout(Some(timeout))?;
        tcp.set_write_timeout(Some(timeout))?;
        let stream = if tls {
            Stream::Tls(Box::new(rustls::StreamOwned::new(tls_client(&host)?, tcp)))
        } else {
            Stream::Plain(tcp)
        };
        let (ws, _) = tungstenite::client(url, stream)
            .map_err(|e| Error::Parse(format!("WebSocket handshake: {e}")))?;
        Ok(Self::new(ws))
    }

    pub(super) fn set_timeout(&mut self, timeout: Duration) {
        let _ = self.ws.get_ref().tcp().set_read_timeout(Some(timeout));
    }

    pub(super) fn recv_text(&mut self) -> RecvText {
        if self.closed {
            return RecvText::Closed(None);
        }
        match self.ws.read() {
            Ok(WsMessage::Text(text)) => RecvText::Text(text.as_str().to_owned()),
            Ok(WsMessage::Close(frame)) => {
                self.closed = true;
                let _ = self.ws.flush();
                RecvText::Closed(
                    frame
                        .map(|f| f.reason.as_str().to_owned())
                        .filter(|r| !r.is_empty()),
                )
            }
            Ok(_) => RecvText::Idle,
            Err(tungstenite::Error::Io(e)) if is_timeout(e.kind()) => {
                // Push out anything a previous write left buffered.
                let _ = self.ws.flush();
                RecvText::Idle
            }
            Err(e) => {
                self.closed = true;
                RecvText::Closed(match e {
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
}

impl Link for Conn {
    fn send_text(&mut self, text: String) -> bool {
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

    fn recv(&mut self) -> Recv {
        match self.recv_text() {
            RecvText::Text(text) => match Message::decode(&text) {
                Some(m) => Recv::Message(m),
                None => Recv::Idle,
            },
            RecvText::Idle => Recv::Idle,
            RecvText::Closed(reason) => Recv::Closed(reason),
        }
    }

    fn close(&mut self, reason: &str) {
        if self.closed {
            return;
        }
        self.closed = true;
        let _ = self.ws.close(Some(CloseFrame {
            code: CloseCode::Normal,
            reason: reason.to_owned().into(),
        }));
        // Wait (briefly) for the peer's close reply, discarding whatever it still
        // sends meanwhile. Dropping a socket with unread data makes Linux and
        // Windows reset the connection, and the peer then sees "Connection
        // lost" instead of this close frame and its reason.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        // Make sure the close frame itself went out (a write that timed out
        // leaves it queued inside tungstenite).
        while std::time::Instant::now() < deadline {
            match self.ws.flush() {
                Err(tungstenite::Error::Io(e)) if is_timeout(e.kind()) => continue,
                _ => break,
            }
        }
        while std::time::Instant::now() < deadline {
            match self.ws.read() {
                Ok(_) => continue,
                Err(tungstenite::Error::Io(e)) if is_timeout(e.kind()) => continue,
                _ => break,
            }
        }
        // Then a graceful TCP close: send FIN after the close frame and drain
        // until the peer closes too, so nothing unread is left to trigger a reset.
        let tcp = self.ws.get_ref().tcp();
        let _ = tcp.shutdown(std::net::Shutdown::Write);
        let mut sink = [0u8; 512];
        while std::time::Instant::now() < deadline {
            match (&*tcp).read(&mut sink) {
                Ok(0) => break,
                Ok(_) => continue,
                Err(e) if is_timeout(e.kind()) => continue,
                Err(_) => break,
            }
        }
    }
}

/// A rustls client for `host` with the Mozilla root set (ring provider).
fn tls_client(host: &str) -> Result<rustls::ClientConnection> {
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| Error::Parse(format!("TLS: {e}")))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.to_owned())
        .map_err(|e| Error::Parse(format!("TLS name {host}: {e}")))?;
    rustls::ClientConnection::new(Arc::new(config), name)
        .map_err(|e| Error::Parse(format!("TLS: {e}")))
}

fn is_timeout(kind: ErrorKind) -> bool {
    matches!(kind, ErrorKind::WouldBlock | ErrorKind::TimedOut)
}
