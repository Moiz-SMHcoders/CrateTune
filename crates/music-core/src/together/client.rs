// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/together/
// TogetherClient.kt, © Rukamori and ArchiveTune contributors, GPL-3.0.
//! A guest's connection to a host: hello, heartbeats (clock sync), room states in,
//! control / add-track requests out; follows a host transfer by reconnecting to
//! the new host (same session, client id and name). One worker thread; the app
//! polls [`TogetherClient::poll_events`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::conn::{Conn, Link, POLL, Recv};
use super::link::JoinInfo;
use super::messages::{
    AddTrackMode, ControlAction, Message, PROTOCOL_VERSION, RoomSettings, RoomState,
    SuggestionState, TogetherTrack,
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
/// How long to keep trying to reach a new host after a transfer.
const RECONNECT_FOR: Duration = Duration::from_secs(15);

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
    /// What became of a song this guest suggested (hosts that support approvals).
    SuggestionStatus {
        track_id: String,
        state: SuggestionState,
        reason: Option<String>,
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
    /// The host wants this guest to take over the session. Start a server with
    /// these (same session id + key; [`super::HostOptions`] from the lists and
    /// the secret), then answer with [`TogetherClient::answer_host_offer`].
    HostOffer {
        /// The room as the host had it (queue, position, repeat, settings).
        state: Option<RoomState>,
        trusted_client_ids: Vec<String>,
        banned_client_ids: Vec<String>,
        relay_secret: Option<String>,
    },
    /// Someone else is the host now; the client is reconnecting to `join`.
    HostChanged {
        name: Option<String>,
        join: JoinInfo,
    },
    /// Connected (and said hello) to the new host after [`ClientEvent::HostChanged`].
    Reconnected,
}

enum Outgoing {
    Message(Message),
    Leave,
}

struct Shared {
    epoch: Instant,
    clock: Mutex<TogetherClock>,
    participant_id: Mutex<Option<String>>,
    /// Where the session is (changes with a host transfer).
    join: Mutex<JoinInfo>,
    shutdown: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Who this client is, for the hello (again after a host transfer).
struct Identity {
    display_name: String,
    client_id: String,
}

/// Connects and says hello.
fn open(join: &JoinInfo, identity: &Identity) -> Result<Conn> {
    let mut conn = Conn::connect(&join.ws_url(), CONNECT_TIMEOUT)?;
    let hello = Message::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        session_id: join.session_id.clone(),
        session_key: join.session_key.clone(),
        client_id: identity.client_id.clone(),
        display_name: identity.display_name.trim().to_owned(),
    };
    if !conn.send(&hello) {
        return Err(Error::Parse("connection closed during hello".into()));
    }
    conn.set_timeout(POLL);
    Ok(conn)
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
        let identity = Identity {
            display_name: display_name.to_owned(),
            client_id: client_id.to_owned(),
        };
        let conn = open(join, &identity)?;
        let shared = Arc::new(Shared {
            epoch: Instant::now(),
            clock: Mutex::new(TogetherClock::default()),
            participant_id: Mutex::new(None),
            join: Mutex::new(join.clone()),
            shutdown: AtomicBool::new(false),
        });
        let (out_tx, out_rx) = channel();
        let (ev_tx, ev_rx) = channel();
        let thread_shared = shared.clone();
        let session_id = join.session_id.clone();
        let thread_session = session_id.clone();
        let thread = std::thread::Builder::new()
            .name("together-client".into())
            .spawn(move || {
                run(
                    conn,
                    &thread_shared,
                    &thread_session,
                    &identity,
                    &out_rx,
                    &ev_tx,
                )
            })
            .map_err(Error::Io)?;
        Ok(Self {
            shared,
            session_id,
            outbox: out_tx,
            events: ev_rx,
            thread: Some(thread),
        })
    }

    /// Where the session is now (it moves with a host transfer).
    pub fn join_info(&self) -> JoinInfo {
        lock(&self.shared.join).clone()
    }

    /// Answers a [`ClientEvent::HostOffer`]: `Ok(Some((host, port)))` where guests
    /// reach this app's new server on the LAN, `Ok(None)` for a relayed session
    /// (registered with the relay already), `Err(why)` to refuse.
    pub fn answer_host_offer(
        &self,
        result: std::result::Result<Option<(String, u16)>, String>,
    ) -> bool {
        let session_id = self.session_id.clone();
        self.send(|participant_id| {
            let (host, port, error) = match result {
                Ok(Some((host, port))) => (Some(host), Some(port), None),
                Ok(None) => (None, None, None),
                Err(e) => (None, None, Some(e)),
            };
            Message::HostTransferReady {
                session_id,
                participant_id,
                host,
                port,
                error,
            }
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
        lock(&self.shared.clock).snapshot()
    }

    pub fn participant_id(&self) -> Option<String> {
        lock(&self.shared.participant_id).clone()
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
    identity: &Identity,
    outbox: &Receiver<Outgoing>,
    events: &Sender<ClientEvent>,
) {
    let elapsed = || shared.epoch.elapsed().as_millis() as u64;
    let emit = |e: ClientEvent| {
        let _ = events.send(e);
    };
    let mut next_ping = Instant::now();
    let mut ping_id = 0u64;
    // Fast pings again after reconnecting (a new host has a new clock).
    let mut fast_pings_from = 0u64;
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
            if let Some(pid) = lock(&shared.participant_id).clone() {
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
                + if ping_id < fast_pings_from + FAST_PINGS {
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
                        *lock(&shared.participant_id) = Some(participant_id.clone());
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
                    Message::SuggestionStatus {
                        track_id,
                        state,
                        reason,
                        ..
                    } => emit(ClientEvent::SuggestionStatus {
                        track_id,
                        state,
                        reason,
                    }),
                    Message::HeartbeatPong {
                        ping_id,
                        server_elapsed_realtime_ms,
                        ..
                    } => {
                        if let Some(sent) = pings.remove(&ping_id) {
                            lock(&shared.clock).on_pong(
                                sent,
                                elapsed(),
                                server_elapsed_realtime_ms,
                            );
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
                    Message::HostTransfer {
                        state,
                        trusted_client_ids,
                        banned_client_ids,
                        relay_secret,
                        ..
                    } => emit(ClientEvent::HostOffer {
                        state: state.map(|s| *s),
                        trusted_client_ids,
                        banned_client_ids,
                        relay_secret,
                    }),
                    Message::HostTransferred {
                        participant_id,
                        participant_name,
                        host,
                        port,
                        ..
                    } => {
                        if lock(&shared.participant_id).as_deref() == Some(participant_id.as_str())
                        {
                            // This app is the new host; it drops this client.
                            continue;
                        }
                        let old = lock(&shared.join).clone();
                        let join = match (host, port) {
                            (Some(host), Some(port)) if !host.is_empty() && port > 0 => JoinInfo {
                                host,
                                port,
                                url: None,
                                ..old
                            },
                            // Same relay: it routes the new connection to the new host.
                            _ if old.is_relay() => old,
                            _ => continue,
                        };
                        emit(ClientEvent::HostChanged {
                            name: participant_name,
                            join: join.clone(),
                        });
                        conn.close("Host moved");
                        *lock(&shared.participant_id) = None;
                        match reconnect(&join, identity, shared, &emit) {
                            Some(new) => {
                                conn = new;
                                *lock(&shared.join) = join;
                                *lock(&shared.clock) = TogetherClock::default();
                                pings.clear();
                                fast_pings_from = ping_id;
                                next_ping = Instant::now();
                                last_heard = Instant::now();
                                emit(ClientEvent::Reconnected);
                            }
                            None if shared.shutdown.load(Ordering::Relaxed) => break None,
                            None => break Some("Couldn't reach the new host".to_owned()),
                        }
                    }
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

/// Tries to reach a new host for [`RECONNECT_FOR`] (it may still be starting, a
/// relay may still count the old connection): connects, says hello and waits for
/// the welcome (recorded and emitted like the first one); refusals are retried.
fn reconnect(
    join: &JoinInfo,
    identity: &Identity,
    shared: &Shared,
    emit: &dyn Fn(ClientEvent),
) -> Option<Conn> {
    let deadline = Instant::now() + RECONNECT_FOR;
    while Instant::now() < deadline && !shared.shutdown.load(Ordering::Relaxed) {
        match open(join, identity) {
            Ok(mut conn) => {
                let wait_until = Instant::now() + CONNECT_TIMEOUT;
                while Instant::now() < wait_until {
                    match conn.recv() {
                        Recv::Message(Message::ServerWelcome {
                            session_id,
                            participant_id,
                            is_pending,
                            settings,
                            ..
                        }) if session_id == join.session_id => {
                            *lock(&shared.participant_id) = Some(participant_id.clone());
                            emit(ClientEvent::Welcome {
                                participant_id,
                                pending: is_pending,
                                settings,
                            });
                            return Some(conn);
                        }
                        Recv::Message(Message::ServerError { code, .. })
                            if code.as_deref() == Some("banned") =>
                        {
                            return None;
                        }
                        Recv::Message(Message::ServerError { .. }) | Recv::Closed(_) => break,
                        _ => {}
                    }
                }
                log::debug!("together: {} refused the reconnect", join.ws_url());
            }
            Err(e) => log::debug!("together: reconnecting to {}: {e}", join.ws_url()),
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    None
}
