//! A pretend Chromecast for trying the app's casting without a device:
//!
//! ```sh
//! cargo run -p music-core --example fake_cast_receiver -- 8010
//! # in the app: MUSIC_SCRIPT="…,@cast=127.0.0.1:8010:Fake TV,…"
//! ```
//!
//! Speaks CASTV2 over TLS (a throwaway self-signed certificate), "launches" the
//! Default Media Receiver, and on LOAD downloads the media URL the way a
//! receiver would (`Range: bytes=0-`), then plays it on a wall clock: PLAYING /
//! PAUSED / SEEK / STOP, FINISHED at the end (`FAKE_CAST_SPEED=10` runs the
//! clock ten times faster). Everything it does is printed.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde_json::{Value, json};

const KEY: &str = include_str!("../tests/fixtures/cast/fake-receiver-key.pem");
const CERT: &str = include_str!("../tests/fixtures/cast/fake-receiver-cert.pem");
const NS_CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";
const NS_HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
const NS_RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";
const NS_MEDIA: &str = "urn:x-cast:com.google.cast.media";

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn frame(from: &str, to: &str, ns: &str, payload: &Value) -> Vec<u8> {
    let mut body = vec![0x08, 0x00];
    for (field, s) in [(2u64, from), (3, to), (4, ns)] {
        varint(&mut body, field << 3 | 2);
        varint(&mut body, s.len() as u64);
        body.extend_from_slice(s.as_bytes());
    }
    body.extend_from_slice(&[0x28, 0x00]);
    let text = payload.to_string();
    varint(&mut body, 6 << 3 | 2);
    varint(&mut body, text.len() as u64);
    body.extend_from_slice(text.as_bytes());
    let mut out = (body.len() as u32).to_be_bytes().to_vec();
    out.extend(body);
    out
}

/// (source, namespace, payload) of one message body.
fn parse(buf: &[u8]) -> Option<(String, String, Value)> {
    let (mut pos, mut fields) = (
        0,
        [String::new(), String::new(), String::new(), String::new()],
    );
    let read_varint = |pos: &mut usize| -> Option<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = *buf.get(*pos)?;
            *pos += 1;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    };
    while pos < buf.len() {
        let key = read_varint(&mut pos)?;
        if key & 7 == 0 {
            read_varint(&mut pos)?;
            continue;
        }
        let len = read_varint(&mut pos)? as usize;
        let text = String::from_utf8_lossy(buf.get(pos..pos + len)?).into_owned();
        pos += len;
        match key >> 3 {
            2 => fields[0] = text,
            4 => fields[2] = text,
            6 => fields[3] = text,
            _ => {}
        }
    }
    Some((
        fields[0].clone(),
        fields[2].clone(),
        serde_json::from_str(&fields[3]).ok()?,
    ))
}

struct Media {
    url: String,
    duration: f64,
    /// Seconds at `since` (None = paused).
    base: f64,
    since: Option<Instant>,
    idle: Option<&'static str>,
    fetched: Arc<AtomicU64>,
}

impl Media {
    fn time(&self, speed: f64) -> f64 {
        let t = self.base
            + self
                .since
                .map_or(0.0, |s| s.elapsed().as_secs_f64() * speed);
        t.min(self.duration)
    }
}

fn serve(tcp: TcpStream, speed: f64) {
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![CertificateDer::from_pem_slice(CERT.as_bytes()).unwrap()],
        PrivateKeyDer::from_pem_slice(KEY.as_bytes()).unwrap(),
    )
    .unwrap();
    let mut tls = rustls::StreamOwned::new(
        rustls::ServerConnection::new(Arc::new(config)).unwrap(),
        tcp,
    );
    tls.sock
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let mut pending = Vec::new();
    let mut buf = vec![0u8; 16 * 1024];
    let mut launched = false;
    let media: Arc<Mutex<Option<Media>>> = Arc::default();
    let mut last_report = Instant::now();
    let mut reported_end = false;
    let status_of = |m: &Option<Media>, id: u64| -> Value {
        match m {
            Some(m) => json!({ "type": "MEDIA_STATUS", "requestId": id, "status": [{
                "mediaSessionId": 1,
                "playerState": if m.idle.is_some() { "IDLE" } else if m.since.is_some() {
                    if m.fetched.load(Ordering::Relaxed) == 0 { "BUFFERING" } else { "PLAYING" }
                } else { "PAUSED" },
                "idleReason": m.idle,
                "currentTime": m.time(speed),
                "media": { "contentId": m.url, "duration": m.duration },
            }]}),
            None => json!({ "type": "MEDIA_STATUS", "requestId": id, "status": [] }),
        }
    };
    loop {
        match tls.read(&mut buf) {
            Ok(0) => {
                eprintln!("fake-cast: sender left");
                return;
            }
            Ok(n) => pending.extend_from_slice(&buf[..n]),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => {
                eprintln!("fake-cast: {e}");
                return;
            }
        }
        // The end of the track, and a status every few seconds while playing.
        let mut out = Vec::new();
        {
            let mut m = media.lock().unwrap();
            if let Some(m) = m.as_mut()
                && m.idle.is_none()
                && m.since.is_some()
                && m.time(speed) >= m.duration
            {
                m.idle = Some("FINISHED");
                m.since = None;
                m.base = m.duration;
                reported_end = false;
                eprintln!("fake-cast: finished {}", m.url);
            }
            if !reported_end && m.as_ref().is_some_and(|m| m.idle.is_some())
                || last_report.elapsed() > Duration::from_secs(3)
            {
                reported_end = true;
                last_report = Instant::now();
                out.push(frame(
                    "transport-1",
                    "sender-0",
                    NS_MEDIA,
                    &status_of(&m, 0),
                ));
            }
        }
        while pending.len() >= 4 {
            let len = u32::from_be_bytes([pending[0], pending[1], pending[2], pending[3]]) as usize;
            if pending.len() < 4 + len {
                break;
            }
            let body: Vec<u8> = pending.drain(..4 + len).skip(4).collect();
            let Some((from, ns, p)) = parse(&body) else {
                continue;
            };
            let kind = p["type"].as_str().unwrap_or("").to_owned();
            let id = p["requestId"].as_u64().unwrap_or(0);
            if kind != "PING" && kind != "PONG" && !(kind == "GET_STATUS" && ns == NS_MEDIA) {
                eprintln!("fake-cast: {ns} {kind}");
            }
            let receiver_status = |launched: bool| {
                json!({ "type": "RECEIVER_STATUS", "requestId": id, "status": {
                    "applications": if launched { json!([{ "appId": "CC1AD845", "sessionId": "session-1",
                        "transportId": "transport-1", "displayName": "Default Media Receiver" }]) } else { json!([]) },
                    "volume": { "level": 0.5, "muted": false } } })
            };
            match (ns.as_str(), kind.as_str()) {
                (NS_HEARTBEAT, "PING") => out.push(frame(
                    "receiver-0",
                    &from,
                    NS_HEARTBEAT,
                    &json!({ "type": "PONG" }),
                )),
                (NS_RECEIVER, "GET_STATUS") => out.push(frame(
                    "receiver-0",
                    &from,
                    NS_RECEIVER,
                    &receiver_status(launched),
                )),
                (NS_RECEIVER, "LAUNCH") => {
                    launched = true;
                    out.push(frame(
                        "receiver-0",
                        &from,
                        NS_RECEIVER,
                        &receiver_status(true),
                    ));
                }
                (NS_RECEIVER, "STOP") => {
                    launched = false;
                    *media.lock().unwrap() = None;
                    out.push(frame(
                        "receiver-0",
                        &from,
                        NS_RECEIVER,
                        &receiver_status(false),
                    ));
                }
                (NS_RECEIVER, "SET_VOLUME") => {
                    eprintln!("fake-cast:   volume {}", p["volume"]["level"]);
                }
                (NS_CONNECTION, _) => {}
                (NS_MEDIA, "LOAD") => {
                    let url = p["media"]["contentId"].as_str().unwrap_or("").to_owned();
                    let duration = p["media"]["duration"].as_f64().unwrap_or(30.0);
                    let start = p["currentTime"].as_f64().unwrap_or(0.0);
                    eprintln!(
                        "fake-cast:   “{}” — {} ({}) from {start:.1} s of {duration:.0} s, {url}",
                        p["media"]["metadata"]["title"].as_str().unwrap_or("?"),
                        p["media"]["metadata"]["artist"].as_str().unwrap_or("?"),
                        p["media"]["contentType"].as_str().unwrap_or("?"),
                    );
                    let fetched = Arc::new(AtomicU64::new(0));
                    let counter = fetched.clone();
                    let target = url.clone();
                    std::thread::spawn(move || {
                        let started = Instant::now();
                        let resp = ureq::Agent::new_with_defaults()
                            .get(&target)
                            .header("Range", "bytes=0-")
                            .header(
                                "User-Agent",
                                "Mozilla/5.0 (X11; Linux armv7l) CrKey/1.56 (fake)",
                            )
                            .call();
                        match resp {
                            Ok(mut resp) => {
                                let status = resp.status();
                                let mut reader = resp.body_mut().as_reader();
                                let mut piece = vec![0u8; 64 * 1024];
                                while let Ok(n) = reader.read(&mut piece) {
                                    if n == 0 {
                                        break;
                                    }
                                    counter.fetch_add(n as u64, Ordering::Relaxed);
                                }
                                eprintln!(
                                    "fake-cast:   fetched {} bytes ({status}) in {:.1} s",
                                    counter.load(Ordering::Relaxed),
                                    started.elapsed().as_secs_f64()
                                );
                            }
                            Err(e) => eprintln!("fake-cast:   fetch failed: {e}"),
                        }
                    });
                    let mut m = media.lock().unwrap();
                    *m = Some(Media {
                        url,
                        duration,
                        base: start,
                        since: p["autoplay"].as_bool().unwrap_or(true).then(Instant::now),
                        idle: None,
                        fetched,
                    });
                    out.push(frame("transport-1", &from, NS_MEDIA, &status_of(&m, id)));
                }
                (NS_MEDIA, other) => {
                    let mut m = media.lock().unwrap();
                    if let Some(m) = m.as_mut() {
                        let now = m.time(speed);
                        match other {
                            "PAUSE" => {
                                m.base = now;
                                m.since = None;
                            }
                            "PLAY" => {
                                m.base = now;
                                m.since = Some(Instant::now());
                                m.idle = None;
                            }
                            "SEEK" => {
                                m.base = p["currentTime"].as_f64().unwrap_or(now);
                                if m.since.is_some() {
                                    m.since = Some(Instant::now());
                                }
                                eprintln!("fake-cast:   to {:.1} s", m.base);
                            }
                            "STOP" => {
                                m.idle = Some("CANCELLED");
                                m.since = None;
                            }
                            _ => {}
                        }
                    }
                    out.push(frame("transport-1", &from, NS_MEDIA, &status_of(&m, id)));
                }
                _ => {}
            }
        }
        for f in out {
            if tls.write_all(&f).and_then(|()| tls.flush()).is_err() {
                return;
            }
        }
    }
}

fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(8010);
    let speed: f64 = std::env::var("FAKE_CAST_SPEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1.0);
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind");
    eprintln!("fake-cast: listening on 127.0.0.1:{port} (clock ×{speed})");
    for tcp in listener.incoming().flatten() {
        eprintln!("fake-cast: sender connected");
        std::thread::spawn(move || serve(tcp, speed));
    }
}
