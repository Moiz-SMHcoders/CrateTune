//! Room codes and the default relay (the "online" mode, after ArchiveTune's
//! `TogetherOnlineApi` / `TogetherOnlineEndpoint`, on our own relay).
//!
//! * A relayed host trades its live room for a short code
//!   ([`super::TogetherServer::register_code`], `POST /v1/together/sessions`); a
//!   guest trades the code for the session id, key and WebSocket URL
//!   ([`resolve_code`], `POST /v1/together/sessions/resolve`). Codes are
//!   [`CODE_LEN`] characters from [`CODE_ALPHABET`]; [`normalize_code`] takes them
//!   in any case, with spaces and dashes.
//! * Which relay: the user's setting, else the address published in the app's
//!   repository (`packaging/relay.txt`, fetched and cached for [`DIRECTORY_TTL`]
//!   by [`directory_relay`]), else the built-in default ([`effective_relay`]).
//!   [`probe`] checks it answers before a session is hosted there.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::link::{JoinInfo, relay_url};

/// 32 symbols without the look-alikes 0 / O / 1 / I (same as the relay's).
pub const CODE_ALPHABET: &str = "23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
pub const CODE_LEN: usize = 8;
/// How long a fetched `relay.txt` is trusted before it is fetched again.
pub const DIRECTORY_TTL: Duration = Duration::from_secs(6 * 60 * 60);
/// After a failed fetch, the cached (or built-in) relay is used this long before
/// trying again (so an unreachable GitHub doesn't slow every session down).
pub const DIRECTORY_RETRY: Duration = Duration::from_secs(10 * 60);
const MAX_RESPONSE: u64 = 64 * 1024;

/// `abcd-efgh`, ` ABCD EFGH ` → `ABCDEFGH`; `None` if it can't be a room code.
pub fn normalize_code(raw: &str) -> Option<String> {
    let code: String = raw
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '-' | '‐' | '‑' | '‒' | '–' | '—' | '−'))
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (code.len() == CODE_LEN && code.chars().all(|c| CODE_ALPHABET.contains(c))).then_some(code)
}

/// `ABCDEFGH` → `ABCD-EFGH` (for reading out and typing).
pub fn display_code(code: &str) -> String {
    let half = code.len() / 2;
    match (code.get(..half), code.get(half..)) {
        (Some(a), Some(b)) if code.len() >= 6 => format!("{a}-{b}"),
        _ => code.to_owned(),
    }
}

/// Why a code couldn't be had or used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeError {
    /// Not a room code at all (checked before asking the relay).
    BadCode,
    /// The relay knows no live session with that code.
    NotFound,
    /// Too many look-ups from this address; try again after this many seconds.
    RateLimited(Option<u64>),
    /// The relay is older or isn't a music-relay: no room codes there.
    Unsupported,
    /// The relay refused (e.g. the room isn't hosted there).
    Refused(String),
    /// The relay couldn't be reached (or answered nonsense).
    Network(String),
}

impl std::fmt::Display for CodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadCode => write!(f, "not a room code"),
            Self::NotFound => write!(f, "no session with that code"),
            Self::RateLimited(Some(s)) => write!(f, "too many attempts, try again in {s} s"),
            Self::RateLimited(None) => write!(f, "too many attempts, try again later"),
            Self::Unsupported => write!(f, "the relay has no room codes"),
            Self::Refused(m) => write!(f, "refused: {m}"),
            Self::Network(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for CodeError {}

/// The relay's HTTP base for its WebSocket URL: `wss://h/p/` → `https://h/p`.
pub fn http_base(relay_ws_url: &str) -> Option<String> {
    let url = relay_url(relay_ws_url)?;
    let (scheme, rest) = url.split_once("://")?;
    let scheme = if scheme == "wss" { "https" } else { "http" };
    Some(format!("{scheme}://{}", rest.trim_end_matches('/')))
}

/// Requests to the relay go straight to it, like the session's WebSocket (no proxy).
fn relay_agent(timeout: Duration) -> ureq::Agent {
    crate::http::agent_with(
        ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .proxy(None)
            .build(),
    )
}

struct Reply {
    status: u16,
    retry_after: Option<u64>,
    body: Value,
}

fn post(
    relay_ws_url: &str,
    path: &str,
    body: &Value,
    timeout: Duration,
) -> Result<Reply, CodeError> {
    let base = http_base(relay_ws_url)
        .ok_or_else(|| CodeError::Network(format!("not a relay address: {relay_ws_url}")))?;
    let mut resp = relay_agent(timeout)
        .post(&format!("{base}{path}"))
        .send_json(body)
        .map_err(|e| CodeError::Network(e.to_string()))?;
    let status = resp.status().as_u16();
    let retry_after = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok());
    let body = resp
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE)
        .read_to_vec()
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    Ok(Reply {
        status,
        retry_after,
        body,
    })
}

fn string(body: &Value, name: &str) -> Option<String> {
    body.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// An error reply as a [`CodeError`] (a reply that isn't the relay's JSON means
/// the endpoint isn't there).
fn reply_error(reply: &Reply) -> CodeError {
    let code = string(&reply.body, "code");
    let message = string(&reply.body, "error");
    match (reply.status, code.as_deref()) {
        (429, _) => CodeError::RateLimited(reply.retry_after),
        (_, Some("not_found")) => CodeError::NotFound,
        (_, Some("bad_code")) => CodeError::BadCode,
        _ if message.is_none() => CodeError::Unsupported,
        _ => CodeError::Refused(message.unwrap_or_default()),
    }
}

/// Host side (see [`super::TogetherServer::register_code`]): the room's code.
pub(super) fn register_code(
    relay_ws_url: &str,
    session_id: &str,
    guest_key: &str,
    host_key: &str,
    timeout: Duration,
) -> Result<String, CodeError> {
    let body = json!({ "sessionId": session_id, "guestKey": guest_key, "hostKey": host_key });
    let reply = post(relay_ws_url, "/v1/together/sessions", &body, timeout)?;
    if reply.status != 200 {
        return Err(reply_error(&reply));
    }
    string(&reply.body, "code")
        .and_then(|c| normalize_code(&c))
        .ok_or_else(|| CodeError::Network("the relay sent no code".into()))
}

/// Guest side: trades a code for the session at the relay `relay_ws_url`.
pub fn resolve_code(
    relay_ws_url: &str,
    code: &str,
    timeout: Duration,
) -> Result<JoinInfo, CodeError> {
    let code = normalize_code(code).ok_or(CodeError::BadCode)?;
    let reply = post(
        relay_ws_url,
        "/v1/together/sessions/resolve",
        &json!({ "code": code }),
        timeout,
    )?;
    if reply.status != 200 {
        return Err(reply_error(&reply));
    }
    let (Some(session_id), Some(key)) = (
        string(&reply.body, "sessionId"),
        string(&reply.body, "guestKey"),
    ) else {
        return Err(CodeError::Network(
            "the relay's answer is incomplete".into(),
        ));
    };
    let ws = string(&reply.body, "wsUrl").unwrap_or_default();
    let url = session_ws_url(relay_ws_url, &ws)
        .ok_or_else(|| CodeError::Network(format!("not a relay address: {relay_ws_url}")))?;
    JoinInfo::via_relay(&url, session_id, key)
        .ok_or_else(|| CodeError::Network(format!("not a relay address: {url}")))
}

/// Where guests connect, from the relay's `wsUrl` answer: a path is taken
/// relative to the relay's address (`/` = the address itself); an absolute URL is
/// used unless it points at a loopback / unspecified host (a relay that doesn't
/// know its public name), as ArchiveTune does.
pub fn session_ws_url(relay_ws_url: &str, ws_url: &str) -> Option<String> {
    let base = relay_url(relay_ws_url)?;
    let ws_url = ws_url.trim();
    if ws_url.is_empty() || ws_url == "/" {
        return Some(base);
    }
    if ws_url.starts_with('/') {
        return relay_url(&format!("{}{ws_url}", base.trim_end_matches('/')));
    }
    let absolute = relay_url(ws_url).filter(|u| u.contains("://"))?;
    let host = url::Url::parse(&absolute)
        .ok()?
        .host_str()?
        .to_ascii_lowercase();
    let local = matches!(
        host.as_str(),
        "localhost" | "127.0.0.1" | "0.0.0.0" | "[::1]" | "::1" | "[::]"
    );
    Some(if local { base } else { absolute })
}

/// Whether a music-relay answers at `relay_ws_url` (`GET /health`), within `timeout`.
pub fn probe(relay_ws_url: &str, timeout: Duration) -> crate::Result<()> {
    let base = http_base(relay_ws_url)
        .ok_or_else(|| crate::Error::Parse(format!("not a relay address: {relay_ws_url}")))?;
    let mut resp = relay_agent(timeout).get(&format!("{base}/health")).call()?;
    let status = resp.status().as_u16();
    let body: Value = resp
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE)
        .read_to_vec()
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    if status == 200 && body.get("status").and_then(Value::as_str) == Some("ok") {
        Ok(())
    } else {
        Err(crate::Error::Parse(format!(
            "{base} isn't a music-relay (HTTP {status})"
        )))
    }
}

// ---- which relay ----------------------------------------------------------------------

/// The relay address in `relay.txt`: the first line that isn't blank or a `#`
/// comment, with an explicit `ws` / `wss` / `http` / `https` scheme and a host
/// (no credentials). Returned as its WebSocket URL.
pub fn parse_relay_txt(text: &str) -> Option<String> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))?;
    let (scheme, _) = line.split_once("://")?;
    if !matches!(
        scheme.to_ascii_lowercase().as_str(),
        "ws" | "wss" | "http" | "https"
    ) {
        return None;
    }
    let url = relay_url(line)?;
    let host = url::Url::parse(&url).ok()?.host_str()?.to_owned();
    (!host.is_empty()).then_some(url)
}

/// What `relay.txt` said last, and when it was checked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct DirectoryCache {
    url: String,
    /// Unix seconds of the last successful fetch.
    fetched_at: u64,
    /// Unix seconds of the last failed fetch (0: none since).
    failed_at: u64,
}

fn unix(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn read_cache(path: Option<&Path>) -> DirectoryCache {
    path.and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_cache(path: Option<&Path>, cache: &DirectoryCache) {
    let Some(path) = path else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_vec_pretty(cache) {
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

/// The cached `relay.txt` address, without fetching (stale or not).
pub fn cached_directory_relay(cache: Option<&Path>) -> Option<String> {
    parse_relay_txt(&read_cache(cache).url)
}

/// The relay published in the repository: the cached one while it is younger
/// than [`DIRECTORY_TTL`], else `fetch()` (the text of `relay.txt`, `None` on
/// failure) — a valid answer is cached at `cache`; on failure the last good one
/// is kept and used (and no new fetch for [`DIRECTORY_RETRY`]). `None`: nothing
/// valid ever came.
pub fn directory_relay(
    cache: Option<&Path>,
    now: SystemTime,
    fetch: impl FnOnce() -> Option<String>,
) -> Option<String> {
    let mut stored = read_cache(cache);
    let cached = parse_relay_txt(&stored.url);
    let now_s = unix(now);
    let age = |at: u64| Duration::from_secs(now_s.saturating_sub(at));
    if cached.is_some() && stored.fetched_at <= now_s && age(stored.fetched_at) < DIRECTORY_TTL {
        return cached;
    }
    if stored.failed_at != 0 && stored.failed_at <= now_s && age(stored.failed_at) < DIRECTORY_RETRY
    {
        return cached;
    }
    match fetch().as_deref().and_then(parse_relay_txt) {
        Some(url) => {
            write_cache(
                cache,
                &DirectoryCache {
                    url: url.clone(),
                    fetched_at: now_s,
                    failed_at: 0,
                },
            );
            Some(url)
        }
        None => {
            stored.failed_at = now_s;
            write_cache(cache, &stored);
            cached
        }
    }
}

/// Where the relay address came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelaySource {
    /// The user's setting.
    Settings,
    /// `relay.txt` in the app's repository (fetched or cached).
    Directory,
    /// The default built into the app.
    BuiltIn,
}

/// The relay to use: the user's (valid) setting, else `directory()` (see
/// [`directory_relay`]; only asked when there's no setting), else `built_in`.
pub fn effective_relay(
    user: &str,
    directory: impl FnOnce() -> Option<String>,
    built_in: &str,
) -> Option<(String, RelaySource)> {
    if let Some(url) = relay_url(user) {
        return Some((url, RelaySource::Settings));
    }
    if let Some(url) = directory().and_then(|u| relay_url(&u)) {
        return Some((url, RelaySource::Directory));
    }
    relay_url(built_in).map(|url| (url, RelaySource::BuiltIn))
}

/// Downloads `relay.txt` (small text, through the app's HTTP settings). `None` on
/// any failure.
pub fn fetch_text(url: &str, timeout: Duration) -> Option<String> {
    let mut resp = crate::http::agent()
        .get(url)
        .config()
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .build()
        .call()
        .ok()?;
    if resp.status().as_u16() != 200 {
        return None;
    }
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(16 * 1024)
        .read_to_vec()
        .ok()?;
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn codes_normalize_and_display() {
        for (raw, want) in [
            ("ABCD-EFGH", Some("ABCDEFGH")),
            ("abcd efgh", Some("ABCDEFGH")),
            (" a-b-c-d e—f g h ", Some("ABCDEFGH")),
            ("2345 6789", Some("23456789")),
            ("ABCD-EFG0", None),
            ("ABCD-EFGO", None),
            ("ABCD-EFG1", None),
            ("ABCD-EFGI", None),
            ("ABCDEFG", None),
            ("ABCDEFGHJ", None),
            ("", None),
        ] {
            assert_eq!(normalize_code(raw).as_deref(), want, "{raw:?}");
        }
        assert_eq!(display_code("ABCDEFGH"), "ABCD-EFGH");
        assert_eq!(
            normalize_code(&display_code("ABCDEFGH")).as_deref(),
            Some("ABCDEFGH")
        );
        assert_eq!(CODE_ALPHABET.len(), 32);
    }

    #[test]
    fn relay_bases_and_session_urls() {
        assert_eq!(
            http_base("wss://relay.example.com").as_deref(),
            Some("https://relay.example.com")
        );
        assert_eq!(
            http_base("ws://10.0.0.2:42118/").as_deref(),
            Some("http://10.0.0.2:42118")
        );
        assert_eq!(
            http_base("wss://example.com/relay/").as_deref(),
            Some("https://example.com/relay")
        );
        let base = "wss://relay.example.com";
        assert_eq!(
            session_ws_url(base, "/").as_deref(),
            Some("wss://relay.example.com/")
        );
        assert_eq!(
            session_ws_url(base, "").as_deref(),
            Some("wss://relay.example.com/")
        );
        assert_eq!(
            session_ws_url("wss://example.com/relay", "/ws").as_deref(),
            Some("wss://example.com/relay/ws")
        );
        assert_eq!(
            session_ws_url(base, "wss://other.example.com/x").as_deref(),
            Some("wss://other.example.com/x")
        );
        // A relay that only knows itself as localhost: its public address instead.
        assert_eq!(
            session_ws_url(base, "ws://127.0.0.1:42118/").as_deref(),
            Some("wss://relay.example.com/")
        );
        assert_eq!(session_ws_url("ftp://x", "/"), None);
    }

    #[test]
    fn relay_txt_parsing() {
        for (text, want) in [
            (
                "wss://relay.smhcoders.click\n",
                Some("wss://relay.smhcoders.click/"),
            ),
            (
                "# the relay\n\n  https://relay.example.com/music  \nwss://ignored",
                Some("wss://relay.example.com/music"),
            ),
            ("ws://192.168.0.51:42118", Some("ws://192.168.0.51:42118/")),
            ("http://10.0.0.2:42118\r\n", Some("ws://10.0.0.2:42118/")),
            // A scheme is required (no guessing from a bare name), and a host.
            ("relay.example.com", None),
            ("ftp://relay.example.com", None),
            ("wss://", None),
            ("wss://user:pw@relay.example.com", None),
            ("<html>404</html>", None),
            ("", None),
            ("# only a comment", None),
        ] {
            assert_eq!(parse_relay_txt(text).as_deref(), want, "{text:?}");
        }
    }

    fn temp_cache(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "together-online-{name}-{}-{}",
            std::process::id(),
            crate::together::new_id()
        ));
        dir.join("relay.json")
    }

    #[test]
    fn directory_caches_for_six_hours() {
        let path = temp_cache("ttl");
        let t0 = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let calls = Cell::new(0);
        let fetch = |text: &'static str| {
            let calls = &calls;
            move || {
                calls.set(calls.get() + 1);
                Some(text.to_owned())
            }
        };
        let first = directory_relay(Some(&path), t0, fetch("wss://a.example.com"));
        assert_eq!(first.as_deref(), Some("wss://a.example.com/"));
        assert_eq!(calls.get(), 1);
        // Within 6 h: the cache, no fetch.
        let soon = t0 + Duration::from_secs(5 * 3600);
        let cached = directory_relay(Some(&path), soon, fetch("wss://b.example.com"));
        assert_eq!(cached.as_deref(), Some("wss://a.example.com/"));
        assert_eq!(calls.get(), 1);
        assert_eq!(cached_directory_relay(Some(&path)), cached);
        // After 6 h: fetched again, the new address wins and is cached.
        let later = t0 + Duration::from_secs(7 * 3600);
        let moved = directory_relay(Some(&path), later, fetch("wss://b.example.com"));
        assert_eq!(moved.as_deref(), Some("wss://b.example.com/"));
        assert_eq!(calls.get(), 2);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn directory_falls_back_to_the_last_good_address() {
        let path = temp_cache("fallback");
        let t0 = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        assert_eq!(
            directory_relay(Some(&path), t0, || Some("wss://good.example.com".into())).as_deref(),
            Some("wss://good.example.com/")
        );
        // Stale, and the fetch fails (offline): the stale address.
        let stale = t0 + Duration::from_secs(8 * 3600);
        assert_eq!(
            directory_relay(Some(&path), stale, || None).as_deref(),
            Some("wss://good.example.com/")
        );
        // Stale, and the file now holds garbage: still the last good one.
        let retry = stale + DIRECTORY_RETRY + Duration::from_secs(1);
        assert_eq!(
            directory_relay(Some(&path), retry, || Some("not a url".into())).as_deref(),
            Some("wss://good.example.com/")
        );
        // Right after a failure: no new fetch for a while.
        let tried = Cell::new(false);
        let again = retry + Duration::from_secs(60);
        assert_eq!(
            directory_relay(Some(&path), again, || {
                tried.set(true);
                Some("wss://new.example.com".into())
            })
            .as_deref(),
            Some("wss://good.example.com/")
        );
        assert!(!tried.get());
        // A broken cache file counts as no cache.
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(cached_directory_relay(Some(&path)), None);
        assert_eq!(directory_relay(Some(&path), again, || None), None);
        // No cache location at all: just the fetch.
        assert_eq!(
            directory_relay(None, t0, || Some("ws://10.0.0.1:42118".into())).as_deref(),
            Some("ws://10.0.0.1:42118/")
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn effective_relay_order() {
        let builtin = "wss://relay.smhcoders.click";
        // The user's setting wins, and the directory isn't even asked.
        let asked = Cell::new(false);
        let r = effective_relay(
            "ws://10.0.0.2:42118",
            || {
                asked.set(true);
                Some("wss://dir.example.com".into())
            },
            builtin,
        );
        assert_eq!(
            r,
            Some(("ws://10.0.0.2:42118/".into(), RelaySource::Settings))
        );
        assert!(!asked.get());
        // Then the directory, then the built-in default.
        assert_eq!(
            effective_relay("", || Some("wss://dir.example.com".into()), builtin),
            Some(("wss://dir.example.com/".into(), RelaySource::Directory))
        );
        assert_eq!(
            effective_relay("  ", || None, builtin),
            Some(("wss://relay.smhcoders.click/".into(), RelaySource::BuiltIn))
        );
        // An invalid setting doesn't count.
        assert_eq!(
            effective_relay("ftp://nope", || None, builtin).map(|r| r.1),
            Some(RelaySource::BuiltIn)
        );
        assert_eq!(effective_relay("", || None, ""), None);
    }

    #[test]
    fn unreachable_relay_fails_fast() {
        // Nothing listens on this port (bound, then closed).
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let url = format!("ws://127.0.0.1:{port}");
        let start = std::time::Instant::now();
        assert!(probe(&url, Duration::from_secs(3)).is_err());
        assert!(matches!(
            resolve_code(&url, "ABCD-EFGH", Duration::from_secs(3)),
            Err(CodeError::Network(_))
        ));
        assert_eq!(
            resolve_code(&url, "nope", Duration::from_secs(3)),
            Err(CodeError::BadCode)
        );
        assert!(start.elapsed() < Duration::from_secs(8));
    }
}
