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
//! * Room codes (see [`codes`]): the host registers its live room with
//!   `POST /v1/together/sessions` (session id, guest key, host key — the room's
//!   secret, checked against its hash) and gets a short code; a guest trades the
//!   code for the session id, guest key and WebSocket path with
//!   `POST /v1/together/sessions/resolve` (rate-limited per address). The code
//!   goes with the room. The host key is never stored or returned.
//! * `GET /health` answers `{"status":"ok","rooms":…,"guests":…,"connections":…}`.
//! * `GET /player-fixes.json` and `/player-fixes.json.minisig` serve the app's
//!   signed player fixes from [`Config::player_fixes_dir`] (a mirror of
//!   `packaging/` in the app's repository; the app checks the signature, so the
//!   relay can't alter them).
//! * Plain `ws://` only: put it behind a TLS reverse proxy (Caddy, nginx) for
//!   `wss://` (see the README).
//!
//! Std threads + `tungstenite`, one thread per connection; no async runtime.

pub mod codes;

use std::collections::HashMap;
use std::io::{ErrorKind, Read as _, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
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
/// Largest request body the code endpoints read.
const MAX_BODY: usize = 4 * 1024;
/// Where guests connect, relative to the relay's base URL (any path works).
const WS_PATH: &str = "/";

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
    /// Code look-ups (`/v1/together/sessions/resolve`) per minute per address.
    pub resolve_per_minute: u32,
    /// Reverse proxies whose `X-Forwarded-For` names the client (for the rate
    /// limit); requests from anywhere else count against their own address.
    pub trusted_proxies: Vec<IpAddr>,
    /// Log connections and rooms to stderr.
    pub verbose: bool,
    /// Where `player-fixes.json` and its `.minisig` are served from (`None`: not served).
    pub player_fixes_dir: Option<std::path::PathBuf>,
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
            resolve_per_minute: 10,
            trusted_proxies: Vec::new(),
            verbose: false,
            player_fixes_dir: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Stats {
    pub rooms: usize,
    pub guests: usize,
    pub connections: usize,
    /// Rooms with a code.
    pub codes: usize,
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
    /// The room's short code, once the host asked for one.
    code: Option<String>,
    /// The guest key, given out for the code (it never sees the host key).
    guest_key: Option<String>,
}

struct Shared {
    config: Config,
    /// Locked before `codes` when both are needed.
    rooms: Mutex<HashMap<String, Room>>,
    /// Code → session id, for the live rooms that have one.
    codes: Mutex<HashMap<String, String>>,
    limiter: Mutex<codes::RateLimiter>,
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
            codes: lock(&self.codes).len(),
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
            limiter: Mutex::new(codes::RateLimiter::new(config.resolve_per_minute)),
            config,
            rooms: Mutex::new(HashMap::new()),
            codes: Mutex::new(HashMap::new()),
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
                            if let Err(e) = serve(stream, peer.ip(), &conn_shared) {
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
    stream: impl Write,
    status: &str,
    content_type: &str,
    body: &str,
) -> std::io::Result<()> {
    http_reply_with(stream, status, content_type, "", body)
}

/// `extra`: more header lines, each ending in `\r\n`.
fn http_reply_with(
    mut stream: impl Write,
    status: &str,
    content_type: &str,
    extra: &str,
    body: &str,
) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n{extra}\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

/// Peeks at the request head without consuming it (the WebSocket handshake reads it again).
fn peek_head(stream: &TimedTcp, timeout: Duration) -> std::io::Result<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    let mut buf = vec![0u8; 8192];
    loop {
        let n = stream.peek(&mut buf)?;
        if n == 0 {
            return Err(ErrorKind::UnexpectedEof.into());
        }
        let head = &buf[..n];
        if head.windows(4).any(|w| w == b"\r\n\r\n") || n == buf.len() {
            buf.truncate(n);
            return Ok(buf);
        }
        if Instant::now() >= deadline {
            return Err(ErrorKind::TimedOut.into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn serve(stream: TcpStream, peer: IpAddr, shared: &Shared) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    let mut stream = TimedTcp::new(stream)?;
    let timeout = shared.config.handshake_timeout;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let head = peek_head(&stream, timeout)?;
    let head_end = header_end(&head).unwrap_or(head.len());
    let lower = String::from_utf8_lossy(&head[..head_end]).to_ascii_lowercase();
    if !lower.contains("upgrade: websocket") {
        return serve_http(stream, &head, peer, shared);
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

/// Index just past the blank line that ends the request head.
fn header_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 4)
}

/// A plain HTTP request: health check, room codes, or a pointer to the WebSocket.
fn serve_http(
    mut stream: TimedTcp,
    peeked: &[u8],
    peer: IpAddr,
    shared: &Shared,
) -> std::io::Result<()> {
    let Some(head_len) = header_end(peeked) else {
        // No end of the head in 8 KiB.
        let mut sink = vec![0u8; peeked.len()];
        let _ = stream.read_exact(&mut sink);
        let _ = stream.shutdown(std::net::Shutdown::Read);
        return http_reply(
            &mut stream,
            "431 Request Header Fields Too Large",
            "text/plain",
            "music-relay: request head too large\n",
        );
    };
    // Consume the request (closing with unread bytes would reset the connection).
    let mut head = vec![0u8; head_len];
    stream.read_exact(&mut head)?;
    let head = String::from_utf8_lossy(&head).into_owned();
    let mut lines = head.split("\r\n");
    let mut request = lines.next().unwrap_or("").split_whitespace();
    let method = request.next().unwrap_or("").to_owned();
    let target = request.next().unwrap_or("");
    let path = target.split(['?', '#']).next().unwrap_or("");
    let header = |name: &str| {
        head.split("\r\n").skip(1).find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
    };
    let length = header("content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    if length > MAX_BODY {
        // Drain some of it, so the answer isn't lost to a reset.
        let mut sink = vec![0u8; length.min(64 * 1024)];
        let _ = stream.read_exact(&mut sink);
        let _ = stream.shutdown(std::net::Shutdown::Read);
        return reply_json(
            &mut stream,
            "413 Payload Too Large",
            &error_body("Request too large", "too_large"),
            "",
        );
    }
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body)?;
    let _ = stream.shutdown(std::net::Shutdown::Read);
    let api = path.trim_end_matches('/');
    let resolve = api.ends_with("/v1/together/sessions/resolve");
    let create = api.ends_with("/v1/together/sessions");
    if (resolve || create) && method != "POST" {
        return reply_json(
            &mut stream,
            "405 Method Not Allowed",
            &error_body("Use POST", "method"),
            "Allow: POST\r\n",
        );
    }
    if resolve {
        let client = client_ip(peer, header("x-forwarded-for").as_deref(), shared);
        let (status, body, extra) = resolve_code(&body, client, shared);
        return reply_json(&mut stream, status, &body, &extra);
    }
    if create {
        let (status, body) = create_code(&body, shared);
        return reply_json(&mut stream, status, &body, "");
    }
    if method == "GET" && matches!(path, "/player-fixes.json" | "/player-fixes.json.minisig") {
        return serve_player_fixes(&mut stream, path, shared);
    }
    if method == "GET" && matches!(path, "/health" | "/healthz") {
        let body = json!({ "status": "ok", "app": APP_NAME });
        let mut body = body.as_object().cloned().unwrap_or_default();
        if let Value::Object(stats) = serde_json::to_value(shared.stats()).unwrap_or_default() {
            body.extend(stats);
        }
        return http_reply(
            &mut stream,
            "200 OK",
            "application/json",
            &Value::Object(body).to_string(),
        );
    }
    http_reply(
        &mut stream,
        "426 Upgrade Required",
        "text/plain",
        "music-relay: WebSocket only (GET /health for status)\n",
    )
}

/// The mirrored player-fixes manifest or its signature (at most 64 KiB, as is).
fn serve_player_fixes(stream: impl Write, path: &str, shared: &Shared) -> std::io::Result<()> {
    let file = shared
        .config
        .player_fixes_dir
        .as_ref()
        .map(|dir| dir.join(path.trim_start_matches('/')))
        .and_then(|file| std::fs::read_to_string(file).ok())
        .filter(|text| text.len() <= 64 * 1024);
    let content_type = if path.ends_with(".json") {
        "application/json"
    } else {
        "text/plain; charset=utf-8"
    };
    match file {
        Some(body) => http_reply(stream, "200 OK", content_type, &body),
        None => http_reply(
            stream,
            "404 Not Found",
            "text/plain",
            "music-relay: no player fixes here\n",
        ),
    }
}

fn reply_json(stream: impl Write, status: &str, body: &Value, extra: &str) -> std::io::Result<()> {
    http_reply_with(stream, status, "application/json", extra, &body.to_string())
}

fn error_body(message: &str, code: &str) -> Value {
    json!({ "ok": false, "error": message, "code": code })
}

/// The address a request counts against: the peer, or, behind a trusted reverse
/// proxy, the last address it put into `X-Forwarded-For`.
fn client_ip(peer: IpAddr, forwarded: Option<&str>, shared: &Shared) -> IpAddr {
    let canonical = |ip: IpAddr| match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    };
    let peer = canonical(peer);
    if !shared
        .config
        .trusted_proxies
        .iter()
        .any(|p| canonical(*p) == peer)
    {
        return peer;
    }
    forwarded
        .and_then(|list| list.rsplit(',').next())
        .map(str::trim)
        .and_then(|last| {
            last.parse::<IpAddr>()
                .ok()
                .or_else(|| last.parse::<SocketAddr>().ok().map(|a| a.ip()))
        })
        .map_or(peer, canonical)
}

fn json_field(body: &Value, name: &str) -> Option<String> {
    body.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .map(str::to_owned)
}

/// `POST /v1/together/sessions` `{sessionId, guestKey, hostKey}` from the room's
/// host (its connection is up): the room's code, made on first ask.
fn create_code(body: &[u8], shared: &Shared) -> (&'static str, Value) {
    let body: Value = serde_json::from_slice(body).unwrap_or_default();
    let (Some(session_id), Some(guest_key), Some(host_key)) = (
        json_field(&body, "sessionId"),
        json_field(&body, "guestKey"),
        json_field(&body, "hostKey"),
    ) else {
        return (
            "400 Bad Request",
            error_body("sessionId, guestKey and hostKey are needed", "bad_request"),
        );
    };
    let mut rooms = lock(&shared.rooms);
    let Some(room) = rooms.get_mut(&session_id).filter(|room| {
        room.key_hash == sha256_hex(&guest_key) && room.secret_hash == sha256_hex(&host_key)
    }) else {
        // Same answer for an unknown session and wrong keys (no probing).
        return (
            "403 Forbidden",
            error_body("Invalid session", "invalid_session"),
        );
    };
    let code = match &room.code {
        Some(code) => code.clone(),
        None => {
            let mut codes = lock(&shared.codes);
            let code = loop {
                let code = codes::new_code();
                if !codes.contains_key(&code) {
                    break code;
                }
            };
            codes.insert(code.clone(), session_id.clone());
            room.code = Some(code.clone());
            shared.log(|| format!("room {session_id}: has a code now"));
            code
        }
    };
    room.guest_key = Some(guest_key);
    (
        "200 OK",
        json!({ "ok": true, "sessionId": session_id, "code": code, "wsUrl": WS_PATH }),
    )
}

/// `POST /v1/together/sessions/resolve` `{code}`: what a guest needs to join.
fn resolve_code(body: &[u8], client: IpAddr, shared: &Shared) -> (&'static str, Value, String) {
    if let Err(wait) = lock(&shared.limiter).check(client, Instant::now()) {
        let secs = wait.as_secs_f64().ceil().max(1.0) as u64;
        return (
            "429 Too Many Requests",
            error_body("Too many attempts, try again later", "rate_limited"),
            format!("Retry-After: {secs}\r\n"),
        );
    }
    let body: Value = serde_json::from_slice(body).unwrap_or_default();
    let Some(code) = body
        .get("code")
        .and_then(Value::as_str)
        .and_then(codes::normalize_code)
    else {
        return (
            "400 Bad Request",
            error_body("Not a room code", "bad_code"),
            String::new(),
        );
    };
    let session_id = lock(&shared.codes).get(&code).cloned();
    let found = session_id.and_then(|sid| {
        let rooms = lock(&shared.rooms);
        let key = rooms.get(&sid)?.guest_key.clone()?;
        Some((sid, key))
    });
    match found {
        Some((session_id, guest_key)) => {
            shared.log(|| format!("room {session_id}: code resolved"));
            (
                "200 OK",
                json!({
                    "ok": true,
                    "sessionId": session_id,
                    "guestKey": guest_key,
                    "wsUrl": WS_PATH,
                }),
                String::new(),
            )
        }
        None => (
            "404 Not Found",
            error_body("No session with that code", "not_found"),
            String::new(),
        ),
    }
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
                        code: None,
                        guest_key: None,
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
    if current
        && let Some(room) = rooms.remove(&session_id)
        && let Some(code) = room.code
    {
        // The code is free again (another room may draw it).
        lock(&shared.codes).remove(&code);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn shared(trusted: &[&str]) -> Shared {
        let config = Config {
            trusted_proxies: trusted.iter().map(|p| p.parse().unwrap()).collect(),
            ..Config::default()
        };
        Shared {
            limiter: Mutex::new(codes::RateLimiter::new(config.resolve_per_minute)),
            config,
            rooms: Mutex::new(HashMap::new()),
            codes: Mutex::new(HashMap::new()),
            connections: AtomicUsize::new(0),
            next_conn: AtomicUsize::new(1),
            shutdown: AtomicBool::new(false),
        }
    }

    #[test]
    fn client_address_behind_a_trusted_proxy() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let open = shared(&[]);
        // Nobody trusted: the header is ignored (anyone could send it).
        assert_eq!(
            client_ip(ip("203.0.113.5"), Some("198.51.100.1"), &open),
            ip("203.0.113.5")
        );
        let proxied = shared(&["192.168.0.52"]);
        // The proxy's last entry is the address that connected to it.
        assert_eq!(
            client_ip(ip("192.168.0.52"), Some("10.9.9.9, 198.51.100.1"), &proxied),
            ip("198.51.100.1")
        );
        assert_eq!(
            client_ip(ip("::ffff:192.168.0.52"), Some("2001:db8::7"), &proxied),
            ip("2001:db8::7")
        );
        assert_eq!(
            client_ip(ip("192.168.0.52"), Some("198.51.100.1:5555"), &proxied),
            ip("198.51.100.1")
        );
        // No (or a broken) header: the proxy itself.
        assert_eq!(
            client_ip(ip("192.168.0.52"), None, &proxied),
            ip("192.168.0.52")
        );
        assert_eq!(
            client_ip(ip("192.168.0.52"), Some("garbage"), &proxied),
            ip("192.168.0.52")
        );
        // Someone else sending the header: their own address.
        assert_eq!(
            client_ip(ip("203.0.113.5"), Some("198.51.100.1"), &proxied),
            ip("203.0.113.5")
        );
    }
}
