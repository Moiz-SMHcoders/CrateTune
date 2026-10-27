//! Process-wide HTTP client.
//!
//! Every network call in music-core goes through one shared `ureq` agent. [`configure`]
//! swaps it for one built from an [`HttpConfig`] (proxy, user agent, timeout); requests
//! already in flight keep the agent they started with.
//!
//! DNS-over-HTTPS is not supported: `ureq` would need a custom resolver plus an HTTPS
//! client that bootstraps without DNS. Use a `socks5h://` proxy (names are resolved by
//! the proxy) or a local DoH-capable resolver instead.

use std::sync::{OnceLock, PoisonError, RwLock};
use std::time::Duration;

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
}

struct Shared {
    config: HttpConfig,
    agent: ureq::Agent,
}

fn shared() -> &'static RwLock<Shared> {
    static SHARED: OnceLock<RwLock<Shared>> = OnceLock::new();
    SHARED.get_or_init(|| {
        // Until `configure` is called: ureq defaults (incl. proxy env vars) + 30 s timeout.
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(DEFAULT_TIMEOUT))
            .build()
            .into();
        RwLock::new(Shared {
            config: HttpConfig::default(),
            agent,
        })
    })
}

fn build_agent(config: &HttpConfig) -> Result<ureq::Agent> {
    let proxy = match config.proxy.as_deref().map(str::trim) {
        Some(url) if !url.is_empty() => Some(
            ureq::Proxy::new(url)
                .map_err(|e| Error::Parse(format!("invalid proxy {url:?}: {e}")))?,
        ),
        _ => None,
    };
    let mut builder = ureq::Agent::config_builder()
        .timeout_global(Some(config.timeout.unwrap_or(DEFAULT_TIMEOUT)))
        .proxy(proxy);
    if let Some(ua) = &config.user_agent {
        builder = builder.user_agent(ua.as_str());
    }
    Ok(builder.build().into())
}

/// Rebuilds the shared HTTP agent from `config`; call again whenever settings change.
/// On an unparsable proxy URL this fails and the previous agent stays in use.
pub fn configure(config: HttpConfig) -> Result<()> {
    let agent = build_agent(&config)?;
    *shared().write().unwrap_or_else(PoisonError::into_inner) = Shared { config, agent };
    Ok(())
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
            };
            assert!(build_agent(&config).is_ok(), "{proxy}");
        }
        let bad = HttpConfig {
            proxy: Some("gopher://nope:1".into()),
            ..Default::default()
        };
        assert!(build_agent(&bad).is_err());
        assert!(build_agent(&HttpConfig::default()).is_ok());
    }
}
