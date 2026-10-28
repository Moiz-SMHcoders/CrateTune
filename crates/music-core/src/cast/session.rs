//! One connection to a Cast device: TLS to port 8009, the virtual connection to
//! the receiver, heartbeats, launching the Default Media Receiver, and media
//! LOAD / PLAY / PAUSE / SEEK / STOP / volume with the device's status mirrored
//! into [`CastStatus`]. One worker thread per session.

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use serde_json::{Value, json};

use super::proto::{CastMessage, FrameReader, NS_CONNECTION, NS_HEARTBEAT, NS_MEDIA, NS_RECEIVER};
use crate::{Error, Result};

/// Google's Default Media Receiver (plays any media URL, shows the metadata).
pub const DEFAULT_MEDIA_RECEIVER: &str = "CC1AD845";
const SENDER: &str = "sender-0";
const RECEIVER: &str = "receiver-0";
const PING_EVERY: Duration = Duration::from_secs(5);
const STATUS_EVERY: Duration = Duration::from_secs(5);
/// No message (not even a pong) for this long: the device is gone.
const DEVICE_TIMEOUT: Duration = Duration::from_secs(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(6);
const POLL: Duration = Duration::from_millis(50);

/// What to play (the receiver fetches `url` itself).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MediaInfo {
    pub url: String,
    /// `audio/mp4`, `audio/webm`, `audio/flac`, …
    pub content_type: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub image: Option<String>,
    pub duration: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CastPlayerState {
    #[default]
    Idle,
    Buffering,
    Playing,
    Paused,
}

/// The device as last reported.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CastStatus {
    /// The TLS connection is up.
    pub connected: bool,
    /// The media receiver runs and its channel is open.
    pub ready: bool,
    pub player_state: CastPlayerState,
    /// Why the player is idle (`FINISHED`, `CANCELLED`, `INTERRUPTED`, `ERROR`).
    pub idle_reason: Option<String>,
    /// A LOAD was sent and the receiver hasn't reported that media yet.
    pub loading: bool,
    pub position: Duration,
    /// When `position` was reported (extrapolated while playing).
    pub updated: Option<Instant>,
    pub duration: Option<Duration>,
    /// URL of the media the receiver has.
    pub content_id: Option<String>,
    pub volume: Option<f32>,
    /// Something went wrong (load failed, receiver stopped on the device, …).
    pub error: Option<String>,
    pub media_session_id: Option<i64>,
}

impl CastStatus {
    /// The position now (extrapolated from the last report while playing).
    pub fn position_now(&self) -> Duration {
        let mut position = self.position;
        if self.player_state == CastPlayerState::Playing
            && let Some(at) = self.updated
        {
            position += at.elapsed();
        }
        match self.duration {
            Some(d) => position.min(d),
            None => position,
        }
    }
}

enum Command {
    Load {
        media: Box<MediaInfo>,
        start: Duration,
        autoplay: bool,
    },
    Play,
    Pause,
    Seek(Duration),
    Stop,
    Volume(f32),
}

struct Shared {
    status: Mutex<CastStatus>,
    shutdown: AtomicBool,
    /// Stop the receiver app when closing.
    stop_app: AtomicBool,
}

impl Shared {
    fn update(&self, f: impl FnOnce(&mut CastStatus)) {
        f(&mut self.status.lock().unwrap_or_else(PoisonError::into_inner));
    }
}

/// A connected Cast device. Dropping it closes the connection (and, unless
/// [`Self::keep_app_running`] was called, stops the media receiver).
pub struct CastSession {
    shared: Arc<Shared>,
    commands: Sender<Command>,
    thread: Option<JoinHandle<()>>,
}

type Tls = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

impl CastSession {
    /// Connects (TLS; Cast devices use self-signed certificates, which is why
    /// none is verified — the LAN address came from discovery) and launches
    /// the Default Media Receiver. Blocking (≤ a few seconds).
    pub fn connect(addr: SocketAddr) -> Result<Self> {
        let tcp = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
        tcp.set_nodelay(true)?;
        tcp.set_write_timeout(Some(CONNECT_TIMEOUT))?;
        tcp.set_read_timeout(Some(CONNECT_TIMEOUT))?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(|e| Error::Parse(format!("TLS: {e}")))?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
            .with_no_client_auth();
        let name = ServerName::IpAddress(addr.ip().into());
        let conn = rustls::ClientConnection::new(Arc::new(config), name)
            .map_err(|e| Error::Parse(format!("TLS: {e}")))?;
        let mut tls = rustls::StreamOwned::new(conn, tcp);
        // Finish the handshake now, so connection errors surface here.
        while tls.conn.is_handshaking() {
            tls.conn
                .complete_io(&mut tls.sock)
                .map_err(|e| Error::Parse(format!("TLS handshake with {addr}: {e}")))?;
        }
        tls.sock.set_read_timeout(Some(POLL))?;
        let shared = Arc::new(Shared {
            status: Mutex::new(CastStatus {
                connected: true,
                ..CastStatus::default()
            }),
            shutdown: AtomicBool::new(false),
            stop_app: AtomicBool::new(true),
        });
        let (tx, rx) = channel();
        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("cast-session".into())
            .spawn(move || Worker::new(tls, thread_shared, rx).run())
            .map_err(Error::Io)?;
        Ok(Self {
            shared,
            commands: tx,
            thread: Some(thread),
        })
    }

    pub fn status(&self) -> CastStatus {
        self.shared
            .status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Plays `media` from `start` (queued until the receiver is ready).
    pub fn load(&self, media: MediaInfo, start: Duration, autoplay: bool) {
        self.shared.update(|s| {
            s.loading = true;
            s.error = None;
        });
        let _ = self.commands.send(Command::Load {
            media: Box::new(media),
            start,
            autoplay,
        });
    }

    pub fn play(&self) {
        let _ = self.commands.send(Command::Play);
    }

    pub fn pause(&self) {
        let _ = self.commands.send(Command::Pause);
    }

    pub fn seek(&self, position: Duration) {
        let _ = self.commands.send(Command::Seek(position));
    }

    /// Stops the media (the receiver app keeps running).
    pub fn stop(&self) {
        let _ = self.commands.send(Command::Stop);
    }

    /// The device volume, 0.0..=1.0.
    pub fn set_volume(&self, level: f32) {
        let _ = self.commands.send(Command::Volume(level.clamp(0.0, 1.0)));
    }

    /// Leave the receiver app running when this session closes.
    pub fn keep_app_running(&self) {
        self.shared.stop_app.store(false, Ordering::SeqCst);
    }
}

impl Drop for CastSession {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Worker {
    tls: Tls,
    shared: Arc<Shared>,
    commands: Receiver<Command>,
    frames: FrameReader,
    request_id: u64,
    /// The media receiver's session and transport (its channel id).
    app: Option<(String, String)>,
    launch_sent: bool,
    pending_load: Option<Command>,
}

impl Worker {
    fn new(tls: Tls, shared: Arc<Shared>, commands: Receiver<Command>) -> Self {
        Self {
            tls,
            shared,
            commands,
            frames: FrameReader::default(),
            request_id: 0,
            app: None,
            launch_sent: false,
            pending_load: None,
        }
    }

    fn send(&mut self, destination: &str, namespace: &str, payload: Value) -> bool {
        let frame = CastMessage::new(SENDER, destination, namespace, payload.to_string()).frame();
        self.tls
            .write_all(&frame)
            .and_then(|()| self.tls.flush())
            .is_ok()
    }

    fn request(&mut self, destination: &str, namespace: &str, mut payload: Value) -> bool {
        self.request_id += 1;
        payload["requestId"] = json!(self.request_id);
        self.send(destination, namespace, payload)
    }

    fn media(&mut self, payload: Value) -> bool {
        let Some((_, transport)) = self.app.clone() else {
            return false;
        };
        self.request(&transport, NS_MEDIA, payload)
    }

    fn media_session(&self) -> Option<i64> {
        self.shared
            .status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .media_session_id
    }

    fn fail(&self, error: String) {
        self.shared.update(|s| {
            s.error = Some(error);
            s.loading = false;
        });
    }

    fn run(mut self) {
        self.send(RECEIVER, NS_CONNECTION, json!({ "type": "CONNECT" }));
        self.request(RECEIVER, NS_RECEIVER, json!({ "type": "GET_STATUS" }));
        let mut last_ping = Instant::now();
        let mut last_status = Instant::now();
        let mut last_heard = Instant::now();
        let mut buf = vec![0u8; 16 * 1024];
        let reason = loop {
            if self.shared.shutdown.load(Ordering::Relaxed) {
                self.close();
                break None;
            }
            while let Ok(command) = self.commands.try_recv() {
                self.handle_command(command);
            }
            if last_ping.elapsed() >= PING_EVERY {
                last_ping = Instant::now();
                self.send(RECEIVER, NS_HEARTBEAT, json!({ "type": "PING" }));
            }
            if self.app.is_some() && last_status.elapsed() >= STATUS_EVERY {
                last_status = Instant::now();
                self.media(json!({ "type": "GET_STATUS" }));
            }
            match self.tls.read(&mut buf) {
                Ok(0) => break Some("The device closed the connection".to_owned()),
                Ok(n) => {
                    last_heard = Instant::now();
                    self.frames.push(&buf[..n]);
                    loop {
                        match self.frames.next() {
                            Ok(Some(message)) => self.handle_message(message),
                            Ok(None) => break,
                            Err(e) => {
                                log::warn!("cast: {e}");
                                break;
                            }
                        }
                    }
                }
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    if last_heard.elapsed() > DEVICE_TIMEOUT {
                        break Some("The device stopped responding".to_owned());
                    }
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => break Some(format!("Connection lost: {e}")),
            }
        };
        self.shared.update(|s| {
            s.connected = false;
            s.ready = false;
            s.loading = false;
            if let Some(reason) = reason {
                s.error.get_or_insert(reason);
            }
        });
    }

    fn close(&mut self) {
        if let Some((session, transport)) = self.app.clone() {
            if self.shared.stop_app.load(Ordering::Relaxed) {
                self.request(
                    RECEIVER,
                    NS_RECEIVER,
                    json!({ "type": "STOP", "sessionId": session }),
                );
            }
            self.send(&transport, NS_CONNECTION, json!({ "type": "CLOSE" }));
        }
        self.send(RECEIVER, NS_CONNECTION, json!({ "type": "CLOSE" }));
        self.tls.conn.send_close_notify();
        let _ = self.tls.flush();
    }

    fn handle_command(&mut self, command: Command) {
        match command {
            Command::Load { .. } if self.app.is_none() => {
                // Once the receiver runs (launched on the first load).
                self.pending_load = Some(command);
                if !self.launch_sent {
                    self.launch_sent = true;
                    self.request(
                        RECEIVER,
                        NS_RECEIVER,
                        json!({ "type": "LAUNCH", "appId": DEFAULT_MEDIA_RECEIVER }),
                    );
                }
            }
            Command::Load {
                media,
                start,
                autoplay,
            } => {
                let session = self.app.as_ref().map(|(s, _)| s.clone());
                let mut metadata = json!({
                    "metadataType": 3, // MUSIC_TRACK
                    "title": media.title,
                    "artist": media.artist,
                });
                if let Some(album) = &media.album {
                    metadata["albumName"] = json!(album);
                }
                if let Some(image) = &media.image {
                    metadata["images"] = json!([{ "url": image }]);
                }
                let mut info = json!({
                    "contentId": media.url,
                    "contentType": media.content_type,
                    "streamType": "BUFFERED",
                    "metadata": metadata,
                });
                if let Some(d) = media.duration {
                    info["duration"] = json!(d.as_secs_f64());
                }
                self.shared.update(|s| {
                    s.loading = true;
                    s.error = None;
                    s.content_id = Some(media.url.clone());
                });
                self.media(json!({
                    "type": "LOAD",
                    "sessionId": session,
                    "media": info,
                    "autoplay": autoplay,
                    "currentTime": start.as_secs_f64(),
                }));
            }
            Command::Play | Command::Pause | Command::Stop => {
                let kind = match command {
                    Command::Play => "PLAY",
                    Command::Pause => "PAUSE",
                    _ => "STOP",
                };
                if let Some(id) = self.media_session() {
                    self.media(json!({ "type": kind, "mediaSessionId": id }));
                }
            }
            Command::Seek(position) => {
                if let Some(id) = self.media_session() {
                    self.media(json!({
                        "type": "SEEK",
                        "mediaSessionId": id,
                        "currentTime": position.as_secs_f64(),
                    }));
                    // Show the new position right away.
                    self.shared.update(|s| {
                        s.position = position;
                        s.updated = Some(Instant::now());
                    });
                }
            }
            Command::Volume(level) => {
                self.request(
                    RECEIVER,
                    NS_RECEIVER,
                    json!({ "type": "SET_VOLUME", "volume": { "level": level } }),
                );
            }
        }
    }

    fn handle_message(&mut self, message: CastMessage) {
        let payload: Value = serde_json::from_str(&message.payload).unwrap_or_default();
        let kind = payload.get("type").and_then(Value::as_str).unwrap_or("");
        match (message.namespace.as_str(), kind) {
            (NS_HEARTBEAT, "PING") => {
                self.send(&message.source, NS_HEARTBEAT, json!({ "type": "PONG" }));
            }
            (NS_RECEIVER, "RECEIVER_STATUS") => self.receiver_status(&payload),
            (NS_RECEIVER, "LAUNCH_ERROR") => {
                let reason = payload
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                self.fail(format!(
                    "The device couldn't start the media player ({reason})"
                ));
            }
            (NS_CONNECTION, "CLOSE")
                if self.app.as_ref().is_some_and(|(_, t)| *t == message.source) =>
            {
                self.app = None;
                self.launch_sent = false;
                self.shared.update(|s| {
                    s.ready = false;
                    s.player_state = CastPlayerState::Idle;
                    s.error = Some("Casting was stopped on the device".into());
                });
            }
            (NS_MEDIA, "MEDIA_STATUS") => self.media_status(&payload),
            (
                NS_MEDIA,
                "LOAD_FAILED" | "LOAD_CANCELLED" | "INVALID_REQUEST" | "INVALID_PLAYER_STATE",
            ) => {
                let detail = payload
                    .get("reason")
                    .or_else(|| payload.get("detailedErrorCode"))
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                self.fail(format!(
                    "The device refused the media ({kind}{}{detail})",
                    if detail.is_empty() { "" } else { " " }
                ));
            }
            _ => {}
        }
    }

    fn receiver_status(&mut self, payload: &Value) {
        let status = &payload["status"];
        let volume = status
            .pointer("/volume/level")
            .and_then(Value::as_f64)
            .map(|v| v as f32);
        let app = status
            .get("applications")
            .and_then(Value::as_array)
            .and_then(|apps| {
                apps.iter().find(|a| {
                    a.get("appId").and_then(Value::as_str) == Some(DEFAULT_MEDIA_RECEIVER)
                })
            })
            .and_then(|a| {
                Some((
                    a.get("sessionId")?.as_str()?.to_owned(),
                    a.get("transportId")?.as_str()?.to_owned(),
                ))
            });
        self.shared.update(|s| {
            if volume.is_some() {
                s.volume = volume;
            }
        });
        match app {
            Some(app) if self.app.as_ref() != Some(&app) => {
                self.send(&app.1, NS_CONNECTION, json!({ "type": "CONNECT" }));
                self.app = Some(app);
                self.shared.update(|s| s.ready = true);
                self.media(json!({ "type": "GET_STATUS" }));
                if let Some(load) = self.pending_load.take() {
                    self.handle_command(load);
                }
            }
            Some(_) => {}
            None if self.app.is_some() => {
                // Another app took over the device, or it was stopped there.
                self.app = None;
                self.launch_sent = false;
                self.shared.update(|s| {
                    s.ready = false;
                    s.player_state = CastPlayerState::Idle;
                    s.error = Some("Casting was stopped on the device".into());
                });
            }
            None => {}
        }
    }

    fn media_status(&mut self, payload: &Value) {
        let Some(status) = payload
            .get("status")
            .and_then(Value::as_array)
            .and_then(|s| s.first())
        else {
            // No media session (after STOP, or before the first LOAD).
            self.shared.update(|s| {
                if !s.loading {
                    s.player_state = CastPlayerState::Idle;
                    s.media_session_id = None;
                }
            });
            return;
        };
        let state = match status.get("playerState").and_then(Value::as_str) {
            Some("PLAYING") => CastPlayerState::Playing,
            Some("PAUSED") => CastPlayerState::Paused,
            Some("BUFFERING" | "LOADING") => CastPlayerState::Buffering,
            _ => CastPlayerState::Idle,
        };
        let content = status
            .pointer("/media/contentId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let duration = status
            .pointer("/media/duration")
            .and_then(Value::as_f64)
            .filter(|d| d.is_finite() && *d > 0.0)
            .map(Duration::from_secs_f64);
        let position = status
            .get("currentTime")
            .and_then(Value::as_f64)
            .filter(|t| t.is_finite() && *t >= 0.0)
            .map(Duration::from_secs_f64);
        let idle_reason = status
            .get("idleReason")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let session = status.get("mediaSessionId").and_then(Value::as_i64);
        self.shared.update(|s| {
            // A status for the media just loaded ends the loading phase.
            if content.is_some() && content == s.content_id {
                s.loading = false;
            } else if content.is_some() {
                s.content_id = content;
                s.loading = false;
            }
            s.player_state = state;
            if session.is_some() {
                s.media_session_id = session;
            }
            if let Some(d) = duration {
                s.duration = Some(d);
            }
            if let Some(p) = position {
                s.position = p;
                s.updated = Some(Instant::now());
            }
            s.idle_reason = (state == CastPlayerState::Idle)
                .then_some(idle_reason)
                .flatten();
            if s.idle_reason.as_deref() == Some("ERROR") {
                s.error = Some("The device couldn't play the media".into());
            }
        });
    }
}

/// Cast devices present self-signed certificates; the signature is still checked.
#[derive(Debug)]
struct AnyCertificate(Arc<CryptoProvider>);

impl ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::net::TcpListener;

    use rustls::pki_types::PrivateKeyDer;
    use rustls::pki_types::pem::PemObject as _;

    use super::*;

    // A throwaway self-signed P-256 certificate for the fake receiver (tests only).
    const TEST_KEY: &str = include_str!("../../tests/fixtures/cast/fake-receiver-key.pem");
    const TEST_CERT: &str = include_str!("../../tests/fixtures/cast/fake-receiver-cert.pem");

    /// What the fake receiver saw: (namespace, destination, payload).
    pub(crate) type Seen = Arc<Mutex<Vec<(String, String, Value)>>>;

    /// A fake Cast device on localhost speaking CASTV2 over TLS: launches the
    /// media receiver, answers LOAD / PLAY / PAUSE / SEEK / STOP with media
    /// statuses, pings, and closes the app on demand (`stop_on_device`).
    pub(crate) fn fake_receiver() -> (SocketAddr, Seen, Arc<AtomicBool>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Seen = Arc::default();
        let stop_on_device = Arc::new(AtomicBool::new(false));
        let (log, stop) = (seen.clone(), stop_on_device.clone());
        std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let config = rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from_pem_slice(TEST_CERT.as_bytes()).unwrap()],
                PrivateKeyDer::from_pem_slice(TEST_KEY.as_bytes()).unwrap(),
            )
            .unwrap();
            let conn = rustls::ServerConnection::new(Arc::new(config)).unwrap();
            let mut tls = rustls::StreamOwned::new(conn, tcp);
            tls.sock
                .set_read_timeout(Some(Duration::from_millis(50)))
                .unwrap();
            let mut frames = FrameReader::default();
            let mut buf = vec![0u8; 16 * 1024];
            let mut launched = false;
            let mut media: Option<(String, f64, &str)> = None; // (url, time, state)
            let send = |tls: &mut rustls::StreamOwned<rustls::ServerConnection, TcpStream>,
                        from: &str,
                        ns: &str,
                        payload: Value| {
                let frame = CastMessage::new(from, SENDER, ns, payload.to_string()).frame();
                let _ = tls.write_all(&frame);
                let _ = tls.flush();
            };
            let media_status = |media: &Option<(String, f64, &str)>, request: u64| match media {
                Some((url, time, state)) => json!({
                    "type": "MEDIA_STATUS", "requestId": request,
                    "status": [{ "mediaSessionId": 7, "playerState": state, "currentTime": time,
                                 "idleReason": if *state == "IDLE" { Some("CANCELLED") } else { None },
                                 "media": { "contentId": url, "duration": 200.0 } }]
                }),
                None => json!({ "type": "MEDIA_STATUS", "requestId": request, "status": [] }),
            };
            let receiver_status = |launched: bool, request: u64| {
                let apps = if launched {
                    json!([{ "appId": DEFAULT_MEDIA_RECEIVER, "sessionId": "sess-1",
                             "transportId": "transport-1", "displayName": "Default Media Receiver" }])
                } else {
                    json!([])
                };
                json!({ "type": "RECEIVER_STATUS", "requestId": request,
                        "status": { "applications": apps, "volume": { "level": 0.4, "muted": false } } })
            };
            loop {
                if stop.swap(false, Ordering::SeqCst) {
                    launched = false;
                    send(
                        &mut tls,
                        "transport-1",
                        NS_CONNECTION,
                        json!({ "type": "CLOSE" }),
                    );
                    send(&mut tls, RECEIVER, NS_RECEIVER, receiver_status(false, 0));
                }
                match tls.read(&mut buf) {
                    Ok(0) => return,
                    Ok(n) => frames.push(&buf[..n]),
                    Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                        continue;
                    }
                    Err(_) => return,
                }
                while let Ok(Some(m)) = frames.next() {
                    let p: Value = serde_json::from_str(&m.payload).unwrap();
                    log.lock().unwrap().push((
                        m.namespace.clone(),
                        m.destination.clone(),
                        p.clone(),
                    ));
                    let id = p["requestId"].as_u64().unwrap_or(0);
                    match (m.namespace.as_str(), p["type"].as_str().unwrap_or("")) {
                        (NS_HEARTBEAT, "PING") => {
                            send(&mut tls, RECEIVER, NS_HEARTBEAT, json!({ "type": "PONG" }))
                        }
                        (NS_RECEIVER, "GET_STATUS") => send(
                            &mut tls,
                            RECEIVER,
                            NS_RECEIVER,
                            receiver_status(launched, id),
                        ),
                        (NS_RECEIVER, "LAUNCH") => {
                            launched = true;
                            send(&mut tls, RECEIVER, NS_RECEIVER, receiver_status(true, id));
                        }
                        (NS_RECEIVER, "SET_VOLUME") => send(
                            &mut tls,
                            RECEIVER,
                            NS_RECEIVER,
                            json!({ "type": "RECEIVER_STATUS", "requestId": id,
                                    "status": { "applications": [{ "appId": DEFAULT_MEDIA_RECEIVER,
                                    "sessionId": "sess-1", "transportId": "transport-1" }],
                                    "volume": { "level": p["volume"]["level"] } } }),
                        ),
                        (NS_MEDIA, kind) => {
                            match kind {
                                "LOAD" if p["media"]["contentType"] == "audio/bogus" => {
                                    send(
                                        &mut tls,
                                        "transport-1",
                                        NS_MEDIA,
                                        json!({ "type": "LOAD_FAILED", "requestId": id }),
                                    );
                                    continue;
                                }
                                "LOAD" => {
                                    media = Some((
                                        p["media"]["contentId"].as_str().unwrap().to_owned(),
                                        p["currentTime"].as_f64().unwrap(),
                                        "PLAYING",
                                    ))
                                }
                                "PAUSE" => media.iter_mut().for_each(|m| m.2 = "PAUSED"),
                                "PLAY" => media.iter_mut().for_each(|m| m.2 = "PLAYING"),
                                "SEEK" => media
                                    .iter_mut()
                                    .for_each(|m| m.1 = p["currentTime"].as_f64().unwrap()),
                                "STOP" => media.iter_mut().for_each(|m| m.2 = "IDLE"),
                                _ => {}
                            }
                            send(&mut tls, "transport-1", NS_MEDIA, media_status(&media, id));
                        }
                        _ => {}
                    }
                }
            }
        });
        (addr, seen, stop_on_device)
    }

    pub(crate) fn wait_for<T>(mut f: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(v) = f() {
                return v;
            }
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn launches_loads_and_controls_a_fake_receiver_over_tls() {
        let (addr, seen, stop_on_device) = fake_receiver();
        let session = CastSession::connect(addr).unwrap();
        assert!(session.status().connected);
        session.load(
            MediaInfo {
                url: "http://192.168.1.2:4000/media/abc".into(),
                content_type: "audio/mp4".into(),
                title: "Instant Crush".into(),
                artist: "Daft Punk".into(),
                album: Some("Random Access Memories".into()),
                image: Some("https://lh3.googleusercontent.com/x=w544".into()),
                duration: Some(Duration::from_secs(337)),
            },
            Duration::from_secs(42),
            true,
        );
        assert!(session.status().loading);
        let status = wait_for(|| {
            let s = session.status();
            (s.player_state == CastPlayerState::Playing && !s.loading).then_some(s)
        });
        assert!(status.ready);
        assert_eq!(status.media_session_id, Some(7));
        assert_eq!(status.volume, Some(0.4));
        assert_eq!(status.duration, Some(Duration::from_secs(200)));
        assert!(status.position_now() >= Duration::from_secs(42));

        session.pause();
        wait_for(|| (session.status().player_state == CastPlayerState::Paused).then_some(()));
        session.seek(Duration::from_secs(90));
        wait_for(|| (session.status().position == Duration::from_secs(90)).then_some(()));
        session.play();
        wait_for(|| (session.status().player_state == CastPlayerState::Playing).then_some(()));
        session.set_volume(0.8);
        wait_for(|| (session.status().volume == Some(0.8)).then_some(()));
        session.stop();
        let idle = wait_for(|| {
            let s = session.status();
            (s.player_state == CastPlayerState::Idle).then_some(s)
        });
        assert_eq!(idle.idle_reason.as_deref(), Some("CANCELLED"));

        // A refused LOAD is an error, not a hang.
        session.load(
            MediaInfo {
                url: "http://x/y".into(),
                content_type: "audio/bogus".into(),
                ..MediaInfo::default()
            },
            Duration::ZERO,
            true,
        );
        let failed = wait_for(|| session.status().error);
        assert!(failed.contains("LOAD_FAILED"), "{failed}");
        assert!(!session.status().loading);

        // What went over the wire.
        {
            let seen = seen.lock().unwrap();
            let find = |kind: &str| {
                seen.iter()
                    .find(|(_, _, p)| p["type"] == kind)
                    .cloned()
                    .unwrap_or_else(|| panic!("no {kind}"))
            };
            let (ns, to, launch) = find("LAUNCH");
            assert_eq!((ns.as_str(), to.as_str()), (NS_RECEIVER, RECEIVER));
            assert_eq!(launch["appId"], DEFAULT_MEDIA_RECEIVER);
            let (ns, to, load) = find("LOAD");
            assert_eq!((ns.as_str(), to.as_str()), (NS_MEDIA, "transport-1"));
            assert_eq!(load["sessionId"], "sess-1");
            assert_eq!(load["currentTime"], 42.0);
            assert_eq!(load["autoplay"], true);
            assert_eq!(load["media"]["streamType"], "BUFFERED");
            assert_eq!(load["media"]["metadata"]["metadataType"], 3);
            assert_eq!(
                load["media"]["metadata"]["albumName"],
                "Random Access Memories"
            );
            assert_eq!(load["media"]["duration"], 337.0);
            let (_, _, seek) = find("SEEK");
            assert_eq!(
                (
                    seek["mediaSessionId"].as_i64(),
                    seek["currentTime"].as_f64()
                ),
                (Some(7), Some(90.0))
            );
            // CONNECT to the receiver and to the app's transport.
            let connects: Vec<String> = seen
                .iter()
                .filter(|(ns, _, p)| ns == NS_CONNECTION && p["type"] == "CONNECT")
                .map(|(_, to, _)| to.clone())
                .collect();
            assert_eq!(
                connects,
                vec![RECEIVER.to_owned(), "transport-1".to_owned()]
            );
        }

        // Stopped on the device: reported as such.
        stop_on_device.store(true, Ordering::SeqCst);
        let error = wait_for(|| {
            let s = session.status();
            (!s.ready).then_some(s.error)
        });
        assert_eq!(error.as_deref(), Some("Casting was stopped on the device"));
        drop(session);
    }
}
