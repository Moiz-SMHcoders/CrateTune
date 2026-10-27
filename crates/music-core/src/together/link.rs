// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/together/
// TogetherLink.kt, © Rukamori and ArchiveTune contributors, GPL-3.0.
//! Join links: `music-together://join?host=…&port=…&sid=…&key=…`, a WebSocket URL
//! (`ws://host:port/together?sid=…&key=…`) or the compact `host|port|sid|key`.

use std::net::{IpAddr, UdpSocket};

use crate::spotify::{parse_query, percent_encode};

pub const LINK_SCHEME: &str = "music-together";
pub const WS_PATH: &str = "/together";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinInfo {
    pub host: String,
    pub port: u16,
    pub session_id: String,
    pub session_key: String,
}

impl JoinInfo {
    pub fn ws_url(&self) -> String {
        let host = if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("ws://{host}:{}{WS_PATH}", self.port)
    }

    pub fn to_link(&self) -> String {
        format!(
            "{LINK_SCHEME}://join?host={}&port={}&sid={}&key={}",
            percent_encode(&self.host),
            self.port,
            percent_encode(&self.session_id),
            percent_encode(&self.session_key)
        )
    }

    pub fn to_compact(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.host, self.port, self.session_id, self.session_key
        )
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        if let Some((scheme, rest)) = raw.split_once("://") {
            let scheme = scheme.to_ascii_lowercase();
            let (authority, query) = rest.split_once('?').unwrap_or((rest, ""));
            let params = parse_query(query.split('#').next().unwrap_or(""));
            let get = |k: &str| {
                params
                    .iter()
                    .find(|(key, _)| key == k)
                    .map(|(_, v)| v.trim().to_owned())
                    .filter(|v| !v.is_empty())
            };
            let (host, port) = if scheme == LINK_SCHEME {
                (get("host")?, get("port")?.parse().ok()?)
            } else if matches!(scheme.as_str(), "ws" | "http") {
                let authority = authority.split('/').next()?;
                split_host_port(authority, 80)?
            } else {
                return None;
            };
            return Self::checked(host, port, get("sid")?, get("key")?);
        }
        let parts: Vec<&str> = raw.split('|').map(str::trim).collect();
        match parts.as_slice() {
            [host, port, sid, key] => Self::checked(
                (*host).into(),
                port.parse().ok()?,
                (*sid).into(),
                (*key).into(),
            ),
            _ => None,
        }
    }

    fn checked(host: String, port: u16, session_id: String, session_key: String) -> Option<Self> {
        let host = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        (!host.is_empty() && port > 0 && !session_id.is_empty() && !session_key.is_empty())
            .then_some(Self {
                host,
                port,
                session_id,
                session_key,
            })
    }
}

fn split_host_port(authority: &str, default_port: u16) -> Option<(String, u16)> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None => default_port,
        };
        return Some((host.to_owned(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Some((host.to_owned(), port.parse().ok()?)),
        None => Some((authority.to_owned(), default_port)),
    }
}

/// This machine's LAN address as other devices would reach it (the source address
/// of the default route; no packet is sent). `None` when offline.
pub fn local_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified()).then_some(ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_round_trip() {
        let info = JoinInfo {
            host: "192.168.1.20".into(),
            port: 42117,
            session_id: "abc-123".into(),
            session_key: "k/y+z".into(),
        };
        let link = info.to_link();
        assert_eq!(
            link,
            "music-together://join?host=192.168.1.20&port=42117&sid=abc-123&key=k%2Fy%2Bz"
        );
        assert_eq!(JoinInfo::parse(&link), Some(info.clone()));
        assert_eq!(JoinInfo::parse(&info.to_compact()), Some(info.clone()));
        assert_eq!(
            JoinInfo::parse("ws://192.168.1.20:42117/together?sid=abc-123&key=k%2Fy%2Bz"),
            Some(info.clone())
        );
        assert_eq!(info.ws_url(), "ws://192.168.1.20:42117/together");
        let v6 = JoinInfo {
            host: "fe80::1".into(),
            ..info
        };
        assert_eq!(v6.ws_url(), "ws://[fe80::1]:42117/together");
        assert_eq!(
            JoinInfo::parse("ws://[fe80::1]:42117/together?sid=abc-123&key=k").map(|i| i.host),
            Some("fe80::1".into())
        );
    }

    #[test]
    fn rejects_incomplete_links() {
        for bad in [
            "",
            "music-together://join?host=h&port=1&sid=s",
            "music-together://join?host=h&port=0&sid=s&key=k",
            "https://example.com/?sid=s&key=k",
            "h|1|s",
            "h|x|s|k",
        ] {
            assert_eq!(JoinInfo::parse(bad), None, "{bad}");
        }
    }
}
