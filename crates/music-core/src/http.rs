//! Process-wide HTTP client.
//!
//! Every network call in music-core goes through one shared `ureq` agent. [`configure`]
//! swaps it for one built from an [`HttpConfig`] (proxy, user agent, timeout,
//! DNS-over-HTTPS); requests already in flight keep the agent they started with.
//!
//! With [`HttpConfig::doh`] set, host names are resolved with DNS-over-HTTPS
//! ([`crate::doh`]: Cloudflare, Google, Quad9 or a custom RFC 8484 URL, cached per TTL,
//! optional fallback to the system resolver). With a `socks5h://` proxy the proxy
//! resolves the target names itself; DoH then only resolves the proxy's host.

use std::sync::{OnceLock, PoisonError, RwLock};
use std::time::Duration;

use crate::doh::DohResolver;
pub use crate::doh::{DohConfig, DohProvider, Lookup, LookupSource};
use crate::{Error, Result};

/// Largest single response we buffer (audio files are fetched in ranges below this).
const MAX_BODY_BYTES: u64 = 64 * 1024 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Settings for the shared HTTP agent, applied with [`configure`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HttpConfig {
    /// Proxy URL: `http://[user:pass@]host:port`, `https://…`, `socks4://…`, `socks4a://…`,
    /// `socks5://…` or `socks5h://…` (DNS resolved by the proxy). `None` = direct
    /// connections (proxy environment variables are then ignored as well).
    pub proxy: Option<String>,
    /// Replaces the browser user agent InnerTube sends and ureq's default user agent
    /// everywhere else. Requests that must match a specific client (the iOS stream
    /// resolver) keep theirs.
    pub user_agent: Option<String>,
    /// Whole-request timeout; `None` = 30 s.
    pub timeout: Option<Duration>,
    /// Resolve host names with DNS-over-HTTPS; `None` = the system resolver.
    pub doh: Option<DohConfig>,
}

struct Shared {
    config: HttpConfig,
    agent: ureq::Agent,
    /// The DoH resolver inside `agent` (shared cache), for [`lookup`] and [`agent_with`].
    resolver: Option<DohResolver>,
}

fn shared() -> &'static RwLock<Shared> {
    static SHARED: OnceLock<RwLock<Shared>> = OnceLock::new();
    SHARED.get_or_init(|| {
        // Until `configure` is called: ureq defaults (incl. proxy env vars) + 30 s timeout.
        let agent = match build_agent(&HttpConfig::default()) {
            // Fixture mode: through the closed proxy.
            Ok((agent, _)) if crate::fixtures::enabled() => agent,
            _ => ureq::Agent::config_builder()
                .timeout_global(Some(DEFAULT_TIMEOUT))
                .build()
                .into(),
        };
        RwLock::new(Shared {
            config: HttpConfig::default(),
            agent,
            resolver: None,
        })
    })
}

fn proxy_of(config: &HttpConfig) -> Result<Option<ureq::Proxy>> {
    // Fixture mode (`MUSIC_FIXTURES`): nothing leaves the machine.
    let proxy = if crate::fixtures::enabled() {
        Some(crate::fixtures::DEAD_PROXY)
    } else {
        config.proxy.as_deref()
    };
    Ok(match proxy.map(str::trim) {
        Some(url) if !url.is_empty() => Some(
            ureq::Proxy::new(url)
                .map_err(|e| Error::Parse(format!("invalid proxy {url:?}: {e}")))?,
        ),
        _ => None,
    })
}

fn build_resolver(config: &HttpConfig) -> Result<Option<DohResolver>> {
    let Some(doh) = &config.doh else {
        return Ok(None);
    };
    DohResolver::new(doh, proxy_of(config)?)
        .map(Some)
        .map_err(|e| Error::Parse(format!("DNS-over-HTTPS: {e}")))
}

fn wrap(config: ureq::config::Config, resolver: Option<&DohResolver>) -> ureq::Agent {
    match resolver {
        Some(r) => ureq::Agent::with_parts(
            config,
            ureq::unversioned::transport::DefaultConnector::default(),
            r.clone(),
        ),
        None => config.into(),
    }
}

fn build_agent(config: &HttpConfig) -> Result<(ureq::Agent, Option<DohResolver>)> {
    let mut builder = ureq::Agent::config_builder()
        .timeout_global(Some(config.timeout.unwrap_or(DEFAULT_TIMEOUT)))
        .proxy(proxy_of(config)?);
    if let Some(ua) = &config.user_agent {
        builder = builder.user_agent(ua.as_str());
    }
    let resolver = build_resolver(config)?;
    Ok((wrap(builder.build(), resolver.as_ref()), resolver))
}

/// Rebuilds the shared HTTP agent from `config`; call again whenever settings change.
/// On an unparsable proxy URL (or DoH URL) this fails and the previous agent stays in use.
pub fn configure(config: HttpConfig) -> Result<()> {
    let (agent, resolver) = build_agent(&config)?;
    *shared().write().unwrap_or_else(PoisonError::into_inner) = Shared {
        config,
        agent,
        resolver,
    };
    Ok(())
}

/// An agent with its own settings (`config`, e.g. other timeouts or no redirects) that
/// resolves names like the shared one (DoH when configured).
pub(crate) fn agent_with(config: ureq::config::Config) -> ureq::Agent {
    let resolver = shared()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .resolver
        .clone();
    wrap(config, resolver.as_ref())
}

/// Resolves `host` the way requests do right now (DoH with its cache, or the system
/// resolver) and says how. Blocking; for diagnostics ("Test DNS").
pub fn lookup(host: &str) -> std::result::Result<Lookup, String> {
    let resolver = shared()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .resolver
        .clone();
    match resolver {
        Some(r) => r.lookup(host, ureq::config::IpFamily::Any),
        None => {
            use std::net::ToSocketAddrs;
            let addrs: Vec<_> = (host, 443)
                .to_socket_addrs()
                .map_err(|e| format!("{host}: {e}"))?
                .map(|a| a.ip())
                .collect();
            Ok(Lookup {
                addrs,
                source: LookupSource::System,
            })
        }
    }
}

/// The configuration currently in effect.
pub fn http_config() -> HttpConfig {
    shared()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .config
        .clone()
}

/// The shared agent (a cheap, reference-counted handle).
pub(crate) fn agent() -> ureq::Agent {
    shared()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .agent
        .clone()
}

/// [`HttpConfig::user_agent`], if set.
pub(crate) fn user_agent_override() -> Option<String> {
    shared()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .config
        .user_agent
        .clone()
}

pub(crate) fn read_body(resp: &mut ureq::http::Response<ureq::Body>) -> Result<Vec<u8>> {
    Ok(resp
        .body_mut()
        .with_config()
        .limit(MAX_BODY_BYTES)
        .read_to_vec()?)
}

/// Downloads a small resource such as album art. Blocking.
pub fn fetch_bytes(url: &str) -> Result<Vec<u8>> {
    let mut resp = agent().get(url).call()?;
    read_body(&mut resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_agents_for_supported_proxies() {
        for proxy in [
            "socks5://127.0.0.1:1080",
            "socks5h://127.0.0.1:1080",
            "http://user:pw@proxy.local:3128",
        ] {
            let config = HttpConfig {
                proxy: Some(proxy.into()),
                user_agent: Some("test-agent".into()),
                timeout: Some(Duration::from_secs(5)),
                doh: Some(DohConfig::new(DohProvider::Quad9)),
            };
            assert!(build_agent(&config).is_ok(), "{proxy}");
        }
        let bad = HttpConfig {
            proxy: Some("gopher://nope:1".into()),
            ..Default::default()
        };
        assert!(build_agent(&bad).is_err());
        assert!(build_agent(&HttpConfig::default()).is_ok());
        let bad_doh = HttpConfig {
            doh: Some(DohConfig::new(DohProvider::Custom("dns.example".into()))),
            ..Default::default()
        };
        assert!(build_agent(&bad_doh).is_err());
    }
}
