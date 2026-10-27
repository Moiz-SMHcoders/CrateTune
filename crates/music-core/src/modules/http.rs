// HTTP policy ported from ArchiveTune
// modules/src/main/kotlin/moe/rukamori/archivetune/modules/ModuleHttpClient.kt and
// app/src/main/kotlin/moe/rukamori/archivetune/modules/ModuleNetworkPolicy.kt
// — © Rukamori and ArchiveTune contributors, GPL-3.0
//! HTTP for modules: the provider's `fetch`, package / catalog downloads.
//!
//! * Only absolute `http(s)` URLs without user info.
//! * The app's proxy and user agent ([`crate::http::configure`]).
//! * Connect / read / whole-call timeouts of 10 / 15 / 20 s, also bounded by
//!   the caller's deadline.
//! * Request and response bodies of at most 8 MiB (documents: 4 MiB).
//! * Redirects are followed here (at most 10); `Authorization`, `Cookie` and
//!   `Proxy-Authorization` are dropped when the origin changes, and a 303 (or
//!   a 301/302 after a POST) continues as GET without a body.
//! * No cookie jar; nothing is shared with the YouTube account.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{Map, Value};

use super::error::{Failure, ModuleError, ModuleResult, Stage};
use crate::http::HttpConfig;

/// Largest fetch request or response body.
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// Largest downloaded package / descriptor / catalog document.
pub const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;
const MAX_REDIRECTS: usize = 10;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(15);
const CALL_TIMEOUT: Duration = Duration::from_secs(20);

/// Validates and normalizes an absolute HTTP(S) URL (`webUrl`).
pub fn web_url(value: &str) -> ModuleResult<String> {
    let url = url::Url::parse(value.trim()).map_err(|_| ModuleError::invalid())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none_or(str::is_empty)
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(ModuleError::invalid());
    }
    Ok(url.to_string())
}

/// `value` resolved against `base` (`resolveUrl`); absolute values stay as they are.
pub fn resolve_url(value: &str, base: &str) -> ModuleResult<String> {
    match url::Url::parse(value) {
        Ok(url) => Ok(url.to_string()),
        Err(url::ParseError::RelativeUrlWithoutBase) => url::Url::parse(base)
            .and_then(|b| b.join(value))
            .map(|u| u.to_string())
            .map_err(|_| ModuleError::invalid()),
        Err(_) => Err(ModuleError::invalid()),
    }
}

/// The parts the JS `URL` polyfill reads (`__moduleUrl`).
pub(crate) fn url_parts(input: &str, base: Option<&str>) -> ModuleResult<Value> {
    let resolved = match base {
        Some(base) if !base.is_empty() => resolve_url(input, base)?,
        _ => input.to_owned(),
    };
    let url = url::Url::parse(&resolved).map_err(|_| ModuleError::invalid())?;
    let host = match (url.host_str(), url.port()) {
        (Some(h), Some(p)) => format!("{h}:{p}"),
        (Some(h), None) => h.to_owned(),
        _ => String::new(),
    };
    let path = url.path();
    Ok(serde_json::json!({
        "href": url.as_str(),
        "protocol": format!("{}:", url.scheme()),
        "hostname": url.host_str().unwrap_or_default(),
        "host": host,
        "pathname": if path.is_empty() { "/" } else { path },
        "search": url.query().map(|q| format!("?{q}")).unwrap_or_default(),
        "hash": url.fragment().map(|f| format!("#{f}")).unwrap_or_default(),
        "origin": url.origin().ascii_serialization(),
    }))
}

/// An outgoing request (what the JS `fetch` hands to the host).
#[derive(Debug, Clone, Default)]
pub(crate) struct Request {
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl Request {
    pub(crate) fn get(url: &str) -> Self {
        Self {
            url: url.to_owned(),
            method: "GET".into(),
            ..Default::default()
        }
    }

    /// From the JSON the host script builds: `{url, method, headers, body | bodyBase64}`.
    pub(crate) fn from_json(input: &Value, base: &str) -> ModuleResult<Self> {
        let raw = input.get("url").and_then(Value::as_str).unwrap_or_default();
        let url = web_url(&if base.is_empty() {
            raw.to_owned()
        } else {
            resolve_url(raw, base)?
        })?;
        let method = input
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("GET")
            .to_ascii_uppercase();
        if !matches!(
            method.as_str(),
            "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS"
        ) {
            return Err(ModuleError::invalid());
        }
        let headers = input
            .get("headers")
            .and_then(Value::as_object)
            .map(|h| {
                h.iter()
                    .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
                    .collect()
            })
            .unwrap_or_default();
        let body = match (input.get("bodyBase64"), input.get("body")) {
            (Some(Value::String(b64)), _) => Some(
                base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|_| ModuleError::invalid())?,
            ),
            (_, Some(Value::String(text))) => Some(text.as_bytes().to_vec()),
            _ => None,
        };
        if body.as_ref().is_some_and(|b| b.len() > MAX_BODY_BYTES) {
            return Err(ModuleError::invalid());
        }
        Ok(Self {
            url,
            method,
            headers,
            body,
        })
    }
}

/// A buffered response.
#[derive(Debug, Clone)]
pub(crate) struct Response {
    pub status: u16,
    pub status_text: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub(crate) fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// `{status, ok, statusText, url, headers, text, base64}` for the JS side.
    pub(crate) fn to_json(&self) -> Value {
        let mut headers = Map::new();
        for (name, value) in &self.headers {
            match headers.get_mut(name) {
                Some(Value::Array(values)) => values.push(value.clone().into()),
                Some(existing) => {
                    let first = existing.take();
                    *existing = Value::Array(vec![first, value.clone().into()]);
                }
                None => {
                    headers.insert(name.clone(), value.clone().into());
                }
            }
        }
        serde_json::json!({
            "status": self.status,
            "ok": self.ok(),
            "statusText": self.status_text,
            "url": self.url,
            "headers": headers,
            "text": String::from_utf8_lossy(&self.body),
            "base64": base64::engine::general_purpose::STANDARD.encode(&self.body),
        })
    }
}

fn agent() -> ureq::Agent {
    static AGENT: Mutex<Option<(HttpConfig, ureq::Agent)>> = Mutex::new(None);
    let config = crate::http::http_config();
    let mut slot = AGENT.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((cached, agent)) = slot.as_ref()
        && *cached == config
    {
        return agent.clone();
    }
    let proxy = config
        .proxy
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .and_then(|p| ureq::Proxy::new(p).ok());
    let mut builder = ureq::Agent::config_builder()
        .proxy(proxy)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(READ_TIMEOUT))
        .timeout_recv_body(Some(READ_TIMEOUT))
        .timeout_global(Some(CALL_TIMEOUT));
    if let Some(ua) = &config.user_agent {
        builder = builder.user_agent(ua.as_str());
    }
    let agent: ureq::Agent = builder.build().into();
    *slot = Some((config, agent.clone()));
    agent
}

fn same_origin(a: &str, b: &str) -> bool {
    match (url::Url::parse(a), url::Url::parse(b)) {
        (Ok(a), Ok(b)) => a.origin() == b.origin(),
        _ => false,
    }
}

/// Performs `request`, following redirects. Blocking; gives up at `deadline`
/// and as soon as `cancel` is set (checked between reads).
pub(crate) fn execute(
    mut request: Request,
    limit: usize,
    deadline: Option<Instant>,
    cancel: &AtomicBool,
) -> ModuleResult<Response> {
    let network = |code: &str| ModuleError::at(Failure::Network, Stage::Provider, code);
    for _ in 0..=MAX_REDIRECTS {
        let remaining = deadline.map(|d| d.saturating_duration_since(Instant::now()));
        if remaining.is_some_and(|r| r.is_zero()) {
            return Err(ModuleError::at(
                Failure::Timeout,
                Stage::Provider,
                "REQUEST_TIMEOUT",
            ));
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(network("ABORTED"));
        }
        let timeout = remaining.map_or(CALL_TIMEOUT, |r| r.min(CALL_TIMEOUT));
        let uri: ureq::http::Uri = request.url.parse().map_err(|_| ModuleError::invalid())?;
        let mut builder = ureq::http::Request::builder()
            .method(request.method.as_str())
            .uri(uri);
        for (k, v) in &request.headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        let agent = agent();
        let run = |req: ureq::http::Request<Vec<u8>>| {
            let req = agent
                .configure_request(req)
                .timeout_global(Some(timeout))
                .build();
            agent.run(req)
        };
        let body = match &request.body {
            Some(body) if !matches!(request.method.as_str(), "GET" | "HEAD") => body.clone(),
            _ => Vec::new(),
        };
        let result = run(builder.body(body).map_err(|_| ModuleError::invalid())?);
        let mut resp = result.map_err(|e| match e {
            ureq::Error::Timeout(_) => {
                ModuleError::at(Failure::Timeout, Stage::Provider, "REQUEST_TIMEOUT")
            }
            _ => network("REQUEST_NETWORK"),
        })?;
        let status = resp.status().as_u16();
        if matches!(status, 301 | 302 | 303 | 307 | 308)
            && let Some(location) = resp.headers().get("location").and_then(|l| l.to_str().ok())
        {
            let next = web_url(&resolve_url(location, &request.url)?)?;
            if !same_origin(&next, &request.url) {
                request.headers.retain(|(k, _)| {
                    !["authorization", "cookie", "proxy-authorization"]
                        .contains(&k.to_ascii_lowercase().as_str())
                });
            }
            if status == 303 || (matches!(status, 301 | 302) && request.method == "POST") {
                request.method = "GET".into();
                request.body = None;
                request
                    .headers
                    .retain(|(k, _)| !k.eq_ignore_ascii_case("content-type"));
            }
            request.url = next;
            continue;
        }
        let headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .filter_map(|(k, v)| {
                v.to_str()
                    .ok()
                    .map(|v| (k.as_str().to_owned(), v.to_owned()))
            })
            .collect();
        let declared = resp
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        if declared.is_some_and(|len| len > limit as u64) {
            return Err(ModuleError::at(
                Failure::InvalidDocument,
                Stage::Provider,
                "RESPONSE_SIZE",
            ));
        }
        let status_text = resp
            .status()
            .canonical_reason()
            .unwrap_or_default()
            .to_owned();
        let mut body = Vec::new();
        let mut reader = resp.body_mut().as_reader();
        let mut piece = [0u8; 16 * 1024];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(network("ABORTED"));
            }
            match reader.read(&mut piece) {
                Ok(0) => break,
                Ok(n) => {
                    if body.len() + n > limit {
                        return Err(ModuleError::at(
                            Failure::InvalidDocument,
                            Stage::Provider,
                            "RESPONSE_SIZE",
                        ));
                    }
                    body.extend_from_slice(&piece[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                    return Err(ModuleError::at(
                        Failure::Timeout,
                        Stage::Provider,
                        "REQUEST_TIMEOUT",
                    ));
                }
                Err(_) => return Err(network("REQUEST_NETWORK")),
            }
        }
        return Ok(Response {
            status,
            status_text,
            url: request.url,
            headers,
            body,
        });
    }
    Err(network("TOO_MANY_REDIRECTS"))
}

/// Downloads a package / descriptor / catalog: `(bytes, final URL)`.
pub(crate) fn source(url: &str) -> ModuleResult<(Vec<u8>, String)> {
    let url = web_url(url)?;
    let resp = execute(
        Request::get(&url),
        MAX_SOURCE_BYTES,
        Some(Instant::now() + Duration::from_secs(30)),
        &AtomicBool::new(false),
    )
    .map_err(|e| e.with_stage(Stage::Download))?;
    if !resp.ok() {
        return Err(ModuleError::at(
            Failure::Network,
            Stage::Download,
            format!("HTTP_{}", resp.status),
        ));
    }
    Ok((resp.body, resp.url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_validated_and_resolved() {
        assert!(web_url("https://example.org/a?b=1").is_ok());
        assert!(web_url("ftp://example.org").is_err());
        assert!(web_url("https://user:pw@example.org").is_err());
        assert!(web_url("/relative").is_err());
        assert_eq!(
            resolve_url("../x.arch", "https://example.org/a/b/catalog.json").unwrap(),
            "https://example.org/a/x.arch"
        );
        let parts = url_parts("/p?q=1#h", Some("http://127.0.0.1:8080/base")).unwrap();
        assert_eq!(parts["host"], "127.0.0.1:8080");
        assert_eq!(parts["pathname"], "/p");
        assert_eq!(parts["search"], "?q=1");
        assert_eq!(parts["origin"], "http://127.0.0.1:8080");
    }
}
