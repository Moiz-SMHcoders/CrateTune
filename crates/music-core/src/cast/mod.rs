//! Chromecast / Google Cast: find devices on the LAN ([`discover`], mDNS
//! `_googlecast._tcp`), control one over CASTV2 ([`CastSession`]: TLS, the
//! Default Media Receiver, media LOAD / PLAY / PAUSE / SEEK / STOP / volume),
//! and hand it the audio through the app's own HTTP server ([`MediaProxy`]),
//! because googlevideo URLs only work with the User-Agent they were minted for
//! (see `proxy.rs`).
//!
//! Blocking std threads; no async runtime. Tested against a fake CASTV2
//! receiver (TLS on localhost), not against a real device.

mod proto;
mod proxy;
mod session;

use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

pub use proxy::{MediaProxy, content_type_for_path, content_type_of, parse_range};
pub use session::{CastPlayerState, CastSession, CastStatus, DEFAULT_MEDIA_RECEIVER, MediaInfo};

use crate::{Error, Result};

pub const SERVICE_TYPE: &str = "_googlecast._tcp.local.";

/// A Cast device found on the network.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CastDevice {
    /// The name the user gave it ("Living Room TV").
    pub name: String,
    /// "Chromecast", "Google Home Mini", "Nest Audio", …
    pub model: Option<String>,
    /// The device's id (TXT `id`).
    pub id: Option<String>,
    pub addr: SocketAddr,
}

/// The TXT record's friendly name / model / id, or the instance name.
fn device_from(
    fullname: &str,
    name: Option<&str>,
    model: Option<&str>,
    id: Option<&str>,
    addr: SocketAddr,
) -> CastDevice {
    let instance = fullname
        .strip_suffix(SERVICE_TYPE)
        .unwrap_or(fullname)
        .trim_end_matches('.');
    CastDevice {
        name: name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or(instance)
            .to_owned(),
        model: model.filter(|m| !m.is_empty()).map(str::to_owned),
        id: id.filter(|i| !i.is_empty()).map(str::to_owned),
        addr,
    }
}

/// Browses for Cast devices for `timeout` and returns what answered (sorted by
/// name). Blocking.
pub fn discover(timeout: Duration) -> Result<Vec<CastDevice>> {
    let daemon = mdns_sd::ServiceDaemon::new().map_err(|e| Error::Parse(format!("mDNS: {e}")))?;
    let events = daemon
        .browse(SERVICE_TYPE)
        .map_err(|e| Error::Parse(format!("mDNS: {e}")))?;
    let deadline = Instant::now() + timeout;
    let mut devices: Vec<CastDevice> = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Ok(event) = events.recv_timeout(left) else {
            break;
        };
        if let mdns_sd::ServiceEvent::ServiceResolved(info) = event {
            let mut v4: Vec<_> = info.get_addresses_v4().into_iter().collect();
            v4.sort();
            let Some(ip) = v4.first().copied() else {
                continue;
            };
            let device = device_from(
                &info.fullname,
                info.get_property_val_str("fn"),
                info.get_property_val_str("md"),
                info.get_property_val_str("id"),
                SocketAddr::new(IpAddr::V4(ip), info.port),
            );
            if !devices.iter().any(|d| d.addr == device.addr) {
                devices.push(device);
            }
        }
    }
    let _ = daemon.stop_browse(SERVICE_TYPE);
    let _ = daemon.shutdown();
    devices.sort_by_key(|d| d.name.to_lowercase());
    Ok(devices)
}

/// This computer's address as seen from `device` (the interface routing to it).
pub fn local_ip_towards(device: IpAddr) -> Option<IpAddr> {
    let socket = UdpSocket::bind(if device.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    })
    .ok()?;
    socket.connect((device, 9)).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified()).then_some(ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_names_from_txt_or_instance() {
        let addr: SocketAddr = "192.168.1.30:8009".parse().unwrap();
        let d = device_from(
            "Chromecast-0123456789abcdef._googlecast._tcp.local.",
            Some("Living Room"),
            Some("Chromecast"),
            Some("0123456789abcdef"),
            addr,
        );
        assert_eq!(d.name, "Living Room");
        assert_eq!(d.model.as_deref(), Some("Chromecast"));
        let d = device_from(
            "Kitchen-speaker._googlecast._tcp.local.",
            None,
            Some(""),
            None,
            addr,
        );
        assert_eq!((d.name.as_str(), d.model), ("Kitchen-speaker", None));
        assert_eq!(
            local_ip_towards("127.0.0.1".parse().unwrap()),
            Some("127.0.0.1".parse().unwrap())
        );
    }

    /// Fake receiver + proxy: what the app does when casting a stream.
    #[test]
    fn casts_a_proxied_stream_to_a_fake_receiver() {
        use std::net::Ipv4Addr;
        use std::sync::Arc;

        let (addr, seen, _) = session::tests::fake_receiver();
        let data: Arc<[u8]> = (0..50_000u32).map(|i| (i % 7) as u8).collect();
        let upstream = crate::testutil::TestServer::file(data.clone());
        let proxy = MediaProxy::start(IpAddr::V4(Ipv4Addr::LOCALHOST)).unwrap();
        let url = proxy.serve_stream(crate::ResolvedStream {
            url: format!("{}/videoplayback", upstream.base),
            mime_type: "audio/webm; codecs=\"opus\"".into(),
            content_length: Some(data.len() as u64),
            ..crate::ResolvedStream::default()
        });
        let session = CastSession::connect(addr).unwrap();
        session.load(
            MediaInfo {
                url: url.clone(),
                content_type: content_type_of("audio/webm; codecs=\"opus\""),
                title: "T".into(),
                artist: "A".into(),
                ..MediaInfo::default()
            },
            Duration::ZERO,
            true,
        );
        let status = session::tests::wait_for(|| {
            let s = session.status();
            (s.player_state == CastPlayerState::Playing).then_some(s)
        });
        assert_eq!(status.content_id.as_deref(), Some(url.as_str()));
        let load = seen
            .lock()
            .unwrap()
            .iter()
            .find(|(_, _, p)| p["type"] == "LOAD")
            .map(|(_, _, p)| p.clone())
            .unwrap();
        assert_eq!(load["media"]["contentType"], "audio/webm");
        // The receiver would fetch this: the proxy serves the whole stream.
        let mut resp = crate::http::agent().get(&url).call().unwrap();
        assert_eq!(resp.body_mut().read_to_vec().unwrap().as_slice(), &data[..]);
    }
}
