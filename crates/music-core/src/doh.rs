//! DNS-over-HTTPS for the shared HTTP agent ([`crate::http`]).
//!
//! [`DohResolver`] plugs into ureq's resolver hook: host names are looked up with an
//! RFC 8484 query (`POST application/dns-message`, A and AAAA in parallel) to the
//! configured provider, cached for the answer's TTL (30 s – 1 h) and, when
//! [`DohConfig::fallback_to_system`] is set, looked up with the system resolver if the
//! DoH server fails or has no answer. IP literals and `localhost` never go to DoH.
//!
//! The well-known providers are reached through fixed bootstrap addresses (Cloudflare
//! `1.1.1.1`, Google `8.8.8.8`, Quad9 `9.9.9.9`; TLS still verifies the provider's host
//! name), so DoH itself needs no DNS. A custom URL's host is resolved by the system.
//! DoH requests use the same proxy as everything else. ureq speaks HTTP/1.1 only, so
//! servers that require HTTP/2 (Quad9 on port 443, answering 505) can't be used.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use ureq::config::{Config, IpFamily};
use ureq::http::Uri;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

/// Whole-lookup timeout of one DoH request.
const DOH_TIMEOUT: Duration = Duration::from_secs(5);
const MIN_TTL: u32 = 30;
const MAX_TTL: u32 = 3600;
const CACHE_ENTRIES: usize = 512;
const TYPE_A: u16 = 1;
const TYPE_AAAA: u16 = 28;

/// Where DNS-over-HTTPS queries go.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DohProvider {
    /// `https://cloudflare-dns.com/dns-query` (via 1.1.1.1 / 1.0.0.1).
    Cloudflare,
    /// `https://dns.google/dns-query` (via 8.8.8.8 / 8.8.4.4).
    Google,
    /// `https://dns.quad9.net:5053/dns-query` (via 9.9.9.9 / 149.112.112.112): Quad9's
    /// HTTP/1.1 endpoint — the one on port 443 only speaks HTTP/2 (505 otherwise), which
    /// ureq doesn't.
    Quad9,
    /// Any RFC 8484 endpoint (`https://…/dns-query`; its host is resolved by the system).
    /// `http://` works too, for local resolvers and tests.
    Custom(String),
}

impl DohProvider {
    /// The query URL.
    pub fn url(&self) -> &str {
        match self {
            DohProvider::Cloudflare => "https://cloudflare-dns.com/dns-query",
            DohProvider::Google => "https://dns.google/dns-query",
            DohProvider::Quad9 => "https://dns.quad9.net:5053/dns-query",
            DohProvider::Custom(url) => url.trim(),
        }
    }

    /// The provider's host name and the fixed addresses it is reached at.
    fn bootstrap(&self) -> Option<(&'static str, [IpAddr; 2])> {
        let v4 = |a, b, c, d| IpAddr::V4(Ipv4Addr::new(a, b, c, d));
        Some(match self {
            DohProvider::Cloudflare => ("cloudflare-dns.com", [v4(1, 1, 1, 1), v4(1, 0, 0, 1)]),
            DohProvider::Google => ("dns.google", [v4(8, 8, 8, 8), v4(8, 8, 4, 4)]),
            DohProvider::Quad9 => ("dns.quad9.net", [v4(9, 9, 9, 9), v4(149, 112, 112, 112)]),
            DohProvider::Custom(_) => return None,
        })
    }

    /// Checks a custom URL (`http(s)://host[:port]/path`).
    pub fn validate(&self) -> Result<(), String> {
        let url = url::Url::parse(self.url()).map_err(|e| format!("not a URL: {e}"))?;
        if !matches!(url.scheme(), "https" | "http") {
            return Err("the URL must start with https://".into());
        }
        if url.host_str().is_none_or(str::is_empty) {
            return Err("the URL has no host".into());
        }
        Ok(())
    }
}

/// DNS-over-HTTPS settings ([`crate::HttpConfig::doh`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DohConfig {
    pub provider: DohProvider,
    /// Use the system resolver when the DoH server can't be reached, fails or has no
    /// answer for a name (e.g. names only the local network knows).
    pub fallback_to_system: bool,
}

impl DohConfig {
    pub fn new(provider: DohProvider) -> Self {
        Self {
            provider,
            fallback_to_system: true,
        }
    }
}

/// How a name was resolved ([`crate::http::lookup`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lookup {
    pub addrs: Vec<IpAddr>,
    pub source: LookupSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupSource {
    /// Answered by the DoH server just now.
    Doh,
    /// From the DoH cache.
    Cached,
    /// The system resolver (DoH off, a local name, or the fallback after a DoH failure).
    System,
}

type SystemLookup = fn(&str, u16) -> io::Result<Vec<SocketAddr>>;

fn system_lookup(host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
    Ok((host, port).to_socket_addrs()?.collect())
}

struct CacheEntry {
    addrs: Vec<IpAddr>,
    expires: Instant,
}

struct Inner {
    url: String,
    fallback: bool,
    client: ureq::Agent,
    cache: Mutex<HashMap<String, CacheEntry>>,
    system: SystemLookup,
}

/// The ureq [`Resolver`] that sends lookups to a DoH server. Cheap to clone (shared
/// cache).
#[derive(Clone)]
pub struct DohResolver {
    inner: Arc<Inner>,
}

impl fmt::Debug for DohResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DohResolver")
            .field("url", &self.inner.url)
            .field("fallback", &self.inner.fallback)
            .finish_non_exhaustive()
    }
}

/// Resolves the DoH provider's own host name to its bootstrap addresses.
#[derive(Debug)]
struct BootstrapResolver {
    host: Option<(&'static str, [IpAddr; 2])>,
}

impl Resolver for BootstrapResolver {
    fn resolve(
        &self,
        uri: &Uri,
        config: &Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        if let (Some((name, ips)), Some(host)) = (self.host, uri.host())
            && host.eq_ignore_ascii_case(name)
        {
            let port = uri.port_u16().unwrap_or(443);
            let mut out = self.empty();
            for ip in ips {
                out.push(SocketAddr::new(ip, port));
            }
            return Ok(out);
        }
        DefaultResolver::default().resolve(uri, config, timeout)
    }
}

impl DohResolver {
    /// A resolver for `config`; `proxy` is used for the DoH requests themselves.
    pub fn new(config: &DohConfig, proxy: Option<ureq::Proxy>) -> Result<Self, String> {
        Self::with_system(config, proxy, system_lookup)
    }

    fn with_system(
        config: &DohConfig,
        proxy: Option<ureq::Proxy>,
        system: SystemLookup,
    ) -> Result<Self, String> {
        config.provider.validate()?;
        let agent_config = ureq::Agent::config_builder()
            .timeout_global(Some(DOH_TIMEOUT))
            .proxy(proxy)
            .build();
        let client = ureq::Agent::with_parts(
            agent_config,
            DefaultConnector::default(),
            BootstrapResolver {
                host: config.provider.bootstrap(),
            },
        );
        Ok(Self {
            inner: Arc::new(Inner {
                url: config.provider.url().to_owned(),
                fallback: config.fallback_to_system,
                client,
                cache: Mutex::new(HashMap::new()),
                system,
            }),
        })
    }

    /// Resolves `host` (DoH, cache, or the system resolver for local names / fallback).
    pub fn lookup(&self, host: &str, family: IpFamily) -> Result<Lookup, String> {
        let host = host.trim_start_matches('[').trim_end_matches(']');
        if let Ok(ip) = host.parse::<IpAddr>() {
            return Ok(Lookup {
                addrs: vec![ip],
                source: LookupSource::System,
            });
        }
        let name = host.trim_end_matches('.').to_ascii_lowercase();
        if is_local_name(&name) || !name.is_ascii() {
            return self.system(&name);
        }
        if let Some(addrs) = self.cached(&name) {
            return Ok(Lookup {
                addrs: filter_family(addrs, family),
                source: LookupSource::Cached,
            });
        }
        match self.query_both(&name, family) {
            Ok((addrs, ttl)) if !addrs.is_empty() => {
                self.remember(&name, addrs.clone(), ttl);
                Ok(Lookup {
                    addrs,
                    source: LookupSource::Doh,
                })
            }
            Ok(_) if self.inner.fallback => self.system(&name),
            Ok(_) => Err(format!("{name}: no address (DNS-over-HTTPS)")),
            Err(_) if self.inner.fallback => self.system(&name),
            Err(e) => Err(format!("{name}: DNS-over-HTTPS lookup failed: {e}")),
        }
    }

    fn system(&self, name: &str) -> Result<Lookup, String> {
        let addrs: Vec<IpAddr> = (self.inner.system)(name, 0)
            .map_err(|e| format!("{name}: {e}"))?
            .into_iter()
            .map(|a| a.ip())
            .collect();
        if addrs.is_empty() {
            return Err(format!("{name}: no address"));
        }
        Ok(Lookup {
            addrs,
            source: LookupSource::System,
        })
    }

    fn cached(&self, name: &str) -> Option<Vec<IpAddr>> {
        let cache = self
            .inner
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let entry = cache.get(name)?;
        (entry.expires > Instant::now()).then(|| entry.addrs.clone())
    }

    fn remember(&self, name: &str, addrs: Vec<IpAddr>, ttl: u32) {
        let mut cache = self
            .inner
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        if cache.len() >= CACHE_ENTRIES {
            cache.retain(|_, e| e.expires > now);
            if cache.len() >= CACHE_ENTRIES {
                cache.clear();
            }
        }
        cache.insert(
            name.to_owned(),
            CacheEntry {
                addrs,
                expires: now + Duration::from_secs(u64::from(ttl.clamp(MIN_TTL, MAX_TTL))),
            },
        );
    }

    /// Forgets cached answers.
    pub fn clear_cache(&self) {
        self.inner
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// A and AAAA (in parallel); IPv4 addresses first. Fails only if both fail.
    fn query_both(&self, name: &str, family: IpFamily) -> Result<(Vec<IpAddr>, u32), String> {
        let want_v4 = !matches!(family, IpFamily::Ipv6Only);
        let want_v6 = !matches!(family, IpFamily::Ipv4Only);
        let (v4, v6) = std::thread::scope(|s| {
            let v6 = want_v6.then(|| s.spawn(|| self.query(name, TYPE_AAAA)));
            let v4 = want_v4.then(|| self.query(name, TYPE_A));
            let v6 = v6.map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err("lookup thread panicked".into()))
            });
            (v4, v6)
        });
        let mut addrs = Vec::new();
        let mut ttl = u32::MAX;
        let mut error = None;
        for answer in [v4, v6].into_iter().flatten() {
            match answer {
                Ok(a) => {
                    if !a.addrs.is_empty() {
                        ttl = ttl.min(a.ttl);
                    }
                    addrs.extend(a.addrs);
                }
                Err(e) => error = Some(e),
            }
        }
        match error {
            Some(e) if addrs.is_empty() => Err(e),
            _ => Ok((addrs, if ttl == u32::MAX { MIN_TTL } else { ttl })),
        }
    }

    fn query(&self, name: &str, qtype: u16) -> Result<Answer, String> {
        let query = build_query(name, qtype)?;
        let mut resp = self
            .inner
            .client
            .post(&self.inner.url)
            .header("Content-Type", "application/dns-message")
            .header("Accept", "application/dns-message")
            .send(&query[..])
            .map_err(|e| e.to_string())?;
        let body = resp
            .body_mut()
            .with_config()
            .limit(64 * 1024)
            .read_to_vec()
            .map_err(|e| e.to_string())?;
        parse_response(&body, qtype)
    }
}

impl Resolver for DohResolver {
    fn resolve(
        &self,
        uri: &Uri,
        config: &Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let (Some(scheme), Some(authority)) = (uri.scheme(), uri.authority()) else {
            return DefaultResolver::default().resolve(uri, config, timeout);
        };
        let Some(port) = authority
            .port_u16()
            .or_else(|| default_port(scheme.as_str()))
        else {
            return DefaultResolver::default().resolve(uri, config, timeout);
        };
        let lookup = self
            .lookup(authority.host(), config.ip_family())
            .map_err(|e| ureq::Error::Io(io::Error::new(io::ErrorKind::NotFound, e)))?;
        let mut out = self.empty();
        let family = config.ip_family();
        let wanted =
            family.keep_wanted(lookup.addrs.into_iter().map(|ip| SocketAddr::new(ip, port)));
        for addr in wanted.take(16) {
            out.push(addr);
        }
        if out.is_empty() {
            return Err(ureq::Error::HostNotFound);
        }
        Ok(out)
    }
}

fn default_port(scheme: &str) -> Option<u16> {
    Some(match scheme {
        "http" => 80,
        "https" => 443,
        "socks4" | "socks4a" | "socks5" | "socks5h" => 1080,
        _ => return None,
    })
}

fn is_local_name(name: &str) -> bool {
    name == "localhost"
        || name.ends_with(".localhost")
        || name.ends_with(".local")
        || !name.contains('.')
}

fn filter_family(addrs: Vec<IpAddr>, family: IpFamily) -> Vec<IpAddr> {
    addrs
        .into_iter()
        .filter(|ip| match family {
            IpFamily::Ipv4Only => ip.is_ipv4(),
            IpFamily::Ipv6Only => ip.is_ipv6(),
            IpFamily::Any => true,
        })
        .collect()
}

// ---- DNS wire format (RFC 1035 / RFC 8484) -------------------------------------------

#[derive(Debug, PartialEq, Eq)]
struct Answer {
    addrs: Vec<IpAddr>,
    /// Smallest TTL of the address records.
    ttl: u32,
}

/// A recursive query for `name` (id 0 as RFC 8484 recommends, RD set).
fn build_query(name: &str, qtype: u16) -> Result<Vec<u8>, String> {
    let name = name.trim_end_matches('.');
    if name.is_empty() || name.len() > 253 {
        return Err(format!("invalid host name {name:?}"));
    }
    let mut out = vec![0, 0, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in name.split('.') {
        if label.is_empty() || label.len() > 63 || !label.is_ascii() {
            return Err(format!("invalid host name {name:?}"));
        }
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out.extend_from_slice(&qtype.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // class IN
    Ok(out)
}

fn read_u16(buf: &[u8], at: usize) -> Result<u16, String> {
    buf.get(at..at + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .ok_or_else(|| "truncated DNS response".to_owned())
}

/// Skips a (possibly compressed) name; returns the offset after it.
fn skip_name(buf: &[u8], mut at: usize) -> Result<usize, String> {
    loop {
        let len = *buf.get(at).ok_or("truncated DNS name")?;
        match len {
            0 => return Ok(at + 1),
            l if l & 0xC0 == 0xC0 => return Ok(at + 2),
            l => at += 1 + l as usize,
        }
    }
}

/// The A / AAAA records of a response (CNAMEs are followed by the server).
fn parse_response(buf: &[u8], qtype: u16) -> Result<Answer, String> {
    if buf.len() < 12 {
        return Err("truncated DNS response".into());
    }
    let flags = read_u16(buf, 2)?;
    if flags & 0x8000 == 0 {
        return Err("not a DNS response".into());
    }
    match flags & 0x000F {
        0 => {}
        3 => {
            return Ok(Answer {
                addrs: Vec::new(),
                ttl: MIN_TTL,
            });
        }
        2 => return Err("DNS server failure (SERVFAIL)".into()),
        5 => return Err("DNS query refused".into()),
        code => return Err(format!("DNS error code {code}")),
    }
    let questions = read_u16(buf, 4)?;
    let answers = read_u16(buf, 6)?;
    let mut at = 12;
    for _ in 0..questions {
        at = skip_name(buf, at)? + 4;
    }
    let mut out = Answer {
        addrs: Vec::new(),
        ttl: u32::MAX,
    };
    for _ in 0..answers {
        at = skip_name(buf, at)?;
        let rtype = read_u16(buf, at)?;
        let ttl = buf
            .get(at + 4..at + 8)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
            .ok_or("truncated DNS record")?;
        let len = read_u16(buf, at + 8)? as usize;
        let data = buf
            .get(at + 10..at + 10 + len)
            .ok_or("truncated DNS record")?;
        at += 10 + len;
        let ip = match (rtype, len) {
            (TYPE_A, 4) if qtype == TYPE_A => {
                IpAddr::V4(Ipv4Addr::new(data[0], data[1], data[2], data[3]))
            }
            (TYPE_AAAA, 16) if qtype == TYPE_AAAA => {
                let mut b = [0u8; 16];
                b.copy_from_slice(data);
                IpAddr::V6(Ipv6Addr::from(b))
            }
            _ => continue,
        };
        out.ttl = out.ttl.min(ttl);
        out.addrs.push(ip);
    }
    if out.ttl == u32::MAX {
        out.ttl = MIN_TTL;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A response to `query` with one A record per address (TTL 120).
    fn answer(query: &[u8], v4: &[Ipv4Addr], rcode: u8) -> Vec<u8> {
        let qtype = u16::from_be_bytes([query[query.len() - 4], query[query.len() - 3]]);
        let mut out = query.to_vec();
        out[2] = 0x81;
        out[3] = 0x80 | rcode;
        let records: Vec<_> = if qtype == TYPE_A { v4.to_vec() } else { vec![] };
        out[6..8].copy_from_slice(&(records.len() as u16).to_be_bytes());
        for ip in records {
            out.extend_from_slice(&[0xC0, 12]); // pointer to the question name
            out.extend_from_slice(&TYPE_A.to_be_bytes());
            out.extend_from_slice(&1u16.to_be_bytes());
            out.extend_from_slice(&120u32.to_be_bytes());
            out.extend_from_slice(&4u16.to_be_bytes());
            out.extend_from_slice(&ip.octets());
        }
        out
    }

    #[test]
    fn wire_format_round_trip() {
        let q = build_query("music.youtube.com.", TYPE_A).unwrap();
        assert_eq!(&q[..4], &[0, 0, 1, 0]);
        assert_eq!(&q[12..18], b"\x05music");
        assert_eq!(&q[q.len() - 4..], &[0, 1, 0, 1]);
        assert!(build_query("", TYPE_A).is_err());
        assert!(build_query(&"a".repeat(64), TYPE_A).is_err());
        assert!(build_query("a..b", TYPE_A).is_err());

        let ips = [Ipv4Addr::new(142, 250, 1, 1), Ipv4Addr::new(142, 250, 1, 2)];
        let a = parse_response(&answer(&q, &ips, 0), TYPE_A).unwrap();
        assert_eq!(a.addrs, ips.map(IpAddr::V4));
        assert_eq!(a.ttl, 120);
        // AAAA asked, only A in the answer section: no addresses.
        let q6 = build_query("music.youtube.com", TYPE_AAAA).unwrap();
        assert!(
            parse_response(&answer(&q6, &ips, 0), TYPE_AAAA)
                .unwrap()
                .addrs
                .is_empty()
        );
        // NXDOMAIN is an empty answer, SERVFAIL an error, garbage an error.
        assert!(
            parse_response(&answer(&q, &[], 3), TYPE_A)
                .unwrap()
                .addrs
                .is_empty()
        );
        assert!(parse_response(&answer(&q, &[], 2), TYPE_A).is_err());
        assert!(
            parse_response(&q, TYPE_A).is_err(),
            "a query is not a response"
        );
        assert!(parse_response(&[0x00, 0x00, 0x81], TYPE_A).is_err());
        let mut cut = answer(&q, &ips, 0);
        cut.truncate(cut.len() - 2);
        assert!(parse_response(&cut, TYPE_A).is_err());
    }

    #[test]
    fn custom_urls_are_validated() {
        assert!(
            DohProvider::Custom("https://doh.example/dns-query".into())
                .validate()
                .is_ok()
        );
        assert!(DohProvider::Custom("ftp://x/".into()).validate().is_err());
        assert!(DohProvider::Custom("nonsense".into()).validate().is_err());
        assert!(DohProvider::Cloudflare.validate().is_ok());
    }

    /// A minimal HTTP/1.1 DoH server: answers A queries with `ips` (or `status`).
    fn stub_server(ips: Vec<Ipv4Addr>, status: u16) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/dns-query", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let body = loop {
                    let n = stream.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break None;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
                    let len: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    if buf.len() >= end + 4 + len {
                        break Some(buf[end + 4..end + 4 + len].to_vec());
                    }
                };
                let Some(query) = body else { continue };
                counter.fetch_add(1, Ordering::SeqCst);
                let (code, payload) = if status == 200 {
                    (200, answer(&query, &ips, 0))
                } else {
                    (status, Vec::new())
                };
                let head = format!(
                    "HTTP/1.1 {code} X\r\nContent-Type: application/dns-message\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    payload.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&payload);
            }
        });
        (url, hits)
    }

    /// An HTTP server that answers every request with `hello`.
    fn hello_server() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
                );
            }
        });
        port
    }

    fn no_system(_: &str, _: u16) -> io::Result<Vec<SocketAddr>> {
        Err(io::Error::other("system DNS must not be used"))
    }

    fn loopback_system(_: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        Ok(vec![SocketAddr::from(([127, 0, 0, 1], port))])
    }

    #[test]
    fn requests_resolve_through_a_local_doh_server_and_are_cached() {
        let (url, hits) = stub_server(vec![Ipv4Addr::LOCALHOST], 200);
        let config = DohConfig {
            provider: DohProvider::Custom(url),
            fallback_to_system: false,
        };
        let resolver = DohResolver::with_system(&config, None, no_system).unwrap();
        let agent = ureq::Agent::with_parts(
            ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(5)))
                .build(),
            DefaultConnector::default(),
            resolver.clone(),
        );
        let port = hello_server();
        let url = format!("http://music.test.example:{port}/");
        let body = agent
            .get(&url)
            .call()
            .unwrap()
            .body_mut()
            .read_to_string()
            .unwrap();
        assert_eq!(body, "hello");
        let after_first = hits.load(Ordering::SeqCst);
        assert_eq!(after_first, 2, "A + AAAA");
        // Cached: no further DoH requests.
        let body = agent
            .get(&url)
            .call()
            .unwrap()
            .body_mut()
            .read_to_string()
            .unwrap();
        assert_eq!(body, "hello");
        assert_eq!(hits.load(Ordering::SeqCst), after_first);
        let lookup = resolver
            .lookup("music.test.example", IpFamily::Any)
            .unwrap();
        assert_eq!(lookup.source, LookupSource::Cached);
        assert_eq!(lookup.addrs, vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);
        resolver.clear_cache();
        assert_eq!(
            resolver
                .lookup("music.test.example", IpFamily::Ipv4Only)
                .unwrap()
                .source,
            LookupSource::Doh
        );
        // IP literals and local names never go to DoH (and here: not to the system either).
        assert_eq!(
            resolver.lookup("[::1]", IpFamily::Any).unwrap().addrs,
            vec![IpAddr::V6(Ipv6Addr::LOCALHOST)]
        );
        assert!(resolver.lookup("printer.local", IpFamily::Any).is_err());
    }

    #[test]
    fn falls_back_to_system_dns_only_when_allowed() {
        let (url, hits) = stub_server(Vec::new(), 500);
        let strict = DohConfig {
            provider: DohProvider::Custom(url.clone()),
            fallback_to_system: false,
        };
        let resolver = DohResolver::with_system(&strict, None, loopback_system).unwrap();
        let err = resolver
            .lookup("music.test.example", IpFamily::Any)
            .unwrap_err();
        assert!(err.contains("DNS-over-HTTPS"), "{err}");
        assert!(hits.load(Ordering::SeqCst) >= 1);

        let lenient = DohConfig {
            fallback_to_system: true,
            ..strict
        };
        let resolver = DohResolver::with_system(&lenient, None, loopback_system).unwrap();
        let lookup = resolver
            .lookup("music.test.example", IpFamily::Any)
            .unwrap();
        assert_eq!(lookup.source, LookupSource::System);
        // Failures aren't cached: DoH is asked again next time.
        let before = hits.load(Ordering::SeqCst);
        resolver
            .lookup("music.test.example", IpFamily::Any)
            .unwrap();
        assert!(hits.load(Ordering::SeqCst) > before);

        // An empty answer (NXDOMAIN / no records) falls back too.
        let (url, _) = stub_server(Vec::new(), 200);
        let resolver = DohResolver::with_system(
            &DohConfig::new(DohProvider::Custom(url)),
            None,
            loopback_system,
        )
        .unwrap();
        assert_eq!(
            resolver
                .lookup("intranet.example", IpFamily::Any)
                .unwrap()
                .source,
            LookupSource::System
        );
        // An unreachable server.
        let dead = DohConfig {
            provider: DohProvider::Custom("http://127.0.0.1:1/dns-query".into()),
            fallback_to_system: false,
        };
        let resolver = DohResolver::with_system(&dead, None, no_system).unwrap();
        assert!(
            resolver
                .lookup("music.test.example", IpFamily::Any)
                .is_err()
        );
    }

    /// `cargo test -p music-core doh -- --ignored --nocapture`: the real providers.
    /// Cloudflare and Google must answer; Quad9 is reported (its HTTP/1.1 port 5053 is
    /// blocked on some networks).
    #[test]
    #[ignore = "live network"]
    fn live_providers() {
        for provider in [
            DohProvider::Cloudflare,
            DohProvider::Google,
            DohProvider::Quad9,
        ] {
            let config = DohConfig {
                provider: provider.clone(),
                fallback_to_system: false,
            };
            let resolver = DohResolver::with_system(&config, None, no_system).unwrap();
            match resolver.lookup("music.youtube.com", IpFamily::Any) {
                Ok(lookup) => {
                    assert_eq!(lookup.source, LookupSource::Doh, "{provider:?}");
                    assert!(!lookup.addrs.is_empty(), "{provider:?}");
                    eprintln!("{provider:?}: {:?}", lookup.addrs);
                }
                Err(e) if provider == DohProvider::Quad9 => eprintln!("{provider:?}: {e}"),
                Err(e) => panic!("{provider:?}: {e}"),
            }
        }
    }
}
