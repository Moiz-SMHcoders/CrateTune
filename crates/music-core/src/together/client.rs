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
    AddTrackMode, ControlAction, FEATURE_SESSION, FEATURES, IntentMeta, Message, NoteKind,
    PROTOCOL_VERSION, QueueOp, RoomSettings, RoomState, SessionMode, Successor, SuggestionState,
    TogetherTrack,
};
use super::server::HOST_LEFT;
use super::session::Backoff;
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
/// Between attempts at a new host (it may still be starting).
const TRANSFER_RETRY: Duration = Duration::from_millis(300);

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
        /// The host is gone without ending the session (the connection dropped, it
        /// stopped answering, or it quit a Friends session): a roster's successors may
        /// carry the session on ([`super::session::failover`]).
        lost: bool,
    },
    /// Who takes a Friends session over (empty: nobody, it ends with the host).
    Roster {
        successors: Vec<Successor>,
        trusted_client_ids: Vec<String>,
        banned_client_ids: Vec<String>,
        relay_secret: Option<String>,
    },
    /// Someone else reacted to the current song.
    Reaction {
        participant_id: String,
        name: String,
        emoji: String,
        track_id: Option<String>,
    },
    /// Someone did something ("Ana skipped").
    Note {
        participant_id: String,
        name: String,
        note: NoteKind,
        count: u32,
        title: Option<String>,
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
    /// The link to the host dropped (`reason`); reconnecting for a while
    /// ([`ClientOptions::reconnect_for`]). [`ClientEvent::Reconnected`] or, after the
    /// grace, [`ClientEvent::Disconnected`] follows.
    Reconnecting {
        reason: String,
    },
    /// Connected (and said hello) again: to the new host after
    /// [`ClientEvent::HostChanged`], or to the same one after [`ClientEvent::Reconnecting`].
    Reconnected,
}

// One at a time through a channel (never stored in bulk): the size doesn't matter.
#[allow(clippy::large_enum_variant)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClientOptions {
    pub display_name: String,
    pub client_id: String,
    /// What it says it can do beyond the protocol (see [`FEATURES`]); empty: a plain
    /// protocol guest, as an ArchiveTune one.
    pub features: Vec<String>,
    /// Where it would listen on the LAN if it took a Friends session over.
    pub standby: Option<(String, u16)>,
    /// A dropped link (not an ended session) is retried this long (with backoff,
    /// same client id: no new approval) before the session counts as over.
    pub reconnect_for: Option<Duration>,
}

impl ClientOptions {
    /// This app's guest (with our additions).
    pub fn ours(display_name: &str, client_id: &str, standby: Option<(String, u16)>) -> Self {
        Self {
            display_name: display_name.to_owned(),
            client_id: client_id.to_owned(),
            features: FEATURES.iter().map(|f| (*f).to_owned()).collect(),
            standby,
            reconnect_for: Some(super::session::RECONNECT_GRACE),
        }
    }

    fn ours_enabled(&self) -> bool {
        self.features.iter().any(|f| f == FEATURE_SESSION)
    }
}

type Identity = ClientOptions;

/// Connects and says hello.
fn open(join: &JoinInfo, identity: &Identity) -> Result<Conn> {
    let mut conn = Conn::connect(&join.ws_url(), CONNECT_TIMEOUT)?;
    let hello = Message::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        session_id: join.session_id.clone(),
        session_key: join.session_key.clone(),
        client_id: identity.client_id.clone(),
        display_name: identity.display_name.trim().to_owned(),
        features: identity.features.clone(),
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
    /// Connects and says hello as a plain protocol guest (no additions; as an
    /// ArchiveTune guest would). Blocking (up to a few seconds); call off the UI thread.
    pub fn connect(join: &JoinInfo, display_name: &str, client_id: &str) -> Result<Self> {
        Self::connect_with(
            join,
            ClientOptions {
                display_name: display_name.to_owned(),
                client_id: client_id.to_owned(),
                ..ClientOptions::default()
            },
        )
    }

    /// Connects and says hello with `identity` (this app: [`ClientOptions::ours`]).
    /// Blocking (up to a few seconds); call off the UI thread.
    pub fn connect_with(join: &JoinInfo, identity: ClientOptions) -> Result<Self> {
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
        self.request_control_with(action, None)
    }

    /// A control request stamped with what this guest saw (see [`IntentMeta`]).
    pub fn request_control_with(&self, action: ControlAction, intent: Option<IntentMeta>) -> bool {
        let session_id = self.session_id.clone();
        self.send(|participant_id| Message::ControlRequest {
            session_id,
            participant_id,
            action,
            intent,
        })
    }

    /// A queue edit (hosts of this app, in a Friends session).
    pub fn request_queue(&self, op: QueueOp, intent: Option<IntentMeta>) -> bool {
        let session_id = self.session_id.clone();
        self.send(|participant_id| Message::QueueRequest {
            session_id,
            participant_id,
            op: Box::new(op),
            intent,
        })
    }

    pub fn request_mode(&self, mode: SessionMode) -> bool {
        let session_id = self.session_id.clone();
        self.send(|participant_id| Message::ModeRequest {
            session_id,
            participant_id,
            mode,
        })
    }

    pub fn request_kick(&self, target_id: &str, ban: bool) -> bool {
        let session_id = self.session_id.clone();
        let target_id = target_id.to_owned();
        self.send(|participant_id| Message::KickRequest {
            session_id,
            participant_id,
            target_id,
            ban,
        })
    }

    pub fn send_reaction(&self, emoji: &str, track_id: Option<&str>) -> bool {
        let session_id = self.session_id.clone();
        let (emoji, track_id) = (emoji.to_owned(), track_id.map(str::to_owned));
        self.send(|participant_id| Message::Reaction {
            session_id,
            participant_id,
            participant_name: None,
            emoji,
            track_id,
        })
    }

    /// An [`IntentMeta`] for a request about `track_id` / `uid` asked now (the host's
    /// clock as this guest estimates it).
    pub fn intent(&self, track_id: Option<String>, uid: Option<u64>) -> IntentMeta {
        let clock = self.clock();
        IntentMeta {
            track_id,
            uid,
            at_ms: (self.elapsed_ms() as i64 + clock.offset_ms).max(0) as u64,
        }
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
        super::wake();
    };
    // Room states repeat twice a second; only a change is worth waking the app for
    // (its own poll follows the position).
    let mut last_room: Option<RoomKey> = None;
    let mut lost = false;
    let mut next_ping = Instant::now();
    let mut ping_id = 0u64;
    // Fast pings again after reconnecting (a new host has a new clock).
    let mut fast_pings_from = 0u64;
    let mut pings: HashMap<u64, u64> = HashMap::new();
    let mut last_heard = Instant::now();
    // This app's host (it says so in the welcome), and whether it said the session
    // ended (else a "Session ended" from a relay means it lost the host).
    let mut host_ours = false;
    let mut ended_by_host = false;
    let reason = 'session: loop {
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
                            features,
                            ..
                        } if sid == session_id => {
                            host_ours = features.iter().any(|f| f == FEATURE_SESSION);
                            *lock(&shared.participant_id) = Some(participant_id.clone());
                            member_hello(&mut conn, session_id, &participant_id, identity);
                            emit(ClientEvent::Welcome {
                                participant_id,
                                pending: is_pending,
                                settings,
                            });
                        }
                        Message::SessionEnded { session_id: sid } if sid == session_id => {
                            ended_by_host = true;
                        }
                        Message::RoomState { state } if state.session_id == session_id => {
                            let key = RoomKey::of(&state);
                            if last_room.as_ref() == Some(&key) {
                                let _ = events.send(ClientEvent::RoomState(state));
                            } else {
                                last_room = Some(key);
                                emit(ClientEvent::RoomState(state));
                            }
                        }
                        Message::SessionRoster {
                            session_id: sid,
                            successors,
                            trusted_client_ids,
                            banned_client_ids,
                            relay_secret,
                        } if sid == session_id => emit(ClientEvent::Roster {
                            successors,
                            trusted_client_ids,
                            banned_client_ids,
                            relay_secret,
                        }),
                        Message::Reaction {
                            session_id: sid,
                            participant_id,
                            participant_name,
                            emoji,
                            track_id,
                        } if sid == session_id => emit(ClientEvent::Reaction {
                            participant_id,
                            name: participant_name.unwrap_or_default(),
                            emoji,
                            track_id,
                        }),
                        Message::SessionNote {
                            session_id: sid,
                            participant_id,
                            participant_name,
                            note,
                            count,
                            title,
                        } if sid == session_id => emit(ClientEvent::Note {
                            participant_id,
                            name: participant_name,
                            note,
                            count,
                            title,
                        }),
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
                            if lock(&shared.participant_id).as_deref()
                                == Some(participant_id.as_str())
                            {
                                // This app is the new host; it drops this client.
                                continue;
                            }
                            let old = lock(&shared.join).clone();
                            let join = match (host, port) {
                                (Some(host), Some(port)) if !host.is_empty() && port > 0 => {
                                    JoinInfo {
                                        host,
                                        port,
                                        url: None,
                                        ..old
                                    }
                                }
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
                            let retry = Backoff::new(
                                TRANSFER_RETRY,
                                TRANSFER_RETRY,
                                RECONNECT_FOR,
                                Instant::now(),
                            );
                            match reconnect(&join, identity, shared, &emit, retry) {
                                Some(mut new) => {
                                    if let Some(pid) = lock(&shared.participant_id).clone() {
                                        member_hello(&mut new, session_id, &pid, identity);
                                    }
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
                                None => {
                                    lost = true;
                                    break Some("Couldn't reach the new host".to_owned());
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Recv::Idle => {
                    if last_heard.elapsed() > HOST_TIMEOUT {
                        conn.close("Timed out");
                        lost = true;
                        break Some("The host stopped responding".into());
                    }
                }
                Recv::Closed(reason) => {
                    // This app's host says when it ends a session; a bare "Session
                    // ended" then is a relay that lost it.
                    let relay_lost = host_ours
                        && !ended_by_host
                        && reason.as_deref() == Some(super::server::SESSION_ENDED);
                    lost = relay_lost || reason.as_deref().is_none_or(is_lost);
                    break reason.or_else(|| Some("Disconnected".into()));
                }
            }
        };
        // A dropped link: retried for the grace (same client id, so no new approval)
        // while the app keeps playing; the session ends only after that.
        let join = lock(&shared.join).clone();
        log::warn!(
            "together: link to {} dropped ({}; lost: {lost}); {}",
            join.ws_url(),
            reason.as_deref().unwrap_or("-"),
            conn.diagnostics()
        );
        // A host that quit a Friends session said so: no point in waiting for it.
        let quit = reason.as_deref() == Some(HOST_LEFT);
        let Some(grace) = identity.reconnect_for.filter(|_| lost && !quit) else {
            break 'session reason;
        };
        if shared.shutdown.load(Ordering::Relaxed) {
            break 'session None;
        }
        emit(ClientEvent::Reconnecting {
            reason: reason.clone().unwrap_or_default(),
        });
        *lock(&shared.participant_id) = None;
        let started = Instant::now();
        let backoff = Backoff::new(
            Duration::from_millis(500),
            Duration::from_secs(5),
            grace,
            started,
        );
        match reconnect(&join, identity, shared, &emit, backoff) {
            Some(mut new) => {
                log::info!(
                    "together: reconnected to {} after {:.1} s",
                    join.ws_url(),
                    started.elapsed().as_secs_f64()
                );
                if let Some(pid) = lock(&shared.participant_id).clone() {
                    member_hello(&mut new, session_id, &pid, identity);
                }
                conn = new;
                *lock(&shared.clock) = TogetherClock::default();
                pings.clear();
                fast_pings_from = ping_id;
                next_ping = Instant::now();
                last_heard = Instant::now();
                lost = false;
                ended_by_host = false;
                emit(ClientEvent::Reconnected);
            }
            None if shared.shutdown.load(Ordering::Relaxed) => break 'session None,
            None => {
                log::warn!(
                    "together: gave up on {} after {:.0} s",
                    join.ws_url(),
                    started.elapsed().as_secs_f64()
                );
                break 'session reason;
            }
        }
    };
    emit(ClientEvent::Disconnected { reason, lost });
}

/// What of a room state is worth waking the app for when it changes.
#[derive(PartialEq)]
struct RoomKey {
    queue_hash: String,
    current_index: usize,
    is_playing: bool,
    repeat_mode: u8,
    shuffle_enabled: bool,
    settings: RoomSettings,
    participants: Vec<(String, bool)>,
    /// Coarse: a seek moves it out of its 2-second bucket relative to the stamp.
    position_bucket: i64,
}

impl RoomKey {
    fn of(state: &RoomState) -> Self {
        let drift = state.position_ms as i64 - state.sent_at_elapsed_realtime_ms as i64;
        Self {
            queue_hash: state.queue_hash.clone(),
            current_index: state.current_index,
            is_playing: state.is_playing,
            repeat_mode: state.repeat_mode,
            shuffle_enabled: state.shuffle_enabled,
            settings: state.settings,
            participants: state
                .participants
                .iter()
                .map(|p| (p.id.clone(), p.is_pending))
                .collect(),
            // Playing, position and stamp advance together; paused, the position stays.
            position_bucket: if state.is_playing {
                drift.div_euclid(2_000)
            } else {
                (state.position_ms / 2_000) as i64
            },
        }
    }
}

/// A goodbye that means the host is gone, not that the session ended: the connection
/// dropped, the relay lost the host, or the host quit a Friends session.
fn is_lost(reason: &str) -> bool {
    matches!(
        reason,
        "Connection lost" | "Relay connection lost" | "Host connection lost"
    ) || reason == HOST_LEFT
}

/// This app's guest tells the host where it would take a Friends session over.
fn member_hello(conn: &mut Conn, session_id: &str, participant_id: &str, identity: &Identity) {
    if !identity.ours_enabled() {
        return;
    }
    conn.send(&Message::MemberHello {
        session_id: session_id.to_owned(),
        participant_id: participant_id.to_owned(),
        standby_host: identity.standby.as_ref().map(|(h, _)| h.clone()),
        standby_port: identity.standby.as_ref().map(|(_, p)| *p),
    });
}

/// Tries to reach a new host for [`RECONNECT_FOR`] (it may still be starting, a
/// relay may still count the old connection): connects, says hello and waits for
/// the welcome (recorded and emitted like the first one); refusals are retried.
fn reconnect(
    join: &JoinInfo,
    identity: &Identity,
    shared: &Shared,
    emit: &dyn Fn(ClientEvent),
    mut backoff: Backoff,
) -> Option<Conn> {
    while Instant::now() < backoff.deadline() && !shared.shutdown.load(Ordering::Relaxed) {
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
        let Some(wait) = backoff.next_delay(Instant::now()) else {
            break;
        };
        // In short steps, so dropping the client doesn't wait out the backoff.
        let until = Instant::now() + wait;
        while Instant::now() < until && !shared.shutdown.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50).min(until - Instant::now()));
        }
    }
    None
}
