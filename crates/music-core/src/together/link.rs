// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/together/
// TogetherLink.kt, © Rukamori and ArchiveTune contributors, GPL-3.0.
//! Join links: `music-together://join?host=…&port=…&sid=…&key=…` (a host on the
//! LAN), `music-together://join?url=wss%3A…&sid=…&key=…` (a session through a relay),
//! a WebSocket URL (`ws://host:port/together?sid=…&key=…`, `wss://relay…?sid=…&key=…`)
//! or the compact `host|port|sid|key` / `wss://relay…|sid|key`. A relayed link may
//! also carry the room code (`&code=ABCDEFGH`), or only the code
//! (`music-together://join?url=wss%3A…&code=…`): see [`JoinRequest`].

use std::net::{IpAddr, UdpSocket};

use crate::spotify::{parse_query, percent_encode};

pub const LINK_SCHEME: &str = "music-together";
pub const WS_PATH: &str = "/together";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JoinInfo {
    /// The host (or the relay's host name), for display and LAN connections.
    pub host: String,
    pub port: u16,
    pub session_id: String,
    pub session_key: String,
    /// The WebSocket URL to connect to when it isn't `ws://host:port/together`:
    /// a relay (`wss://relay.example.com/…`). `None` for a host on the LAN.
    pub url: Option<String>,
}

impl JoinInfo {
    /// A session reached through the relay at `relay_url` (see [`relay_url`]).
    pub fn via_relay(relay_url: &str, session_id: String, session_key: String) -> Option<Self> {
        let url = relay_url_normalized(relay_url)?;
        let parsed = url::Url::parse(&url).ok()?;
        Some(Self {
            host: parsed.host_str()?.to_owned(),
            port: parsed.port_or_known_default()?,
            session_id,
            session_key,
            url: Some(url),
        })
    }

    pub fn is_relay(&self) -> bool {
        self.url.is_some()
    }

    pub fn ws_url(&self) -> String {
        if let Some(url) = &self.url {
            return url.clone();
        }
        let host = if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("ws://{host}:{}{WS_PATH}", self.port)
    }

    pub fn to_link(&self) -> String {
        match &self.url {
            Some(url) => format!(
                "{LINK_SCHEME}://join?url={}&sid={}&key={}",
                percent_encode(url),
                percent_encode(&self.session_id),
                percent_encode(&self.session_key)
            ),
            None => format!(
                "{LINK_SCHEME}://join?host={}&port={}&sid={}&key={}",
                percent_encode(&self.host),
                self.port,
                percent_encode(&self.session_id),
                percent_encode(&self.session_key)
            ),
        }
    }

    /// [`Self::to_link`] plus the room code (ignored by older versions).
    pub fn to_link_with_code(&self, code: Option<&str>) -> String {
        match code {
            Some(code) => format!("{}&code={}", self.to_link(), percent_encode(code)),
            None => self.to_link(),
        }
    }

    pub fn to_compact(&self) -> String {
        match &self.url {
            Some(url) => format!("{url}|{}|{}", self.session_id, self.session_key),
            None => format!(
                "{}|{}|{}|{}",
                self.host, self.port, self.session_id, self.session_key
            ),
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        if let Some((scheme, rest)) = raw.split_once("://")
            && !rest.contains('|')
        {
            let scheme = scheme.to_ascii_lowercase();
            let query = rest.split_once('?').map_or("", |(_, q)| q);
            let params = parse_query(query.split('#').next().unwrap_or(""));
            let get = |k: &str| {
                params
                    .iter()
                    .find(|(key, _)| key == k)
                    .map(|(_, v)| v.trim().to_owned())
                    .filter(|v| !v.is_empty())
            };
            if scheme == LINK_SCHEME {
                if let Some(url) = get("url") {
                    return Self::from_ws_url(&url, get("sid")?, get("key")?);
                }
                return Self::checked(
                    get("host")?,
                    get("port")?.parse().ok()?,
                    get("sid")?,
                    get("key")?,
                );
            }
            if matches!(scheme.as_str(), "ws" | "wss" | "http" | "https") {
                let base = raw.split(['?', '#']).next().unwrap_or(raw);
                return Self::from_ws_url(base, get("sid")?, get("key")?);
            }
            return None;
        }
        let parts: Vec<&str> = raw.split('|').map(str::trim).collect();
        match parts.as_slice() {
            [host, port, sid, key] => Self::checked(
                (*host).into(),
                port.parse().ok()?,
                (*sid).into(),
                (*key).into(),
            ),
            [url, sid, key] if url.contains("://") => {
                Self::from_ws_url(url, (*sid).into(), (*key).into())
            }
            _ => None,
        }
    }

    /// `ws://host:port/together` is a LAN host; any other WebSocket URL is kept
    /// as is (a relay).
    fn from_ws_url(url: &str, session_id: String, session_key: String) -> Option<Self> {
        let normalized = relay_url_normalized(url)?;
        let parsed = url::Url::parse(&normalized).ok()?;
        let host = parsed.host_str()?.to_owned();
        let port = parsed.port_or_known_default()?;
        let direct = parsed.scheme() == "ws" && parsed.path() == WS_PATH;
        let mut info = Self::checked(host, port, session_id, session_key)?;
        if !direct {
            info.url = Some(normalized);
        }
        Some(info)
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
                url: None,
            })
    }
}

/// What a guest typed or clicked: a full join link, or a room code to look up
/// (at the relay named in the link, else the app's relay).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinRequest {
    Direct(JoinInfo),
    Code { relay: Option<String>, code: String },
}

impl JoinRequest {
    /// A join link / URL / compact form ([`JoinInfo::parse`]), a bare code
    /// (`abcd-efgh`, any case, spaces and dashes ignored), or a code link
    /// `music-together://join?code=…[&url=wss%3A…]`.
    pub fn parse(raw: &str) -> Option<Self> {
        if let Some(info) = JoinInfo::parse(raw) {
            return Some(Self::Direct(info));
        }
        let raw = raw.trim();
        if let Some(code) = super::online::normalize_code(raw) {
            return Some(Self::Code { relay: None, code });
        }
        let (scheme, rest) = raw.split_once("://")?;
        if !scheme.eq_ignore_ascii_case(LINK_SCHEME) {
            return None;
        }
        let query = rest.split_once('?').map_or("", |(_, q)| q);
        let params = parse_query(query.split('#').next().unwrap_or(""));
        let get = |k: &str| {
            params
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let code = super::online::normalize_code(&get("code")?)?;
        let relay = match get("url") {
            Some(url) => Some(relay_url_normalized(&url)?),
            None => None,
        };
        Some(Self::Code { relay, code })
    }

    /// `music-together://join?url=…&code=…` (a link with just the code).
    pub fn code_link(relay: &str, code: &str) -> Option<String> {
        Some(format!(
            "{LINK_SCHEME}://join?url={}&code={}",
            percent_encode(&relay_url_normalized(relay)?),
            percent_encode(code)
        ))
    }
}

/// A relay server address as typed into the settings → its WebSocket URL:
/// `http(s)://` becomes `ws(s)://`; without a scheme, IP addresses and `localhost`
/// get `ws://`, names `wss://` (a relay on the internet sits behind TLS). Query,
/// fragment and credentials are refused / dropped. `None` if it isn't usable.
pub fn relay_url(raw: &str) -> Option<String> {
    relay_url_normalized(raw)
}

fn relay_url_normalized(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let with_scheme = if raw.contains("://") {
        raw.to_owned()
    } else {
        let host = raw.split(['/', '?']).next().unwrap_or(raw);
        let host = match host.rsplit_once(':') {
            Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !h.ends_with(':') => h,
            _ => host,
        };
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let plain = host == "localhost" || host.parse::<IpAddr>().is_ok();
        format!("{}://{raw}", if plain { "ws" } else { "wss" })
    };
    let mut url = url::Url::parse(&with_scheme).ok()?;
    let scheme = match url.scheme() {
        "ws" | "http" => "ws",
        "wss" | "https" => "wss",
        _ => return None,
    };
    if !url.username().is_empty() || url.password().is_some() || url.host_str().is_none() {
        return None;
    }
    url.set_query(None);
    url.set_fragment(None);
    // `Url::set_scheme` refuses http → ws (special → non-special); rebuild instead.
    let rest = &url.as_str()[url.scheme().len()..];
    let out = format!("{scheme}{rest}");
    url::Url::parse(&out).ok()?;
    Some(out)
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
            url: None,
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
            ..info.clone()
        };
        assert_eq!(v6.ws_url(), "ws://[fe80::1]:42117/together");
        assert_eq!(
            JoinInfo::parse("ws://[fe80::1]:42117/together?sid=abc-123&key=k").map(|i| i.host),
            Some("fe80::1".into())
        );
    }

    #[test]
    fn relay_links_round_trip() {
        let info =
            JoinInfo::via_relay("https://relay.example.com/music", "s1".into(), "k/1".into())
                .unwrap();
        assert_eq!(info.url.as_deref(), Some("wss://relay.example.com/music"));
        assert_eq!((info.host.as_str(), info.port), ("relay.example.com", 443));
        assert_eq!(info.ws_url(), "wss://relay.example.com/music");
        let link = info.to_link();
        assert_eq!(
            link,
            "music-together://join?url=wss%3A%2F%2Frelay.example.com%2Fmusic&sid=s1&key=k%2F1"
        );
        assert_eq!(JoinInfo::parse(&link), Some(info.clone()));
        assert_eq!(JoinInfo::parse(&info.to_compact()), Some(info.clone()));
        assert_eq!(
            JoinInfo::parse("wss://relay.example.com/music?sid=s1&key=k%2F1"),
            Some(info)
        );
        // A plain-ws relay with a port.
        let local = JoinInfo::parse("ws://127.0.0.1:42118/?sid=a&key=b").unwrap();
        assert_eq!(local.url.as_deref(), Some("ws://127.0.0.1:42118/"));
        assert_eq!(local.port, 42118);
    }

    #[test]
    fn relay_urls_from_settings() {
        for (raw, want) in [
            ("wss://relay.example.com", Some("wss://relay.example.com/")),
            (
                "https://relay.example.com/x?y=1#z",
                Some("wss://relay.example.com/x"),
            ),
            ("http://10.0.0.2:42118", Some("ws://10.0.0.2:42118/")),
            ("relay.example.com", Some("wss://relay.example.com/")),
            ("192.168.1.5:42118", Some("ws://192.168.1.5:42118/")),
            ("localhost:42118", Some("ws://localhost:42118/")),
            ("ftp://x", None),
            ("wss://user:pw@relay.example.com", None),
            ("", None),
        ] {
            assert_eq!(relay_url(raw).as_deref(), want, "{raw}");
        }
    }

    #[test]
    fn join_requests_with_codes() {
        let full =
            JoinInfo::via_relay("wss://relay.example.com", "s1".into(), "k1".into()).unwrap();
        // A full link that also carries the code joins directly.
        let link = full.to_link_with_code(Some("ABCDEFGH"));
        assert!(link.ends_with("&code=ABCDEFGH"), "{link}");
        assert_eq!(JoinInfo::parse(&link), Some(full.clone()));
        assert_eq!(
            JoinRequest::parse(&link),
            Some(JoinRequest::Direct(full.clone()))
        );
        assert_eq!(full.to_link_with_code(None), full.to_link());
        // A bare code, however typed.
        for raw in ["ABCDEFGH", "abcd-efgh", " Abcd Efgh "] {
            assert_eq!(
                JoinRequest::parse(raw),
                Some(JoinRequest::Code {
                    relay: None,
                    code: "ABCDEFGH".into()
                }),
                "{raw:?}"
            );
        }
        // A code link, with and without the relay.
        let only = JoinRequest::code_link("https://relay.example.com", "ABCDEFGH").unwrap();
        assert_eq!(
            only,
            "music-together://join?url=wss%3A%2F%2Frelay.example.com%2F&code=ABCDEFGH"
        );
        assert_eq!(
            JoinRequest::parse(&only),
            Some(JoinRequest::Code {
                relay: Some("wss://relay.example.com/".into()),
                code: "ABCDEFGH".into()
            })
        );
        assert_eq!(
            JoinRequest::parse("music-together://join?code=abcd-efgh"),
            Some(JoinRequest::Code {
                relay: None,
                code: "ABCDEFGH".into()
            })
        );
        for bad in [
            "music-together://join?code=ABCD-EFG0",
            "music-together://join?url=ftp%3A%2F%2Fx&code=ABCDEFGH",
            "https://example.com/?code=ABCDEFGH",
            "ABCDEFG",
            "",
        ] {
            assert_eq!(JoinRequest::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn rejects_incomplete_links() {
        for bad in [
            "",
            "music-together://join?host=h&port=1&sid=s",
            "music-together://join?host=h&port=0&sid=s&key=k",
            "https://example.com/?sid=s",
            "ftp://example.com/?sid=s&key=k",
            "h|1|s",
            "h|x|s|k",
        ] {
            assert_eq!(JoinInfo::parse(bad), None, "{bad}");
        }
    }
}
