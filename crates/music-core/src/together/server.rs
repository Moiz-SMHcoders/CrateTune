// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/together/
// TogetherServer.kt, © Rukamori and ArchiveTune contributors, GPL-3.0.
//! The host's session server: accepts guests (on the LAN, or tunnelled through a
//! relay, see [`super::relay`]), checks the session key, keeps the participant
//! list, relays guests' requests to the host app as [`ServerEvent`]s, fans the
//! host's [`RoomState`] out to everyone, and hands the session over to a guest
//! ([`TogetherServer::transfer_host`]).
//!
//! One accept (or relay) thread plus one thread per guest (std threads, `tungstenite`).

use std::collections::HashSet;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::conn::{Conn, Link, POLL, Recv, Stream};
use super::link::JoinInfo;
use super::messages::{
    AddTrackMode, ControlAction, Message, PROTOCOL_VERSION, Participant, Role, RoomSettings,
    RoomState, TogetherTrack, queue_hash,
};
use crate::{Error, Result};

pub const HOST_ID: &str = "host";
/// A guest that sent nothing (not even a heartbeat) for this long is dropped.
const GUEST_TIMEOUT: Duration = Duration::from_secs(20);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the guest asked to take over has to answer.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(15);
const RELAY_TIMEOUT: Duration = Duration::from_secs(8);

/// Extras for starting a session ([`TogetherServer::start_with`] /
/// [`TogetherServer::start_relay`]); a host transfer fills them in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostOptions {
    /// Client ids let in without approval (the guests of the previous host).
    pub trusted_client_ids: Vec<String>,
    /// Client ids that can't join.
    pub banned_client_ids: Vec<String>,
    /// Relayed sessions: the room's secret on the relay (a new host claims the room
    /// with it). Generated when `None`.
    pub relay_secret: Option<String>,
}

/// What guests did, for the host app to act on (drained with [`TogetherServer::poll_events`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerEvent {
    /// Waiting for [`TogetherServer::approve`] (the room requires approval).
    JoinRequested(Participant),
    Joined(Participant),
    Left {
        participant: Participant,
        reason: String,
    },
    /// Already checked against the room settings.
    Control {
        participant: Participant,
        action: ControlAction,
    },
    AddTrack {
        participant: Participant,
        track: TogetherTrack,
        mode: AddTrackMode,
    },
    /// The guest asked to take over is ready and everyone has been told to
    /// reconnect to `join`: stop this server (and join `join` as a guest).
    HostTransferred {
        participant: Participant,
        join: JoinInfo,
    },
    /// A transfer didn't happen (the guest left, refused, or didn't answer).
    TransferFailed {
        participant: Option<Participant>,
        reason: String,
    },
    /// The session can't go on (a relayed session lost its relay).
    Stopped {
        reason: String,
    },
    Error(String),
}

struct PendingTransfer {
    participant_id: String,
    deadline: Instant,
}

enum Outgoing {
    Text(String),
    Close(String),
}

struct Client {
    participant_id: String,
    client_id: String,
    name: String,
    pending: bool,
    outbox: Sender<Outgoing>,
}

impl Client {
    fn participant(&self) -> Participant {
        Participant {
            id: self.participant_id.clone(),
            name: self.name.clone(),
            is_host: false,
            is_pending: self.pending,
            is_connected: true,
        }
    }
}

pub(super) struct Inner {
    session_id: String,
    session_key: String,
    host_name: Mutex<String>,
    epoch: Instant,
    settings: Mutex<RoomSettings>,
    /// In join order.
    clients: Mutex<Vec<Client>>,
    banned: Mutex<HashSet<String>>,
    trusted: Mutex<HashSet<String>>,
    /// Relayed sessions: (relay URL, room secret).
    relay: Option<(String, String)>,
    transfer: Mutex<Option<PendingTransfer>>,
    events: Mutex<Sender<ServerEvent>>,
    pub(super) shutdown: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Inner {
    pub(super) fn emit(&self, event: ServerEvent) {
        let _ = lock(&self.events).send(event);
    }

    fn elapsed_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    fn with_client<T>(&self, id: &str, f: impl FnOnce(&mut Client) -> T) -> Option<T> {
        lock(&self.clients)
            .iter_mut()
            .find(|c| c.participant_id == id)
            .map(f)
    }

    fn remove(&self, id: &str) -> Option<Client> {
        let mut clients = lock(&self.clients);
        let ix = clients.iter().position(|c| c.participant_id == id)?;
        Some(clients.remove(ix))
    }
}

/// A running session. Dropping it stops the server and disconnects everyone.
pub struct TogetherServer {
    inner: Arc<Inner>,
    events: Receiver<ServerEvent>,
    /// `None` for a relayed session.
    addr: Option<SocketAddr>,
    accept: Option<JoinHandle<()>>,
}

impl TogetherServer {
    /// Listens on all interfaces at `port` (0 = any free port).
    pub fn start(
        port: u16,
        session_id: String,
        session_key: String,
        host_name: String,
        settings: RoomSettings,
    ) -> Result<Self> {
        Self::start_with(
            port,
            session_id,
            session_key,
            host_name,
            settings,
            HostOptions::default(),
        )
    }

    /// [`Self::start`] with [`HostOptions`] (e.g. taking over a session).
    pub fn start_with(
        port: u16,
        session_id: String,
        session_key: String,
        host_name: String,
        settings: RoomSettings,
        options: HostOptions,
    ) -> Result<Self> {
        let listener = TcpListener::bind(("0.0.0.0", port))?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let (inner, rx) = Inner::new(session_id, session_key, host_name, settings, options, None);
        let accept_inner = inner.clone();
        let accept = std::thread::Builder::new()
            .name("together-accept".into())
            .spawn(move || accept_loop(listener, accept_inner))
            .map_err(Error::Io)?;
        Ok(Self {
            inner,
            events: rx,
            addr: Some(addr),
            accept: Some(accept),
        })
    }

    /// Hosts through a relay server (`ws://` / `wss://` URL, see
    /// [`super::relay_url`]): registers the room there and serves the guests the
    /// relay tunnels in. Blocking (connect + registration, a few seconds at most).
    pub fn start_relay(
        relay_url: &str,
        session_id: String,
        session_key: String,
        host_name: String,
        settings: RoomSettings,
        options: HostOptions,
    ) -> Result<Self> {
        let url = super::link::relay_url(relay_url)
            .ok_or_else(|| Error::Parse(format!("not a relay address: {relay_url}")))?;
        let secret = options.relay_secret.clone().unwrap_or_else(super::new_id);
        let conn = super::relay::register(&url, &session_id, &session_key, &secret, RELAY_TIMEOUT)?;
        let (inner, rx) = Inner::new(
            session_id,
            session_key,
            host_name,
            settings,
            options,
            Some((url, secret)),
        );
        let relay_inner = inner.clone();
        let thread = std::thread::Builder::new()
            .name("together-relay".into())
            .spawn(move || super::relay::run(conn, relay_inner))
            .map_err(Error::Io)?;
        Ok(Self {
            inner,
            events: rx,
            addr: None,
            accept: Some(thread),
        })
    }

    /// The relay this session runs through (normalized `ws(s)://` URL).
    pub fn relay_url(&self) -> Option<&str> {
        self.inner.relay.as_ref().map(|(url, _)| url.as_str())
    }

    /// The listening port (0 for a relayed session).
    pub fn port(&self) -> u16 {
        self.addr.map_or(0, |a| a.port())
    }

    pub fn session_id(&self) -> &str {
        &self.inner.session_id
    }

    pub fn session_key(&self) -> &str {
        &self.inner.session_key
    }

    /// The server clock (ms since start) that stamps room states and answers pings.
    pub fn elapsed_ms(&self) -> u64 {
        self.inner.elapsed_ms()
    }

    pub fn settings(&self) -> RoomSettings {
        *lock(&self.inner.settings)
    }

    pub fn update_settings(&self, settings: RoomSettings) {
        *lock(&self.inner.settings) = settings;
    }

    pub fn set_host_name(&self, name: &str) {
        *lock(&self.inner.host_name) = name.to_owned();
    }

    pub fn poll_events(&self) -> Vec<ServerEvent> {
        let mut events: Vec<ServerEvent> = self.events.try_iter().collect();
        let mut transfer = lock(&self.inner.transfer);
        if transfer
            .as_ref()
            .is_some_and(|t| Instant::now() >= t.deadline)
        {
            let t = transfer.take().expect("checked");
            events.push(ServerEvent::TransferFailed {
                participant: self
                    .inner
                    .with_client(&t.participant_id, |c| c.participant()),
                reason: "No answer".into(),
            });
        }
        events
    }

    /// Whether a host transfer is waiting for the guest's answer.
    pub fn transfer_pending(&self) -> bool {
        lock(&self.inner.transfer).is_some()
    }

    /// Hands the session to a (connected, admitted) guest: sends it `state` (the
    /// room as this host has it), the guests that may rejoin without approval
    /// (everyone admitted now, plus `own_client_id` so this host can come back as a
    /// guest), the banned list and, for a relayed session, the room's secret. Once
    /// the guest's session is up, every guest is told to reconnect and
    /// [`ServerEvent::HostTransferred`] follows (or [`ServerEvent::TransferFailed`]).
    pub fn transfer_host(
        &self,
        participant_id: &str,
        state: RoomState,
        own_client_id: Option<&str>,
    ) -> bool {
        let mut trusted: Vec<String> = lock(&self.inner.trusted).iter().cloned().collect();
        let target = {
            let clients = lock(&self.inner.clients);
            trusted.extend(
                clients
                    .iter()
                    .filter(|c| !c.pending)
                    .map(|c| c.client_id.clone()),
            );
            clients
                .iter()
                .find(|c| c.participant_id == participant_id && !c.pending)
                .map(|c| c.outbox.clone())
        };
        let Some(outbox) = target else {
            return false;
        };
        trusted.extend(own_client_id.map(str::to_owned));
        trusted.sort();
        trusted.dedup();
        let message = Message::HostTransfer {
            session_id: self.inner.session_id.clone(),
            participant_id: participant_id.to_owned(),
            state: Some(Box::new(self.stamp(state))),
            trusted_client_ids: trusted,
            banned_client_ids: lock(&self.inner.banned).iter().cloned().collect(),
            relay_secret: self.inner.relay.as_ref().map(|(_, s)| s.clone()),
        };
        if outbox.send(Outgoing::Text(message.encode())).is_err() {
            return false;
        }
        *lock(&self.inner.transfer) = Some(PendingTransfer {
            participant_id: participant_id.to_owned(),
            deadline: Instant::now() + TRANSFER_TIMEOUT,
        });
        true
    }

    /// Host first, then guests in join order.
    pub fn participants(&self) -> Vec<Participant> {
        let mut list = vec![Participant {
            id: HOST_ID.into(),
            name: lock(&self.inner.host_name).clone(),
            is_host: true,
            is_pending: false,
            is_connected: true,
        }];
        list.extend(lock(&self.inner.clients).iter().map(Client::participant));
        list
    }

    /// Fills in session, host, participants, settings, queue hash and the send time,
    /// and sends `state` to every guest (pending guests get it without the queue).
    pub fn broadcast_room_state(&self, state: RoomState) -> RoomState {
        let state = self.stamp(state);
        let full = Message::RoomState {
            state: state.clone(),
        }
        .encode();
        let redacted = Message::RoomState {
            state: RoomState {
                queue: Vec::new(),
                queue_hash: String::new(),
                current_index: 0,
                is_playing: false,
                position_ms: 0,
                ..state.clone()
            },
        }
        .encode();
        for client in lock(&self.inner.clients).iter() {
            let text = if client.pending { &redacted } else { &full };
            let _ = client.outbox.send(Outgoing::Text(text.clone()));
        }
        state
    }

    fn stamp(&self, mut state: RoomState) -> RoomState {
        state.session_id = self.inner.session_id.clone();
        state.host_id = HOST_ID.into();
        state.participants = self.participants();
        state.settings = self.settings();
        state.queue_hash = queue_hash(&state.queue);
        state.sent_at_elapsed_realtime_ms = self.elapsed_ms();
        state
    }

    /// Lets a pending guest in (or turns them away).
    pub fn approve(&self, participant_id: &str, approved: bool) {
        let decision = Message::JoinDecision {
            session_id: self.inner.session_id.clone(),
            participant_id: participant_id.to_owned(),
            approved,
        }
        .encode();
        if approved {
            let joined = self.inner.with_client(participant_id, |c| {
                if !c.pending {
                    return None;
                }
                c.pending = false;
                let _ = c.outbox.send(Outgoing::Text(decision));
                Some(c.participant())
            });
            if let Some(Some(p)) = joined {
                self.inner.emit(ServerEvent::Joined(p));
            }
        } else if let Some(client) = self.inner.remove(participant_id) {
            let _ = client.outbox.send(Outgoing::Text(decision));
            let _ = client.outbox.send(Outgoing::Close("Not approved".into()));
            self.inner.emit(ServerEvent::Left {
                participant: client.participant(),
                reason: "Not approved".into(),
            });
        }
    }

    /// Disconnects a guest (`ban`: their client id can't rejoin this session).
    pub fn kick(&self, participant_id: &str, reason: Option<&str>, ban: bool) {
        let Some(client) = self.inner.remove(participant_id) else {
            return;
        };
        let (session_id, participant_id) =
            (self.inner.session_id.clone(), participant_id.to_owned());
        let reason_text = reason.map(str::to_owned);
        let message = if ban {
            lock(&self.inner.banned).insert(client.client_id.clone());
            Message::Ban {
                session_id,
                participant_id,
                reason: reason_text,
            }
        } else {
            Message::Kick {
                session_id,
                participant_id,
                reason: reason_text,
            }
        };
        let _ = client.outbox.send(Outgoing::Text(message.encode()));
        let label = if ban { "Banned" } else { "Removed by the host" };
        let _ = client.outbox.send(Outgoing::Close(label.into()));
        self.inner.emit(ServerEvent::Left {
            participant: client.participant(),
            reason: label.into(),
        });
    }

    /// Ends the session: tells every guest and closes the listener.
    pub fn stop(&mut self) {
        if self.inner.shutdown.swap(true, Ordering::SeqCst) {
            return;
        }
        for client in lock(&self.inner.clients).drain(..) {
            let _ = client.outbox.send(Outgoing::Close("Session ended".into()));
        }
        if let Some(handle) = self.accept.take() {
            let _ = handle.join();
        }
    }
}

impl Inner {
    fn new(
        session_id: String,
        session_key: String,
        host_name: String,
        settings: RoomSettings,
        options: HostOptions,
        relay: Option<(String, String)>,
    ) -> (Arc<Self>, Receiver<ServerEvent>) {
        let (tx, rx) = channel();
        let inner = Arc::new(Self {
            session_id,
            session_key,
            host_name: Mutex::new(host_name),
            epoch: Instant::now(),
            settings: Mutex::new(settings),
            clients: Mutex::new(Vec::new()),
            banned: Mutex::new(options.banned_client_ids.into_iter().collect()),
            trusted: Mutex::new(options.trusted_client_ids.into_iter().collect()),
            relay,
            transfer: Mutex::new(None),
            events: Mutex::new(tx),
            shutdown: AtomicBool::new(false),
        });
        (inner, rx)
    }
}

impl Drop for TogetherServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn accept_loop(listener: TcpListener, inner: Arc<Inner>) {
    while !inner.shutdown.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, peer)) => {
                let inner = inner.clone();
                let _ = std::thread::Builder::new()
                    .name("together-guest".into())
                    .spawn(move || {
                        if let Err(e) = serve_guest(stream, &inner) {
                            inner.emit(ServerEvent::Error(format!("{peer}: {e}")));
                        }
                    });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                inner.emit(ServerEvent::Error(format!("accept: {e}")));
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    }
}

fn serve_guest(stream: TcpStream, inner: &Inner) -> Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    let ws = tungstenite::accept(Stream::Plain(stream))
        .map_err(|e| Error::Parse(format!("handshake: {e}")))?;
    let mut conn = Conn::new(ws);
    // The hello must come within the handshake timeout; then poll.
    let hello = conn.recv();
    conn.set_timeout(POLL);
    serve_link_from(conn, hello, inner)
}

/// One guest from its `client_hello` to the end, over any [`Link`] (a relayed
/// guest's link already polls with [`POLL`]).
pub(super) fn serve_link(mut link: impl Link, inner: &Inner) -> Result<()> {
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    let hello = loop {
        match link.recv() {
            Recv::Idle if Instant::now() < deadline => continue,
            other => break other,
        }
    };
    serve_link_from(link, hello, inner)
}

fn serve_link_from<L: Link>(mut conn: L, hello: Recv, inner: &Inner) -> Result<()> {
    let hello = match hello {
        Recv::Message(m @ Message::ClientHello { .. }) => m,
        _ => {
            conn.close("Handshake required");
            return Ok(());
        }
    };
    let Message::ClientHello {
        protocol_version,
        session_id,
        session_key,
        client_id,
        display_name,
    } = hello
    else {
        unreachable!()
    };
    let refuse = |conn: &mut L, message: &str, code: &str| {
        conn.send(&Message::ServerError {
            session_id: Some(session_id.clone()),
            message: message.into(),
            code: Some(code.into()),
        });
        conn.close(message);
    };
    if protocol_version != PROTOCOL_VERSION {
        refuse(&mut conn, "Unsupported protocol version", "protocol");
        return Ok(());
    }
    if session_id != inner.session_id || session_key != inner.session_key {
        refuse(&mut conn, "Invalid session", "invalid_session");
        return Ok(());
    }
    if lock(&inner.banned).contains(&client_id) {
        refuse(&mut conn, "You can't rejoin this session", "banned");
        return Ok(());
    }
    if inner.shutdown.load(Ordering::Relaxed) {
        refuse(&mut conn, "Session ended", "ended");
        return Ok(());
    }

    let participant_id = super::new_id();
    let settings = *lock(&inner.settings);
    let pending =
        settings.require_host_approval_to_join && !lock(&inner.trusted).contains(&client_id);
    let name: String = display_name.trim().chars().take(40).collect();
    let (tx, outbox) = channel();
    let client = Client {
        participant_id: participant_id.clone(),
        client_id,
        name: if name.is_empty() {
            "Guest".into()
        } else {
            name
        },
        pending,
        outbox: tx,
    };
    let participant = client.participant();
    lock(&inner.clients).push(client);
    conn.send(&Message::ServerWelcome {
        protocol_version: PROTOCOL_VERSION,
        session_id: inner.session_id.clone(),
        participant_id: participant_id.clone(),
        role: Role::Guest,
        is_pending: pending,
        settings,
    });
    inner.emit(if pending {
        ServerEvent::JoinRequested(participant.clone())
    } else {
        ServerEvent::Joined(participant.clone())
    });

    let mut last_heard = Instant::now();
    let reason = loop {
        let mut closing = None;
        while let Ok(out) = outbox.try_recv() {
            match out {
                Outgoing::Text(text) => {
                    conn.send_text(text);
                }
                Outgoing::Close(reason) => {
                    closing = Some(reason);
                    break;
                }
            }
        }
        if let Some(reason) = closing {
            conn.close(&reason);
            // Already removed by whoever closed it (kick / deny / stop).
            return Ok(());
        }
        if inner.shutdown.load(Ordering::Relaxed) {
            conn.close("Session ended");
            return Ok(());
        }
        match conn.recv() {
            Recv::Message(message) => {
                last_heard = Instant::now();
                if let Some(reason) = handle(inner, &mut conn, &participant_id, message) {
                    break reason;
                }
            }
            Recv::Idle => {
                if last_heard.elapsed() > GUEST_TIMEOUT {
                    conn.close("Timed out");
                    break "Timed out".to_owned();
                }
            }
            Recv::Closed(reason) => break reason.unwrap_or_else(|| "Disconnected".into()),
        }
    };
    if let Some(client) = inner.remove(&participant_id) {
        let participant = client.participant();
        let mut transfer = lock(&inner.transfer);
        if transfer
            .as_ref()
            .is_some_and(|t| t.participant_id == participant_id)
        {
            *transfer = None;
            inner.emit(ServerEvent::TransferFailed {
                participant: Some(participant.clone()),
                reason: "They left".into(),
            });
        }
        drop(transfer);
        inner.emit(ServerEvent::Left {
            participant,
            reason,
        });
    }
    Ok(())
}

/// One message from a guest; `Some(reason)` ends the connection.
fn handle(
    inner: &Inner,
    conn: &mut impl Link,
    participant_id: &str,
    message: Message,
) -> Option<String> {
    let Some((participant, pending)) =
        inner.with_client(participant_id, |c| (c.participant(), c.pending))
    else {
        return Some("Removed".into());
    };
    let settings = *lock(&inner.settings);
    let denied = |conn: &mut dyn Link, what: &str| {
        conn.send(&Message::ServerError {
            session_id: Some(inner.session_id.clone()),
            message: format!("The host doesn't allow guests to {what}"),
            code: Some("not_allowed".into()),
        });
    };
    match message {
        Message::HeartbeatPing {
            ping_id,
            client_elapsed_realtime_ms,
            ..
        } => {
            conn.send(&Message::HeartbeatPong {
                session_id: inner.session_id.clone(),
                ping_id,
                client_elapsed_realtime_ms,
                server_elapsed_realtime_ms: inner.elapsed_ms(),
            });
        }
        Message::ControlRequest { action, .. } if !pending => {
            if settings.allow_guests_to_control_playback {
                inner.emit(ServerEvent::Control {
                    participant,
                    action,
                });
            } else {
                denied(conn, "control playback");
            }
        }
        Message::AddTrackRequest { track, mode, .. } if !pending => {
            if !settings.allow_guests_to_add_tracks {
                denied(conn, "add songs");
            } else if super::is_shareable_id(&track.id) {
                inner.emit(ServerEvent::AddTrack {
                    participant,
                    track,
                    mode,
                });
            }
        }
        Message::ClientLeave { .. } => {
            conn.close("Left");
            return Some("Left".into());
        }
        Message::HostTransferReady {
            host, port, error, ..
        } => {
            let mut transfer = lock(&inner.transfer);
            if !transfer
                .as_ref()
                .is_some_and(|t| t.participant_id == participant_id)
            {
                return None;
            }
            *transfer = None;
            drop(transfer);
            let join = match (&inner.relay, host, port) {
                _ if error.is_some() => None,
                (Some((url, _)), _, _) => {
                    JoinInfo::via_relay(url, inner.session_id.clone(), inner.session_key.clone())
                }
                (None, Some(host), Some(port)) if !host.is_empty() && port > 0 => Some(JoinInfo {
                    host,
                    port,
                    session_id: inner.session_id.clone(),
                    session_key: inner.session_key.clone(),
                    url: None,
                }),
                _ => None,
            };
            let Some(join) = join else {
                inner.emit(ServerEvent::TransferFailed {
                    participant: Some(participant),
                    reason: error.unwrap_or_else(|| "No address to reconnect to".into()),
                });
                return None;
            };
            let moved = Message::HostTransferred {
                session_id: inner.session_id.clone(),
                participant_id: participant_id.to_owned(),
                participant_name: Some(participant.name.clone()),
                host: (!join.is_relay()).then(|| join.host.clone()),
                port: (!join.is_relay()).then_some(join.port),
            }
            .encode();
            for client in lock(&inner.clients).iter() {
                let _ = client.outbox.send(Outgoing::Text(moved.clone()));
            }
            inner.emit(ServerEvent::HostTransferred { participant, join });
        }
        _ => {}
    }
    None
}
