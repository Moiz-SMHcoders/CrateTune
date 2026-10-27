// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/together/
// TogetherClient.kt, © Rukamori and ArchiveTune contributors, GPL-3.0.
//! A guest's connection to a host: hello, heartbeats (clock sync), room states in,
//! control / add-track requests out. One worker thread; the app polls
//! [`TogetherClient::poll_events`].

use std::collections::HashMap;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::conn::{Conn, POLL, Recv};
use super::link::JoinInfo;
use super::messages::{
    AddTrackMode, ControlAction, Message, PROTOCOL_VERSION, RoomSettings, RoomState, TogetherTrack,
};
use super::sync::{ClockSnapshot, TogetherClock};
use crate::{Error, Result};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(6);
/// Pings: quickly at first (to converge the clock), then every few seconds.
const FAST_PINGS: u64 = 5;
const FAST_PING_INTERVAL: Duration = Duration::from_millis(400);
const PING_INTERVAL: Duration = Duration::from_secs(3);
/// The host broadcasts twice a second; this much silence means it's gone.
const HOST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientEvent {
    Welcome {
        participant_id: String,
        pending: bool,
        settings: RoomSettings,
    },
    RoomState(RoomState),
    /// The host decided on a pending join.
    JoinDecision {
        approved: bool,
    },
    /// The host refused something (`code`: `not_allowed`, `invalid_session`, …).
    ServerError {
        message: String,
        code: Option<String>,
    },
    /// Removed by the host (`banned`: can't rejoin).
    Kicked {
        reason: String,
        banned: bool,
    },
    Disconnected {
        reason: Option<String>,
    },
}

enum Outgoing {
    Message(Message),
    Leave,
}

struct Shared {
    epoch: Instant,
    clock: Mutex<TogetherClock>,
    participant_id: Mutex<Option<String>>,
    shutdown: AtomicBool,
}

/// Dropping it disconnects (without a "leave" message; call [`Self::leave`] first).
pub struct TogetherClient {
    shared: Arc<Shared>,
    session_id: String,
    outbox: Sender<Outgoing>,
    events: Receiver<ClientEvent>,
    thread: Option<JoinHandle<()>>,
}

impl TogetherClient {
    /// Connects and says hello. Blocking (up to a few seconds); call off the UI thread.
    pub fn connect(join: &JoinInfo, display_name: &str, client_id: &str) -> Result<Self> {
        let addrs: Vec<_> = (join.host.as_str(), join.port)
            .to_socket_addrs()
            .map_err(|e| Error::Parse(format!("can't resolve {}: {e}", join.host)))?
            .collect();
        let mut last_error = None;
        let mut stream = None;
        for addr in addrs {
            match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(e) => last_error = Some(e),
            }
        }
        let stream = stream.ok_or_else(|| match last_error {
            Some(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                Error::Parse("connection refused (is the session still running?)".into())
            }
            Some(e) => Error::Io(e),
            None => Error::Parse(format!("no address for {}", join.host)),
        })?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(CONNECT_TIMEOUT))?;
        let (ws, _) = tungstenite::client(join.ws_url().as_str(), stream)
            .map_err(|e| Error::Parse(format!("WebSocket handshake: {e}")))?;
        let mut conn = Conn::new(ws);
        let hello = Message::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            session_id: join.session_id.clone(),
            session_key: join.session_key.clone(),
            client_id: client_id.to_owned(),
            display_name: display_name.trim().to_owned(),
        };
        if !conn.send(&hello) {
            return Err(Error::Parse("connection closed during hello".into()));
        }
        conn.set_timeout(POLL);
        let shared = Arc::new(Shared {
            epoch: Instant::now(),
            clock: Mutex::new(TogetherClock::default()),
            participant_id: Mutex::new(None),
            shutdown: AtomicBool::new(false),
        });
        let (out_tx, out_rx) = channel();
        let (ev_tx, ev_rx) = channel();
        let thread_shared = shared.clone();
        let session_id = join.session_id.clone();
        let thread_session = session_id.clone();
        let thread = std::thread::Builder::new()
            .name("together-client".into())
            .spawn(move || run(conn, &thread_shared, &thread_session, &out_rx, &ev_tx))
            .map_err(Error::Io)?;
        Ok(Self {
            shared,
            session_id,
            outbox: out_tx,
            events: ev_rx,
            thread: Some(thread),
        })
    }

    pub fn poll_events(&self) -> Vec<ClientEvent> {
        self.events.try_iter().collect()
    }

    /// This side's clock (ms since connecting), comparable through [`Self::clock`].
    pub fn elapsed_ms(&self) -> u64 {
        self.shared.epoch.elapsed().as_millis() as u64
    }

    pub fn clock(&self) -> ClockSnapshot {
        self.shared
            .clock
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot()
    }

    pub fn participant_id(&self) -> Option<String> {
        self.shared
            .participant_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn send(&self, make: impl FnOnce(String) -> Message) -> bool {
        let Some(pid) = self.participant_id() else {
            return false;
        };
        self.outbox.send(Outgoing::Message(make(pid))).is_ok()
    }

    pub fn request_control(&self, action: ControlAction) -> bool {
        let session_id = self.session_id.clone();
        self.send(|participant_id| Message::ControlRequest {
            session_id,
            participant_id,
            action,
        })
    }

    pub fn request_add_track(&self, track: TogetherTrack, mode: AddTrackMode) -> bool {
        let session_id = self.session_id.clone();
        self.send(|participant_id| Message::AddTrackRequest {
            session_id,
            participant_id,
            track,
            mode,
        })
    }

    /// Says goodbye and disconnects.
    pub fn leave(mut self) {
        let _ = self.outbox.send(Outgoing::Leave);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for TogetherClient {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(
    mut conn: Conn,
    shared: &Shared,
    session_id: &str,
    outbox: &Receiver<Outgoing>,
    events: &Sender<ClientEvent>,
) {
    let elapsed = || shared.epoch.elapsed().as_millis() as u64;
    let emit = |e: ClientEvent| {
        let _ = events.send(e);
    };
    let mut next_ping = Instant::now();
    let mut ping_id = 0u64;
    let mut pings: HashMap<u64, u64> = HashMap::new();
    let mut last_heard = Instant::now();
    let reason = loop {
        if shared.shutdown.load(Ordering::Relaxed) {
            conn.close("Disconnect");
            break None;
        }
        let mut leave = false;
        while let Ok(out) = outbox.try_recv() {
            match out {
                Outgoing::Message(m) => {
                    conn.send(&m);
                }
                Outgoing::Leave => {
                    leave = true;
                    break;
                }
            }
        }
        if leave {
            if let Some(pid) = shared
                .participant_id
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
            {
                conn.send(&Message::ClientLeave {
                    session_id: session_id.to_owned(),
                    participant_id: pid,
                });
            }
            conn.close("Left");
            break Some("Left the session".to_owned());
        }
        if Instant::now() >= next_ping {
            ping_id += 1;
            let now = elapsed();
            pings.insert(ping_id, now);
            pings.retain(|&id, _| id + 20 > ping_id);
            conn.send(&Message::HeartbeatPing {
                session_id: session_id.to_owned(),
                ping_id,
                client_elapsed_realtime_ms: now,
            });
            next_ping = Instant::now()
                + if ping_id < FAST_PINGS {
                    FAST_PING_INTERVAL
                } else {
                    PING_INTERVAL
                };
        }
        match conn.recv() {
            Recv::Message(message) => {
                last_heard = Instant::now();
                match message {
                    Message::ServerWelcome {
                        session_id: sid,
                        participant_id,
                        is_pending,
                        settings,
                        ..
                    } if sid == session_id => {
                        *shared
                            .participant_id
                            .lock()
                            .unwrap_or_else(|e| e.into_inner()) = Some(participant_id.clone());
                        emit(ClientEvent::Welcome {
                            participant_id,
                            pending: is_pending,
                            settings,
                        });
                    }
                    Message::RoomState { state } if state.session_id == session_id => {
                        emit(ClientEvent::RoomState(state));
                    }
                    Message::JoinDecision { approved, .. } => {
                        emit(ClientEvent::JoinDecision { approved });
                    }
                    Message::HeartbeatPong {
                        ping_id,
                        server_elapsed_realtime_ms,
                        ..
                    } => {
                        if let Some(sent) = pings.remove(&ping_id) {
                            shared
                                .clock
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .on_pong(sent, elapsed(), server_elapsed_realtime_ms);
                        }
                    }
                    Message::ServerError { message, code, .. } => {
                        emit(ClientEvent::ServerError { message, code });
                    }
                    // Only ever sent to the guest concerned.
                    Message::Kick { reason, .. } => emit(ClientEvent::Kicked {
                        reason: reason.unwrap_or_else(|| "Removed by the host".into()),
                        banned: false,
                    }),
                    Message::Ban { reason, .. } => emit(ClientEvent::Kicked {
                        reason: reason.unwrap_or_else(|| "Banned by the host".into()),
                        banned: true,
                    }),
                    _ => {}
                }
            }
            Recv::Idle => {
                if last_heard.elapsed() > HOST_TIMEOUT {
                    conn.close("Timed out");
                    break Some("The host stopped responding".into());
                }
            }
            Recv::Closed(reason) => break reason.or_else(|| Some("Disconnected".into())),
        }
    };
    emit(ClientEvent::Disconnected { reason });
}
