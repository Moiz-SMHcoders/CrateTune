//! A small self-hostable relay for the app's (CrateTune's) Music Together sessions
//! across networks.
//!
//! Guests speak the normal session protocol to the relay's URL; the relay checks
//! the first frame (`client_hello`: session id + key against the room's key hash)
//! and tunnels the connection to the room's host, which keeps one WebSocket to the
//! relay (`relay_host`) and runs the whole session itself (see
//! `music_core::together::relay` for the envelope format). The relay never reads
//! anything after the hello: it forwards text frames between host and guests.
//!
//! * Rooms are keyed by session id; a room lives while its host is connected. A
//!   second `relay_host` for the same session is accepted only with the room's
//!   secret (a host transfer): new guest connections then go to the new host,
//!   connections still open to the old host stay with it until it leaves.
//! * Limits ([`Config`]): rooms, guests per room, connections, message size,
//!   messages per second (guests over the limit are disconnected; a host's excess
//!   frames are dropped), handshake and idle timeouts.
//! * `GET /health` answers `{"status":"ok","rooms":…,"guests":…,"connections":…}`.
//! * Plain `ws://` only: put it behind a TLS reverse proxy (Caddy, nginx) for
//!   `wss://` (see the README).
//!
//! Std threads + `tungstenite`, one thread per connection; no async runtime.

use std::collections::HashMap;
use std::io::{ErrorKind, Read as _, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use music_net::TimedTcp;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tungstenite::{Message as WsMessage, WebSocket};

/// The app this relay serves (`APP_NAME` in `packaging/app.env`).
pub const APP_NAME: &str = env!("MUSIC_APP_NAME");
/// The session protocol version the relay understands (`relay_host.protocolVersion`).
pub const PROTOCOL_VERSION: u32 = 1;
pub const DEFAULT_PORT: u16 = 42118;
const POLL: Duration = Duration::from_millis(40);

#[derive(Debug, Clone)]
pub struct Config {
    pub listen: SocketAddr,
    pub max_rooms: usize,
    pub max_guests_per_room: usize,
    /// Open connections of any kind (hosts, guests, health checks).
    pub max_connections: usize,
    /// Largest text frame a guest may send (a host's envelopes may be twice that).
    pub max_message_bytes: usize,
    /// Sustained messages per second per guest (a host gets this per guest + 1).
    pub rate_per_sec: u32,
    /// Burst allowance on top of the rate.
    pub burst: u32,
    pub handshake_timeout: Duration,
    /// A connection that sends nothing for this long is closed (hosts send room
    /// states twice a second, guests ping every few seconds).
    pub idle_timeout: Duration,
    /// Log connections and rooms to stderr.
    pub verbose: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT)),
            max_rooms: 100,
            max_guests_per_room: 20,
            max_connections: 1_000,
            max_message_bytes: 1 << 20,
            rate_per_sec: 20,
            burst: 60,
            handshake_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(60),
            verbose: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Stats {
    pub rooms: usize,
    pub guests: usize,
    pub connections: usize,
}

/// Relay ↔ host envelopes (the same shape as `music_core::together::relay::RelayFrame`).
#[allow(clippy::enum_variant_names)] // the wire names
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum Frame {
    RelayHost {
        protocol_version: u32,
        session_id: String,
        key_hash: String,
        secret_hash: String,
    },
    RelayWelcome {
        session_id: String,
        max_guests: u32,
    },
    RelayError {
        message: String,
        code: Option<String>,
    },
    RelayOpen {
        conn: u64,
    },
    RelayData {
        conn: u64,
        data: String,
    },
    RelayClose {
        conn: u64,
        #[serde(default)]
        reason: Option<String>,
    },
}

impl Frame {
    fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// What the host thread of a room gets from its guests' threads.
enum HostIn {
    Open { conn: u64, guest: Sender<GuestIn> },
    Data { conn: u64, data: String },
    Closed { conn: u64, reason: Option<String> },
}

enum GuestIn {
    Text(String),
    Close(String),
}

struct Room {
    key_hash: String,
    secret_hash: String,
    /// The current host connection (new guests are bound to it).
    host_conn: u64,
    host: Sender<HostIn>,
    guests: usize,
}

struct Shared {
    config: Config,
    rooms: Mutex<HashMap<String, Room>>,
    connections: AtomicUsize,
    next_conn: AtomicUsize,
    shutdown: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    fn log(&self, line: impl FnOnce() -> String) {
        if self.config.verbose {
            eprintln!("music-relay: {}", line());
        }
    }

    fn stats(&self) -> Stats {
        let rooms = lock(&self.rooms);
        Stats {
            rooms: rooms.len(),
            guests: rooms.values().map(|r| r.guests).sum(),
            connections: self.connections.load(Ordering::Relaxed),
        }
    }
}

/// A running relay. Dropping it stops accepting and closes every connection.
pub struct Relay {
    shared: Arc<Shared>,
    addr: SocketAddr,
    accept: Option<JoinHandle<()>>,
}

impl Relay {
    pub fn start(config: Config) -> std::io::Result<Self> {
        let listener = TcpListener::bind(config.listen)?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared {
            config,
            rooms: Mutex::new(HashMap::new()),
            connections: AtomicUsize::new(0),
            next_conn: AtomicUsize::new(1),
            shutdown: AtomicBool::new(false),
        });
        let accept_shared = shared.clone();
        let accept = std::thread::Builder::new()
            .name("relay-accept".into())
            .spawn(move || accept_loop(&listener, &accept_shared))?;
        Ok(Self {
            shared,
            addr,
            accept: Some(accept),
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn stats(&self) -> Stats {
        self.shared.stats()
    }

    /// Blocks until [`Self::stop`] is called from another thread (for `main`).
    pub fn wait(mut self) {
        if let Some(handle) = self.accept.take() {
            let _ = handle.join();
        }
    }

    pub fn stop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        if let Some(handle) = self.accept.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stop();
    }
}

fn accept_loop(listener: &TcpListener, shared: &Arc<Shared>) {
    while !shared.shutdown.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, peer)) => {
                let open = shared.connections.fetch_add(1, Ordering::SeqCst) + 1;
                if open > shared.config.max_connections {
                    shared.connections.fetch_sub(1, Ordering::SeqCst);
                    let _ = stream.set_nonblocking(false);
                    let _ = http_reply(&stream, "503 Service Unavailable", "text/plain", "busy\n");
                    continue;
                }
                let conn_shared = shared.clone();
                let spawned =
                    std::thread::Builder::new()
                        .name("relay-conn".into())
                        .spawn(move || {
                            if let Err(e) = serve(stream, &conn_shared) {
                                conn_shared.log(|| format!("{peer}: {e}"));
                            }
                            conn_shared.connections.fetch_sub(1, Ordering::SeqCst);
                        });
                if spawned.is_err() {
                    shared.connections.fetch_sub(1, Ordering::SeqCst);
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}

fn http_reply(
    mut stream: impl Write,
    status: &str,
    content_type: &str,
    body: &str,
) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

/// Peeks at the request head without consuming it (the WebSocket handshake reads it again).
fn peek_head(stream: &TimedTcp, timeout: Duration) -> std::io::Result<(String, usize)> {
    let deadline = Instant::now() + timeout;
    let mut buf = vec![0u8; 8192];
    loop {
        let n = stream.peek(&mut buf)?;
        if n == 0 {
            return Err(ErrorKind::UnexpectedEof.into());
        }
        let head = &buf[..n];
        if head.windows(4).any(|w| w == b"\r\n\r\n") || n == buf.len() {
            return Ok((String::from_utf8_lossy(head).into_owned(), n));
        }
        if Instant::now() >= deadline {
            return Err(ErrorKind::TimedOut.into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn serve(stream: TcpStream, shared: &Shared) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    let mut stream = TimedTcp::new(stream)?;
    let timeout = shared.config.handshake_timeout;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let (head, head_len) = peek_head(&stream, timeout)?;
    let lower = head.to_ascii_lowercase();
    let path = head.split_whitespace().nth(1).unwrap_or("");
    if !lower.contains("upgrade: websocket") {
        // Consume the request (closing with unread bytes would reset the connection).
        let mut sink = vec![0u8; head_len];
        let _ = stream.read_exact(&mut sink);
        let _ = stream.shutdown(std::net::Shutdown::Read);
        return if head.starts_with("GET ") && matches!(path, "/health" | "/healthz") {
            let body = json!({ "status": "ok", "app": APP_NAME });
            let mut body = body.as_object().cloned().unwrap_or_default();
            if let Value::Object(stats) = serde_json::to_value(shared.stats()).unwrap_or_default() {
                body.extend(stats);
            }
            http_reply(
                &mut stream,
                "200 OK",
                "application/json",
                &Value::Object(body).to_string(),
            )
        } else {
            http_reply(
                &mut stream,
                "426 Upgrade Required",
                "text/plain",
                "music-relay: WebSocket only (GET /health for status)\n",
            )
        };
    }
    let max = shared.config.max_message_bytes;
    let ws_config = WebSocketConfig::default()
        .max_message_size(Some(max * 2 + 1024))
        .max_frame_size(Some(max * 2 + 1024));
    let ws = tungstenite::accept_with_config(stream, Some(ws_config))
        .map_err(|e| std::io::Error::other(format!("handshake: {e}")))?;
    let mut ws = Ws::new(ws);
    // The first frame says who this is.
    let first = match ws.recv() {
        Recv::Text(text) => text,
        _ => {
            ws.close("Handshake required");
            return Ok(());
        }
    };
    let _ = ws.ws.get_mut().set_read_timeout(Some(POLL));
    let conn = shared.next_conn.fetch_add(1, Ordering::SeqCst) as u64;
    let value: Value = serde_json::from_str(&first).unwrap_or_default();
    match value.get("type").and_then(Value::as_str) {
        Some("relay_host") => match serde_json::from_value::<Frame>(value) {
            Ok(Frame::RelayHost {
                protocol_version,
                session_id,
                key_hash,
                secret_hash,
            }) => serve_host(
                ws,
                conn,
                protocol_version,
                session_id,
                key_hash,
                secret_hash,
                shared,
            ),
            _ => ws.close("Bad request"),
        },
        Some("client_hello") => serve_guest(ws, conn, first, &value, shared),
        _ => ws.close("Handshake required"),
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn serve_host(
    mut ws: Ws,
    conn: u64,
    protocol_version: u32,
    session_id: String,
    key_hash: String,
    secret_hash: String,
    shared: &Shared,
) {
    let refuse = |ws: &mut Ws, message: &str, code: &str| {
        ws.send(
            Frame::RelayError {
                message: message.into(),
                code: Some(code.into()),
            }
            .encode(),
        );
        ws.close(message);
    };
    let valid = |s: &str| !s.is_empty() && s.len() <= 128;
    if protocol_version != PROTOCOL_VERSION {
        return refuse(&mut ws, "Unsupported protocol version", "protocol");
    }
    if !valid(&session_id) || key_hash.len() != 64 || secret_hash.len() != 64 {
        return refuse(&mut ws, "Bad request", "bad_request");
    }
    let (tx, inbox) = channel();
    {
        let mut rooms = lock(&shared.rooms);
        let rooms_len = rooms.len();
        match rooms.get_mut(&session_id) {
            Some(room) if room.secret_hash == secret_hash && room.key_hash == key_hash => {
                // A host transfer: new guests go to this connection from now on.
                room.host_conn = conn;
                room.host = tx;
            }
            Some(_) => {
                drop(rooms);
                return refuse(
                    &mut ws,
                    "That session is already hosted here",
                    "room_exists",
                );
            }
            None if rooms_len >= shared.config.max_rooms => {
                drop(rooms);
                return refuse(&mut ws, "The relay is full", "relay_full");
            }
            None => {
                rooms.insert(
                    session_id.clone(),
                    Room {
                        key_hash,
                        secret_hash,
                        host_conn: conn,
                        host: tx,
                        guests: 0,
                    },
                );
            }
        }
    }
    shared.log(|| format!("room {session_id}: host connected (#{conn})"));
    ws.send(
        Frame::RelayWelcome {
            session_id: session_id.clone(),
            max_guests: shared.config.max_guests_per_room as u32,
        }
        .encode(),
    );
    let (reason, guests) = host_loop(&mut ws, &inbox, shared);
    shared.log(|| format!("room {session_id}: host #{conn} left ({reason})"));
    // The room goes with its current host; after a transfer it stays with the new one.
    let mut rooms = lock(&shared.rooms);
    let current = rooms
        .get(&session_id)
        .is_some_and(|room| room.host_conn == conn);
    if current {
        rooms.remove(&session_id);
    }
    drop(rooms);
    let goodbye = if current {
        "Session ended"
    } else {
        "Host moved"
    };
    // Everyone still bound to this host goes (including guests that were just opening).
    for guest in guests.into_values() {
        let _ = guest.send(GuestIn::Close(goodbye.into()));
    }
    while let Ok(msg) = inbox.try_recv() {
        if let HostIn::Open { guest, .. } = msg {
            let _ = guest.send(GuestIn::Close(goodbye.into()));
        }
    }
}

/// The host's frames out to its guests and theirs in; returns why it ended and
/// the guests still connected.
fn host_loop(
    ws: &mut Ws,
    inbox: &Receiver<HostIn>,
    shared: &Shared,
) -> (String, HashMap<u64, Sender<GuestIn>>) {
    let mut guests: HashMap<u64, Sender<GuestIn>> = HashMap::new();
    let mut bucket = Bucket::new(shared.config.rate_per_sec, shared.config.burst);
    let mut last_heard = Instant::now();
    let reason = loop {
        if shared.shutdown.load(Ordering::Relaxed) {
            ws.close("Relay shutting down");
            break "Relay shutting down".to_owned();
        }
        while let Ok(msg) = inbox.try_recv() {
            let frame = match msg {
                HostIn::Open { conn, guest } => {
                    guests.insert(conn, guest);
                    Frame::RelayOpen { conn }
                }
                HostIn::Data { conn, data } => Frame::RelayData { conn, data },
                HostIn::Closed { conn, reason } => {
                    guests.remove(&conn);
                    Frame::RelayClose { conn, reason }
                }
            };
            ws.send(frame.encode());
        }
        match ws.recv() {
            Recv::Text(text) => {
                last_heard = Instant::now();
                let scale = guests.len() as u32 + 1;
                if !bucket.take(scale) {
                    continue; // over the rate: drop (room states repeat anyway)
                }
                match serde_json::from_str::<Frame>(&text) {
                    Ok(Frame::RelayData { conn, data })
                        if data.len() <= shared.config.max_message_bytes =>
                    {
                        if let Some(guest) = guests.get(&conn) {
                            let _ = guest.send(GuestIn::Text(data));
                        }
                    }
                    Ok(Frame::RelayClose { conn, reason }) => {
                        if let Some(guest) = guests.remove(&conn) {
                            let _ = guest.send(GuestIn::Close(reason.unwrap_or_default()));
                        }
                    }
                    _ => {}
                }
            }
            Recv::Idle => {
                if last_heard.elapsed() > shared.config.idle_timeout {
                    ws.close("Timed out");
                    break "timed out".to_owned();
                }
            }
            Recv::Closed(reason) => break reason.unwrap_or_else(|| "disconnected".into()),
        }
    };
    (reason, guests)
}

fn serve_guest(mut ws: Ws, conn: u64, hello: String, value: &Value, shared: &Shared) {
    let session_id = value
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let key = value
        .get("sessionKey")
        .and_then(Value::as_str)
        .unwrap_or("");
    let refuse = |ws: &mut Ws, message: &str, code: &str| {
        ws.send(
            json!({
                "type": "server_error",
                "sessionId": session_id,
                "message": message,
                "code": code,
            })
            .to_string(),
        );
        ws.close(message);
    };
    let (tx, inbox) = channel();
    let host = {
        let mut rooms = lock(&shared.rooms);
        match rooms.get_mut(&session_id) {
            Some(room) if room.key_hash == sha256_hex(key) => {
                if room.guests >= shared.config.max_guests_per_room {
                    Err(("The session is full", "room_full"))
                } else {
                    room.guests += 1;
                    Ok(room.host.clone())
                }
            }
            // Same answer for a wrong key and an unknown session (no probing).
            _ => Err(("Invalid session", "invalid_session")),
        }
    };
    let host = match host {
        Ok(host) => host,
        Err((message, code)) => return refuse(&mut ws, message, code),
    };
    if hello.len() > shared.config.max_message_bytes
        || host.send(HostIn::Open { conn, guest: tx }).is_err()
        || host.send(HostIn::Data { conn, data: hello }).is_err()
    {
        release_guest(shared, &session_id);
        return refuse(&mut ws, "Session ended", "ended");
    }
    shared.log(|| format!("room {session_id}: guest #{conn} connected"));
    let mut bucket = Bucket::new(shared.config.rate_per_sec, shared.config.burst);
    let mut last_heard = Instant::now();
    let reason: Option<String> = loop {
        let mut closing = None;
        while let Ok(msg) = inbox.try_recv() {
            match msg {
                GuestIn::Text(text) => {
                    ws.send(text);
                }
                GuestIn::Close(reason) => {
                    closing = Some(reason);
                    break;
                }
            }
        }
        if let Some(reason) = closing {
            ws.close(if reason.is_empty() {
                "Session ended"
            } else {
                &reason
            });
            break None; // the host knows
        }
        if shared.shutdown.load(Ordering::Relaxed) {
            ws.close("Relay shutting down");
            break Some("Relay shutting down".into());
        }
        match ws.recv() {
            Recv::Text(text) => {
                last_heard = Instant::now();
                if text.len() > shared.config.max_message_bytes {
                    ws.close("Message too large");
                    break Some("Message too large".into());
                }
                if !bucket.take(1) {
                    ws.close("Too many messages");
                    break Some("Too many messages".into());
                }
                if host.send(HostIn::Data { conn, data: text }).is_err() {
                    ws.close("Session ended");
                    break None;
                }
            }
            Recv::Idle => {
                if last_heard.elapsed() > shared.config.idle_timeout {
                    ws.close("Timed out");
                    break Some("Timed out".into());
                }
            }
            Recv::Closed(reason) => break Some(reason.unwrap_or_else(|| "Disconnected".into())),
        }
    };
    if let Some(reason) = reason {
        let _ = host.send(HostIn::Closed {
            conn,
            reason: Some(reason),
        });
    }
    release_guest(shared, &session_id);
    shared.log(|| format!("room {session_id}: guest #{conn} left"));
}

fn release_guest(shared: &Shared, session_id: &str) {
    if let Some(room) = lock(&shared.rooms).get_mut(session_id) {
        room.guests = room.guests.saturating_sub(1);
    }
}

/// Token bucket: `rate` per second, `burst` extra; `take(n)` scales both by `n`.
struct Bucket {
    rate: f64,
    burst: f64,
    tokens: f64,
    last: Instant,
}

impl Bucket {
    fn new(rate: u32, burst: u32) -> Self {
        Self {
            rate: f64::from(rate.max(1)),
            burst: f64::from(burst.max(1)),
            tokens: f64::from(burst.max(1)),
            last: Instant::now(),
        }
    }

    fn take(&mut self, scale: u32) -> bool {
        let scale = f64::from(scale.max(1));
        let now = Instant::now();
        let cap = self.burst * scale;
        self.tokens = (self.tokens
            + now.duration_since(self.last).as_secs_f64() * self.rate * scale)
            .min(cap);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

enum Recv {
    Text(String),
    Idle,
    Closed(Option<String>),
}

struct Ws {
    ws: WebSocket<TimedTcp>,
    closed: bool,
}

impl Ws {
    fn new(ws: WebSocket<TimedTcp>) -> Self {
        Self { ws, closed: false }
    }

    fn send(&mut self, text: String) -> bool {
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
        if self.closed {
            return Recv::Closed(None);
        }
        match self.ws.read() {
            Ok(WsMessage::Text(text)) => Recv::Text(text.as_str().to_owned()),
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
                let _ = self.ws.flush();
                Recv::Idle
            }
            Err(_) => {
                self.closed = true;
                Recv::Closed(Some("Connection lost".into()))
            }
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
