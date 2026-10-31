//! This device's iroh endpoint and the hand-off protocol, on a thread of its own (a
//! single-threaded tokio runtime: iroh needs one, the rest of the app has none).
//!
//! The app talks to it through [`HandoffNode`]: commands go in over a channel and
//! [`Event`]s come back on another, read with [`HandoffNode::poll_events`] (the app
//! does that from its existing poll; nothing here wakes the UI). What this device is
//! playing is mirrored into the node ([`HandoffNode::set_snapshot`]) so requests from
//! paired devices are answered without the UI thread.
//!
//! Connectivity without n0's services: no public relays (only the relay the app
//! passes, if any; none in tests), no DNS / pkarr address lookup, no captive-portal
//! check; devices find each other on the LAN with mDNS (service name of the app's
//! choosing) and otherwise dial the relay and direct addresses learnt at pairing and
//! kept current from every frame they exchange. Connections are made on demand and
//! close when idle (QUIC idle timeout); the endpoint itself only lives while the app
//! runs and has paired devices or shows a code.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt as _;
use iroh::address_lookup::UserData;
use iroh::endpoint::{Connection, NetReportConfig, RecvStream, SendStream, presets};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayConfig, RelayMap, RelayMode, SecretKey};
use iroh_mdns_address_lookup::{DiscoveryEvent, MdnsAddressLookup};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use super::messages::{
    DEVICES_ALPN, DecodeError, Frame, MAX_FRAME, Message, PAIR_ALPN, PeerInfo, Snapshot, code,
    decode, encode, length_prefix,
};
use super::pairing::{
    self, CODE_TTL, Exchange, MAX_ATTEMPTS, PAIRING_USER_DATA, PairInput, PairingLink, Side,
};

/// Giving up on reaching a device.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
/// Giving up on an answer.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
/// Pairing by code: how long to look for a device showing a code on the LAN.
const FIND_TIMEOUT: Duration = Duration::from_secs(10);
/// mDNS queries: rarer than iroh's default (700 ms) to keep the idle cost down; a
/// device that starts showing a code still announces itself at once.
const MDNS_CADENCE: Duration = Duration::from_secs(4);

/// A device this one is paired with (`settings.json` → `devices.paired`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PairedDevice {
    /// Its device id (hex public key).
    pub id: String,
    pub name: String,
    /// Where it was last reachable (kept current while connected).
    pub relay_url: Option<String>,
    pub addrs: Vec<SocketAddr>,
    pub paired_at_unix_ms: u64,
}

impl PairedDevice {
    fn endpoint_id(&self) -> Option<EndpointId> {
        self.id.parse().ok()
    }

    fn addr(&self, id: EndpointId) -> EndpointAddr {
        let mut addr = EndpointAddr::new(id);
        if let Some(url) = self.relay_url.as_deref().and_then(|u| u.parse().ok()) {
            addr = addr.with_relay_url(url);
        }
        for ip in &self.addrs {
            addr = addr.with_ip_addr(*ip);
        }
        addr
    }

    fn apply(&mut self, peer: &PeerInfo) -> bool {
        let before = self.clone();
        if !peer.name.trim().is_empty() {
            self.name = peer.name.trim().chars().take(60).collect();
        }
        if peer.relay_url.is_some() {
            self.relay_url = peer.relay_url.clone();
        }
        if !peer.addrs.is_empty() {
            self.addrs = peer.addrs.iter().copied().take(16).collect();
        }
        *self != before
    }
}

/// How the node starts.
#[derive(Clone)]
pub struct NodeConfig {
    /// The device key (from the keychain); never logged.
    pub secret_key: [u8; 32],
    pub name: String,
    /// Our relay (`https://…`); `None`: LAN and direct addresses only.
    pub relay_url: Option<String>,
    pub paired: Vec<PairedDevice>,
    /// mDNS on the LAN (off in tests).
    pub local_discovery: bool,
    /// mDNS service name (the app's id).
    pub service_name: String,
    /// UDP port to bind (0: any).
    pub port: u16,
}

impl std::fmt::Debug for NodeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeConfig")
            .field("name", &self.name)
            .field("relay_url", &self.relay_url)
            .field("paired", &self.paired.len())
            .finish_non_exhaustive()
    }
}

/// What a request was for (in [`Event::Failed`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Query,
    Pause,
    TakeOver,
    PlayOn,
}

/// Why pairing failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairError {
    /// No device showing a code was found on this network.
    NotFound,
    /// The code didn't match.
    WrongCode,
    /// The other device isn't showing a code (any more), or stopped after too many tries.
    NotPairing,
    /// Couldn't reach it, or it broke off.
    Network(String),
}

/// What the node reports to the app.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// What a paired device is playing (`Ok(None)`: nothing).
    Status {
        id: String,
        result: Result<Option<Snapshot>, String>,
    },
    /// "Continue here" worked: play this (the other device paused).
    TookOver {
        id: String,
        snapshot: Snapshot,
    },
    /// "Play on …" worked: the other device plays; pause here.
    Sent {
        id: String,
    },
    Failed {
        id: String,
        action: Action,
        error: String,
    },
    /// A paired device asked this one to pause (it pressed "Continue here", or Pause).
    PauseRequested {
        from: String,
    },
    /// A paired device sent its playback here ("Play on …").
    PlayRequested {
        from: String,
        snapshot: Snapshot,
    },
    /// A paired device's name or addresses changed: save them.
    DeviceUpdated(PairedDevice),
    Paired(PairedDevice),
    PairFailed(PairError),
    /// The shown code is no longer valid (used, expired, too many tries).
    PairingEnded,
    /// A paired device forgot this one.
    Unpaired {
        id: String,
    },
}

/// A shown pairing code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingOffer {
    pub code: String,
    pub link: PairingLink,
}

enum Command {
    Query(EndpointId),
    Pause(EndpointId),
    TakeOver(EndpointId),
    PlayOn(EndpointId, Box<Snapshot>),
    Pair(PairInput),
    Unpair(EndpointId),
    Advertise(bool),
}

struct Pairing {
    code: String,
    until: Instant,
    attempts: u32,
    /// The shown code was already used by a joiner that finished.
    done: bool,
}

#[derive(Default)]
struct Shared {
    name: Mutex<String>,
    /// What this device plays, and when that was taken.
    local: Mutex<Option<(Snapshot, Instant)>>,
    paired: Mutex<HashMap<EndpointId, PairedDevice>>,
    pairing: Mutex<Option<Pairing>>,
    /// Devices showing a code on the LAN (from mDNS).
    showing: Mutex<HashSet<EndpointId>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The running node; dropping it closes the endpoint.
pub struct HandoffNode {
    commands: Option<mpsc::UnboundedSender<Command>>,
    events: std_mpsc::Receiver<Event>,
    shared: Arc<Shared>,
    endpoint: Endpoint,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl HandoffNode {
    /// Binds the endpoint (blocking until it is up, a few ms).
    pub fn start(config: NodeConfig) -> Result<Self, String> {
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (event_tx, events) = std_mpsc::channel();
        let (ready_tx, ready_rx) = std_mpsc::channel();
        let shared = Arc::new(Shared::default());
        *lock(&shared.name) = config.name.clone();
        lock(&shared.paired).extend(
            config
                .paired
                .iter()
                .filter_map(|d| Some((d.endpoint_id()?, d.clone()))),
        );
        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("handoff".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                rt.block_on(run(config, thread_shared, command_rx, event_tx, ready_tx));
            })
            .map_err(|e| e.to_string())?;
        let endpoint = ready_rx
            .recv()
            .map_err(|_| "the hand-off thread stopped".to_owned())??;
        Ok(Self {
            commands: Some(commands),
            events,
            shared,
            endpoint,
            thread: Some(thread),
        })
    }

    /// This device's id (hex public key).
    pub fn id(&self) -> String {
        self.endpoint.id().to_string()
    }

    /// Where this device can be reached now (relay, direct addresses).
    pub fn reachable(&self) -> (Option<String>, Vec<SocketAddr>) {
        let addr = self.endpoint.addr();
        (
            addr.relay_urls().next().map(|u| u.to_string()),
            addr.ip_addrs().copied().collect(),
        )
    }

    fn send(&self, command: Command) {
        if let Some(tx) = &self.commands {
            let _ = tx.send(command);
        }
    }

    /// Events since the last call (never blocks).
    pub fn poll_events(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    /// What this device plays (`None`: nothing in the queue). The position is taken
    /// as of now and advanced while playing, so this only needs calling on changes.
    pub fn set_snapshot(&self, snapshot: Option<Snapshot>) {
        *lock(&self.shared.local) = snapshot.map(|s| (s, Instant::now()));
    }

    pub fn set_name(&self, name: &str) {
        *lock(&self.shared.name) = name.to_owned();
    }

    /// The paired devices (only these are accepted).
    pub fn set_paired(&self, paired: &[PairedDevice]) {
        let mut map = lock(&self.shared.paired);
        map.clear();
        map.extend(
            paired
                .iter()
                .filter_map(|d| Some((d.endpoint_id()?, d.clone()))),
        );
    }

    fn target(&self, id: &str) -> Option<EndpointId> {
        id.parse().ok()
    }

    /// Asks what `id` is playing → [`Event::Status`].
    pub fn query(&self, id: &str) {
        if let Some(id) = self.target(id) {
            self.send(Command::Query(id));
        }
    }

    /// Asks every paired device what it plays.
    pub fn query_all(&self) {
        let ids: Vec<EndpointId> = lock(&self.shared.paired).keys().copied().collect();
        for id in ids {
            self.send(Command::Query(id));
        }
    }

    pub fn pause(&self, id: &str) {
        if let Some(id) = self.target(id) {
            self.send(Command::Pause(id));
        }
    }

    /// "Continue here" → [`Event::TookOver`] (the other device pauses).
    pub fn take_over(&self, id: &str) {
        if let Some(id) = self.target(id) {
            self.send(Command::TakeOver(id));
        }
    }

    /// "Play on …" → [`Event::Sent`].
    pub fn play_on(&self, id: &str, snapshot: Snapshot) {
        if let Some(id) = self.target(id) {
            self.send(Command::PlayOn(id, Box::new(snapshot)));
        }
    }

    /// Shows a new code (replacing any other) for [`CODE_TTL`].
    pub fn start_pairing(&self) -> PairingOffer {
        let code = pairing::new_code();
        *lock(&self.shared.pairing) = Some(Pairing {
            code: code.clone(),
            until: Instant::now() + CODE_TTL,
            attempts: 0,
            done: false,
        });
        self.send(Command::Advertise(true));
        let (relay_url, addrs) = self.reachable();
        PairingOffer {
            link: PairingLink {
                id: self.id(),
                code: code.clone(),
                relay_url,
                addrs,
            },
            code,
        }
    }

    pub fn cancel_pairing(&self) {
        if lock(&self.shared.pairing).take().is_some() {
            self.send(Command::Advertise(false));
        }
    }

    /// Pairs with the device showing this code (or link) → [`Event::Paired`] /
    /// [`Event::PairFailed`].
    pub fn pair(&self, input: PairInput) {
        self.send(Command::Pair(input));
    }

    /// Forgets `id` (and tells it to forget this one, if it can be reached).
    pub fn unpair(&self, id: &str) {
        if let Some(id) = self.target(id) {
            self.send(Command::Unpair(id));
        }
    }
}

impl Drop for HandoffNode {
    fn drop(&mut self) {
        // Closing the channel ends the loop, which closes the endpoint.
        self.commands = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn bind(config: &NodeConfig) -> Result<Endpoint, String> {
    let relay_mode = match config
        .relay_url
        .as_deref()
        .map(str::parse::<iroh::RelayUrl>)
    {
        Some(Ok(url)) => {
            // QUIC address discovery off: our relay sits behind a TLS proxy that
            // doesn't forward UDP (see deploy/iroh-relay/README.md).
            let map = RelayMap::empty();
            map.insert(url.clone(), Arc::new(RelayConfig::new(url, None)));
            RelayMode::Custom(map)
        }
        Some(Err(e)) => return Err(format!("relay URL: {e}")),
        None => RelayMode::Disabled,
    };
    let mut net_report = NetReportConfig::minimal();
    // `/ping` latency probes against our relay; no captive-portal check (it asks for
    // `http://…/generate_204`, which the proxy redirects to HTTPS).
    net_report.https_probes = true;
    Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(&config.secret_key))
        .alpns(vec![DEVICES_ALPN.to_vec(), PAIR_ALPN.to_vec()])
        .relay_mode(relay_mode)
        .net_report_config(net_report)
        .bind_addr(std::net::SocketAddr::from(([0, 0, 0, 0], config.port)))
        .map_err(|e| e.to_string())?
        .bind()
        .await
        .map_err(|e| e.to_string())
}

/// Everything a task needs.
struct Ctx {
    endpoint: Endpoint,
    shared: Arc<Shared>,
    events: std_mpsc::Sender<Event>,
    conns: Mutex<HashMap<EndpointId, Connection>>,
}

impl Ctx {
    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn me(&self) -> PeerInfo {
        let addr = self.endpoint.addr();
        PeerInfo {
            name: lock(&self.shared.name).clone(),
            relay_url: addr.relay_urls().next().map(|u| u.to_string()),
            addrs: addr.ip_addrs().copied().collect(),
        }
    }

    fn local_snapshot(&self) -> Option<Snapshot> {
        let local = lock(&self.shared.local);
        let (snapshot, at) = local.as_ref()?;
        let mut snapshot = snapshot.clone();
        if snapshot.is_playing {
            snapshot.position_ms += at.elapsed().as_millis() as u64;
            snapshot.last_played_unix_ms = now_unix_ms();
        }
        snapshot.device_name = lock(&self.shared.name).clone();
        Some(snapshot)
    }

    /// Keeps a paired device's name / addresses current.
    fn learn(&self, id: EndpointId, peer: Option<&PeerInfo>) {
        let Some(peer) = peer else { return };
        let updated = {
            let mut paired = lock(&self.shared.paired);
            paired
                .get_mut(&id)
                .and_then(|device| device.apply(peer).then(|| device.clone()))
        };
        if let Some(device) = updated {
            self.emit(Event::DeviceUpdated(device));
        }
    }

    async fn connect(self: &Arc<Self>, id: EndpointId) -> Result<Connection, String> {
        if let Some(conn) = lock(&self.conns).get(&id)
            && conn.close_reason().is_none()
        {
            return Ok(conn.clone());
        }
        let addr = match lock(&self.shared.paired).get(&id) {
            Some(device) => device.addr(id),
            None => return Err("not paired".into()), // i18n-ignore
        };
        let conn = tokio::time::timeout(CONNECT_TIMEOUT, self.endpoint.connect(addr, DEVICES_ALPN))
            .await
            .map_err(|_| "timed out".to_owned())? // i18n-ignore
            .map_err(|e| e.to_string())?;
        lock(&self.conns).insert(id, conn.clone());
        // Either side may ask on either's connection.
        tokio::spawn(answer_all(self.clone(), conn.clone()));
        Ok(conn)
    }

    /// One request, one answer (a stale cached connection is retried once).
    async fn request(self: &Arc<Self>, id: EndpointId, msg: &Message) -> Result<Message, String> {
        let mut last = String::new();
        for _ in 0..2 {
            let conn = self.connect(id).await?;
            match exchange(&conn, &self.me(), msg).await {
                Ok(frame) => {
                    self.learn(id, frame.from.as_ref());
                    return match frame.msg {
                        Message::Error { code, message } => Err(if message.is_empty() {
                            code
                        } else {
                            format!("{code}: {message}")
                        }),
                        msg => Ok(msg),
                    };
                }
                Err(e) => {
                    last = e;
                    lock(&self.conns).remove(&id);
                }
            }
        }
        Err(last)
    }
}

async fn write_frame(
    send: &mut SendStream,
    from: Option<&PeerInfo>,
    msg: &Message,
) -> Result<(), String> {
    let body = encode(from, msg);
    send.write_all(&length_prefix(body.len()))
        .await
        .map_err(|e| e.to_string())?;
    send.write_all(&body).await.map_err(|e| e.to_string())
}

async fn read_frame(recv: &mut RecvStream) -> Result<Frame, ReadError> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len)
        .await
        .map_err(|e| ReadError::Io(e.to_string()))?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(ReadError::Decode(DecodeError::Malformed));
    }
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body)
        .await
        .map_err(|e| ReadError::Io(e.to_string()))?;
    decode(&body).map_err(ReadError::Decode)
}

enum ReadError {
    Io(String),
    Decode(DecodeError),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => f.write_str(e),
            Self::Decode(e) => e.fmt(f),
        }
    }
}

/// Sends `msg` on a new stream and reads the answer.
async fn exchange(conn: &Connection, me: &PeerInfo, msg: &Message) -> Result<Frame, String> {
    tokio::time::timeout(REQUEST_TIMEOUT, async {
        let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
        write_frame(&mut send, Some(me), msg).await?;
        send.finish().map_err(|e| e.to_string())?;
        read_frame(&mut recv).await.map_err(|e| e.to_string())
    })
    .await
    .map_err(|_| "timed out".to_owned())? // i18n-ignore
}

async fn run(
    config: NodeConfig,
    shared: Arc<Shared>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: std_mpsc::Sender<Event>,
    ready: std_mpsc::Sender<Result<Endpoint, String>>,
) {
    let endpoint = match bind(&config).await {
        Ok(endpoint) => endpoint,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    // mDNS: advertise this device and watch for devices showing a code.
    let mdns = if config.local_discovery {
        match MdnsAddressLookup::builder()
            .service_name(config.service_name.clone())
            .discovery_cadence(MDNS_CADENCE)
            .build(endpoint.id())
        {
            Ok(mdns) => {
                if let Ok(lookup) = endpoint.address_lookup() {
                    lookup.add(mdns.clone());
                }
                Some(mdns)
            }
            Err(e) => {
                log::warn!("handoff: no local discovery ({e})");
                None
            }
        }
    } else {
        None
    };
    let _ = ready.send(Ok(endpoint.clone()));
    let ctx = Arc::new(Ctx {
        endpoint: endpoint.clone(),
        shared,
        events,
        conns: Mutex::new(HashMap::new()),
    });
    if let Some(mdns) = mdns {
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let mut stream = mdns.subscribe().await;
            while let Some(event) = stream.next().await {
                let mut showing = lock(&ctx.shared.showing);
                match event {
                    DiscoveryEvent::Discovered { endpoint_info, .. } => {
                        let id = endpoint_info.endpoint_id;
                        if endpoint_info.user_data().map(|d| d.to_string()).as_deref()
                            == Some(PAIRING_USER_DATA)
                        {
                            showing.insert(id);
                        } else {
                            showing.remove(&id);
                        }
                    }
                    DiscoveryEvent::Expired { endpoint_id } => {
                        showing.remove(&endpoint_id);
                    }
                    _ => {}
                }
            }
        });
    }
    loop {
        tokio::select! {
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break };
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    let Ok(accepting) = incoming.accept() else { return };
                    let Ok(conn) = accepting.await else { return };
                    serve(ctx, conn).await;
                });
            }
            command = commands.recv() => {
                let Some(command) = command else { break };
                handle(&ctx, command);
            }
        }
    }
    endpoint.close().await;
}

fn handle(ctx: &Arc<Ctx>, command: Command) {
    let ctx = ctx.clone();
    match command {
        Command::Query(id) => {
            tokio::spawn(async move {
                let result = match ctx.request(id, &Message::WhatAreYouPlaying).await {
                    Ok(Message::Playing { snapshot }) => Ok(snapshot),
                    Ok(other) => Err(format!("unexpected answer {other:?}")), // i18n-ignore
                    Err(e) => Err(e),
                };
                ctx.emit(Event::Status {
                    id: id.to_string(),
                    result,
                });
            });
        }
        Command::Pause(id) => {
            tokio::spawn(async move {
                if let Err(error) = ctx.request(id, &Message::Pause).await {
                    ctx.emit(Event::Failed {
                        id: id.to_string(),
                        action: Action::Pause,
                        error,
                    });
                }
            });
        }
        Command::TakeOver(id) => {
            tokio::spawn(async move {
                let start = Instant::now();
                let event = match ctx.request(id, &Message::TakeOver).await {
                    Ok(Message::Playing {
                        snapshot: Some(mut snapshot),
                    }) => {
                        // The other device's position is half a round trip old.
                        if snapshot.is_playing {
                            snapshot.position_ms += start.elapsed().as_millis() as u64 / 2;
                        }
                        Event::TookOver {
                            id: id.to_string(),
                            snapshot,
                        }
                    }
                    Ok(Message::Playing { snapshot: None }) => Event::Failed {
                        id: id.to_string(),
                        action: Action::TakeOver,
                        error: "nothing playing".into(), // i18n-ignore
                    },
                    Ok(other) => Event::Failed {
                        id: id.to_string(),
                        action: Action::TakeOver,
                        error: format!("unexpected answer {other:?}"), // i18n-ignore
                    },
                    Err(error) => Event::Failed {
                        id: id.to_string(),
                        action: Action::TakeOver,
                        error,
                    },
                };
                ctx.emit(event);
            });
        }
        Command::PlayOn(id, snapshot) => {
            tokio::spawn(async move {
                let msg = Message::PlayThisHere {
                    snapshot: *snapshot,
                };
                ctx.emit(match ctx.request(id, &msg).await {
                    Ok(_) => Event::Sent { id: id.to_string() },
                    Err(error) => Event::Failed {
                        id: id.to_string(),
                        action: Action::PlayOn,
                        error,
                    },
                });
            });
        }
        Command::Pair(input) => {
            tokio::spawn(async move {
                let event = match pair(&ctx, input).await {
                    Ok(device) => Event::Paired(device),
                    Err(e) => Event::PairFailed(e),
                };
                ctx.emit(event);
            });
        }
        Command::Unpair(id) => {
            tokio::spawn(async move {
                // Tell it while it still accepts us, then forget it.
                let _ = tokio::time::timeout(
                    Duration::from_secs(4),
                    ctx.request(id, &Message::Unpaired),
                )
                .await;
                lock(&ctx.shared.paired).remove(&id);
                if let Some(conn) = lock(&ctx.conns).remove(&id) {
                    conn.close(0u32.into(), b"unpaired");
                }
            });
        }
        Command::Advertise(on) => {
            let data = on
                .then(|| PAIRING_USER_DATA.parse::<UserData>().ok())
                .flatten();
            ctx.endpoint.set_user_data_for_address_lookup(data);
            if on {
                // The code expires on its own.
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(CODE_TTL).await;
                    end_pairing_if_expired(&ctx);
                });
            }
        }
    }
}

fn end_pairing_if_expired(ctx: &Ctx) {
    let ended = {
        let mut pairing = lock(&ctx.shared.pairing);
        match pairing.as_ref() {
            Some(p) if p.until <= Instant::now() => {
                *pairing = None;
                true
            }
            _ => false,
        }
    };
    if ended {
        ctx.endpoint.set_user_data_for_address_lookup(None);
        ctx.emit(Event::PairingEnded);
    }
}

/// An incoming connection: requests from a paired device, or pairing.
async fn serve(ctx: Arc<Ctx>, conn: Connection) {
    let id = conn.remote_id();
    match conn.alpn() {
        DEVICES_ALPN => {
            // Only paired devices; anyone else is turned away at once.
            if !lock(&ctx.shared.paired).contains_key(&id) {
                conn.close(1u32.into(), b"not paired");
                return;
            }
            lock(&ctx.conns).insert(id, conn.clone());
            answer_all(ctx, conn).await;
        }
        PAIR_ALPN => serve_pairing(&ctx, &conn).await,
        _ => conn.close(2u32.into(), b"unknown protocol"),
    }
}

/// Answers the requests that come in on a connection with a paired device.
async fn answer_all(ctx: Arc<Ctx>, conn: Connection) {
    let id = conn.remote_id();
    while let Ok((send, recv)) = conn.accept_bi().await {
        let ctx = ctx.clone();
        tokio::spawn(async move { answer(&ctx, id, send, recv).await });
    }
}

/// Answers one request from paired device `id`.
async fn answer(ctx: &Ctx, id: EndpointId, mut send: SendStream, mut recv: RecvStream) {
    let reply = match tokio::time::timeout(REQUEST_TIMEOUT, read_frame(&mut recv)).await {
        Err(_) => return,
        Ok(Err(ReadError::Io(_))) => return,
        Ok(Err(ReadError::Decode(DecodeError::Unsupported(kind)))) => {
            Message::error(code::UNSUPPORTED, kind)
        }
        Ok(Err(ReadError::Decode(e))) => Message::error(code::MALFORMED, e.to_string()),
        Ok(Ok(frame)) => {
            ctx.learn(id, frame.from.as_ref());
            let from = id.to_string();
            match frame.msg {
                Message::WhatAreYouPlaying => Message::Playing {
                    snapshot: ctx.local_snapshot(),
                },
                Message::Pause => {
                    ctx.emit(Event::PauseRequested { from });
                    Message::Ok
                }
                Message::TakeOver => {
                    let snapshot = ctx.local_snapshot();
                    if snapshot.is_some() {
                        ctx.emit(Event::PauseRequested { from });
                    }
                    Message::Playing { snapshot }
                }
                Message::PlayThisHere { snapshot } => {
                    ctx.emit(Event::PlayRequested { from, snapshot });
                    Message::Ok
                }
                Message::Unpaired => {
                    lock(&ctx.shared.paired).remove(&id);
                    ctx.emit(Event::Unpaired { id: from });
                    Message::Ok
                }
                other => Message::error(code::UNSUPPORTED, format!("{other:?}")),
            }
        }
    };
    if write_frame(&mut send, Some(&ctx.me()), &reply)
        .await
        .is_ok()
    {
        let _ = send.finish();
        // Let the answer go out before the stream is dropped.
        let _ = send.stopped().await;
    }
}

/// The shower's side of pairing (see `pairing.rs`).
async fn serve_pairing(ctx: &Ctx, conn: &Connection) {
    let joiner = conn.remote_id();
    let Ok(Ok((mut send, mut recv))) =
        tokio::time::timeout(REQUEST_TIMEOUT, conn.accept_bi()).await
    else {
        return;
    };
    let result = tokio::time::timeout(REQUEST_TIMEOUT, async {
        let Ok(Frame {
            msg: Message::PairStart { spake },
            ..
        }) = read_frame(&mut recv).await
        else {
            return Err(Message::error(code::MALFORMED, ""));
        };
        // Every attempt counts, right or wrong: a guess costs one.
        let code = {
            let mut pairing = lock(&ctx.shared.pairing);
            match pairing.as_mut() {
                Some(p) if p.until > Instant::now() && !p.done && p.attempts < MAX_ATTEMPTS => {
                    p.attempts += 1;
                    p.code.clone()
                }
                _ => return Err(Message::error(code::NOT_PAIRING, "")),
            }
        };
        let (exchange, ours) = Exchange::start(
            Side::Shower,
            &code,
            *ctx.endpoint.id().as_bytes(),
            *joiner.as_bytes(),
        );
        let keys = exchange
            .finish(&spake)
            .ok_or_else(|| Message::error(code::MALFORMED, ""))?;
        write_frame(
            &mut send,
            None,
            &Message::PairChallenge {
                spake: ours,
                confirm: keys.confirm(),
            },
        )
        .await
        .map_err(|_| Message::Ok)?;
        let Ok(Frame {
            msg: Message::PairConfirm { confirm, peer },
            ..
        }) = read_frame(&mut recv).await
        else {
            return Err(Message::Ok);
        };
        if !keys.verify(&confirm) {
            return Err(Message::error(code::WRONG_CODE, ""));
        }
        Ok(peer)
    })
    .await
    .unwrap_or(Err(Message::Ok));

    match result {
        Ok(peer) => {
            {
                let mut pairing = lock(&ctx.shared.pairing);
                if let Some(p) = pairing.as_mut() {
                    p.done = true;
                }
                *pairing = None;
            }
            let device = new_device(joiner, &peer);
            lock(&ctx.shared.paired).insert(joiner, device.clone());
            if write_frame(&mut send, None, &Message::PairDone { peer: ctx.me() })
                .await
                .is_ok()
            {
                let _ = send.finish();
                let _ = send.stopped().await;
            }
            ctx.endpoint.set_user_data_for_address_lookup(None);
            ctx.emit(Event::Paired(device));
        }
        Err(reply) => {
            let too_many = lock(&ctx.shared.pairing)
                .as_ref()
                .is_some_and(|p| p.attempts >= MAX_ATTEMPTS);
            if reply != Message::Ok {
                let reply = match reply {
                    Message::Error { code, .. } if code == code::WRONG_CODE && too_many => {
                        Message::error(code::TOO_MANY_TRIES, "")
                    }
                    other => other,
                };
                let _ = write_frame(&mut send, None, &reply).await;
                let _ = send.finish();
                let _ = send.stopped().await;
            }
            if too_many {
                *lock(&ctx.shared.pairing) = None;
                ctx.endpoint.set_user_data_for_address_lookup(None);
                ctx.emit(Event::PairingEnded);
            }
        }
    }
}

fn new_device(id: EndpointId, peer: &PeerInfo) -> PairedDevice {
    let mut device = PairedDevice {
        id: id.to_string(),
        name: id.fmt_short().to_string(),
        paired_at_unix_ms: now_unix_ms(),
        ..PairedDevice::default()
    };
    device.apply(peer);
    device
}

/// The joiner's side: finds the shower (link, or a device showing a code on the LAN)
/// and runs the exchange.
async fn pair(ctx: &Ctx, input: PairInput) -> Result<PairedDevice, PairError> {
    let code = input.code().to_owned();
    let targets: Vec<EndpointAddr> = match &input {
        PairInput::Link(link) => {
            let id: EndpointId = link
                .id
                .parse()
                .map_err(|_| PairError::Network("bad device id".into()))?; // i18n-ignore
            let device = PairedDevice {
                relay_url: link.relay_url.clone(),
                addrs: link.addrs.clone(),
                ..PairedDevice::default()
            };
            vec![device.addr(id)]
        }
        PairInput::Code(_) => {
            let deadline = Instant::now() + FIND_TIMEOUT;
            loop {
                let found: Vec<EndpointId> = lock(&ctx.shared.showing).iter().copied().collect();
                if !found.is_empty() {
                    break found.into_iter().map(EndpointAddr::new).collect();
                }
                if Instant::now() >= deadline {
                    return Err(PairError::NotFound);
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    };
    let mut last = PairError::NotFound;
    for target in targets {
        match pair_with(ctx, target, &code).await {
            Ok(device) => return Ok(device),
            Err(e) => last = e,
        }
    }
    Err(last)
}

async fn pair_with(ctx: &Ctx, target: EndpointAddr, code: &str) -> Result<PairedDevice, PairError> {
    let shower = target.id;
    if shower == ctx.endpoint.id() {
        return Err(PairError::NotPairing);
    }
    let net = |e: String| PairError::Network(e);
    let conn = tokio::time::timeout(CONNECT_TIMEOUT, ctx.endpoint.connect(target, PAIR_ALPN))
        .await
        .map_err(|_| net("timed out".into()))? // i18n-ignore
        .map_err(|e| net(e.to_string()))?;
    let result = tokio::time::timeout(REQUEST_TIMEOUT, async {
        let (mut send, mut recv) = conn.open_bi().await.map_err(|e| net(e.to_string()))?;
        let (exchange, ours) = Exchange::start(
            Side::Joiner,
            code,
            *shower.as_bytes(),
            *ctx.endpoint.id().as_bytes(),
        );
        write_frame(&mut send, None, &Message::PairStart { spake: ours })
            .await
            .map_err(net)?;
        let keys = match read_frame(&mut recv)
            .await
            .map_err(|e| net(e.to_string()))?
            .msg
        {
            Message::PairChallenge { spake, confirm } => {
                let keys = exchange
                    .finish(&spake)
                    .ok_or_else(|| net("bad answer".into()))?; // i18n-ignore
                if !keys.verify(&confirm) {
                    return Err(PairError::WrongCode);
                }
                keys
            }
            Message::Error { code: c, .. } if c == code::NOT_PAIRING => {
                return Err(PairError::NotPairing);
            }
            other => return Err(net(format!("unexpected answer {other:?}"))), // i18n-ignore
        };
        write_frame(
            &mut send,
            None,
            &Message::PairConfirm {
                confirm: keys.confirm(),
                peer: ctx.me(),
            },
        )
        .await
        .map_err(net)?;
        let _ = send.finish();
        match read_frame(&mut recv)
            .await
            .map_err(|e| net(e.to_string()))?
            .msg
        {
            Message::PairDone { peer } => Ok(peer),
            Message::Error { code: c, .. } if c == code::WRONG_CODE => Err(PairError::WrongCode),
            Message::Error { code: c, .. }
                if c == code::TOO_MANY_TRIES || c == code::NOT_PAIRING =>
            {
                Err(PairError::NotPairing)
            }
            other => Err(net(format!("unexpected answer {other:?}"))), // i18n-ignore
        }
    })
    .await
    .map_err(|_| net("timed out".into()))?; // i18n-ignore
    conn.close(0u32.into(), b"done");
    let device = new_device(shower, &result?);
    lock(&ctx.shared.paired).insert(shower, device.clone());
    Ok(device)
}
