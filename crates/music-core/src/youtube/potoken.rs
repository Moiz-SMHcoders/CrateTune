// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/utils/potoken/{BotGuardTokenGenerator.kt, ChallengeParser.kt} — © Rukamori and ArchiveTune contributors, GPL-3.0
//! Proof-of-origin tokens ("PoTokens") from YouTube's BotGuard attestation.
//!
//! The flow, as YouTube's web player does it:
//! 1. fetch `youtube.com`; the page embeds `ytcfg` and a BotGuard challenge
//!    (`window.ytAtN({..bgChallenge..})`) naming an interpreter script;
//! 2. run the interpreter + program in a real browser engine ([`BotGuardVm`]) →
//!    a BotGuard response and a minter factory (`webPoSignalOutput[0]`);
//! 3. exchange the response for an integrity token (`api/jnn/v1/GenerateIT`);
//! 4. create the minter once, then mint a token per identifier — the video id
//!    ("content-bound"), which is what googlevideo checks for web clients.
//!
//! Step 2 needs a browser JS environment, so it is behind [`BotGuardVm`]:
//! a hidden WKWebView on macOS (`webkit.rs`), WebView2 on Windows
//! (`webview2.rs`), WebKitGTK on Linux (`webkitgtk.rs`, opt-in feature).

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE};
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::http::{agent, read_body};
use crate::{Error, Result};

/// Mints PoTokens bound to an identifier (a video id for content-bound tokens).
pub trait PoTokenProvider: Send + Sync {
    /// A token for `video_id`, base64url-encoded. Blocking (cold start ~1 s).
    fn content_token(&self, video_id: &str) -> Result<String>;
    /// Drop cached state (e.g. after googlevideo rejected a token).
    fn reset(&self) {}
    /// Treat the attestation (integrity token) as expired now, so the next
    /// [`Self::content_token`] renews it, exactly as when its lifetime runs out.
    /// For tests and checks of the renewal; providers without one ignore it.
    fn expire_attestation(&self) {}
}

/// A browser JS environment that can run BotGuard (see the module docs).
///
/// Implementations run `assets/po_token.html` (which defines `runBotGuard`,
/// `createPoTokenMinter` and `obtainPoToken`) in a real web engine.
pub trait BotGuardVm: Send + Sync {
    /// Loads a fresh page and runs the challenge: sets `globalThis.yt.config_`
    /// to `config_json`, awaits `runBotGuard(challenge)`, keeps
    /// `webPoSignalOutput`, and returns the BotGuard response string.
    fn run_challenge(&self, config_json: &str, challenge_json: &str) -> Result<String>;
    /// `createPoTokenMinter(webPoSignalOutput, integrityToken)`.
    fn create_minter(&self, integrity_token: &[u8]) -> Result<()>;
    /// `obtainPoToken(identifier)` → token bytes.
    fn mint(&self, identifier: &[u8]) -> Result<Vec<u8>>;
    /// Release the page (memory); the next [`Self::run_challenge`] starts over.
    fn release(&self) {}
    /// The User-Agent of the engine BotGuard runs in; the challenge page and
    /// `GenerateIT` are fetched with it so both sides look like one browser.
    fn user_agent(&self) -> &'static str {
        ATTESTATION_UA
    }
}

/// The page BotGuard runs in.
pub const PO_TOKEN_HTML: &str = include_str!("../../assets/po_token.html");

/// Matches the WebKit engine the challenge actually runs in (a Chrome identity
/// on WebKit is a mismatch BotGuard can see).
pub(crate) const ATTESTATION_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";
const CHALLENGE_PAGE: &str = "https://www.youtube.com/?hl=en";
const GENERATE_IT: &str = "https://www.youtube.com/api/jnn/v1/GenerateIT";
/// YouTube web player's WAA request key and API key (public, embedded in its JS).
const REQUEST_KEY: &str = "O43z0dpjhgX20SCx4KAo";
const API_KEY: &str = "AIzaSyDyT5W0Jh49F30Pqqtyfdf7pDLFKLJoAnw";
/// Renew the minter this long before the integrity token expires.
const EXPIRY_MARGIN: Duration = Duration::from_secs(10 * 60);
const TOKEN_CACHE: usize = 200;

/// [`PoTokenProvider`] on top of a [`BotGuardVm`]: bootstraps lazily, keeps the
/// minter until its integrity token is about to expire, caches tokens per video.
pub struct BotGuardTokenProvider<V: BotGuardVm> {
    vm: V,
    state: Mutex<State>,
    /// Unit tests replace the network part of [`Self::bootstrap`].
    #[cfg(test)]
    fake_bootstrap: Option<fn(&V) -> Result<Duration>>,
}

#[derive(Default)]
struct State {
    /// The minter is ready until then.
    ready_until: Option<Instant>,
    tokens: HashMap<String, String>,
    order: VecDeque<String>,
}

impl<V: BotGuardVm> BotGuardTokenProvider<V> {
    pub fn new(vm: V) -> Self {
        Self {
            vm,
            state: Mutex::default(),
            #[cfg(test)]
            fake_bootstrap: None,
        }
    }

    fn bootstrap(&self) -> Result<Duration> {
        #[cfg(test)]
        if let Some(fake) = self.fake_bootstrap {
            return fake(&self.vm);
        }
        let started = Instant::now();
        let ua = self.vm.user_agent();
        let page = fetch_text(CHALLENGE_PAGE, None, ua)?;
        let challenge = parse_page_challenge(&page)?;
        let interpreter_url = challenge.interpreter_url()?;
        let interpreter = fetch_text(&interpreter_url, None, ua)?;
        if interpreter.trim().is_empty() {
            return Err(Error::Parse("BotGuard: empty interpreter".into()));
        }
        let mut bg = challenge.challenge;
        bg.insert(
            "interpreterJavascript".into(),
            json!({ "privateDoNotAccessOrElseSafeScriptWrappedValue": interpreter }),
        );
        let response = self.vm.run_challenge(
            &Value::Object(challenge.configuration).to_string(),
            &Value::Object(bg).to_string(),
        )?;
        let body = json!([REQUEST_KEY, response]).to_string();
        let raw = fetch_text(GENERATE_IT, Some(&body), ua)?;
        let (token, lifetime) = parse_integrity_token(&raw)?;
        self.vm.create_minter(&token)?;
        log::info!(
            "youtube: BotGuard ready in {:?} (integrity token valid {lifetime:?})",
            started.elapsed()
        );
        Ok(lifetime)
    }
}

impl<V: BotGuardVm> PoTokenProvider for BotGuardTokenProvider<V> {
    fn content_token(&self, video_id: &str) -> Result<String> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let ready = state
            .ready_until
            .is_some_and(|until| Instant::now() < until);
        if ready && let Some(token) = state.tokens.get(video_id) {
            return Ok(token.clone());
        }
        if !ready {
            state.tokens.clear();
            state.order.clear();
            state.ready_until = None;
            let lifetime = self.bootstrap().inspect_err(|_| self.vm.release())?;
            state.ready_until = Some(
                Instant::now()
                    + lifetime
                        .saturating_sub(EXPIRY_MARGIN)
                        .max(Duration::from_secs(60)),
            );
        }
        let token = match self.vm.mint(video_id.as_bytes()) {
            Ok(bytes) => URL_SAFE.encode(bytes),
            Err(e) => {
                state.ready_until = None;
                self.vm.release();
                return Err(e);
            }
        };
        state.tokens.insert(video_id.to_owned(), token.clone());
        state.order.push_back(video_id.to_owned());
        while state.order.len() > TOKEN_CACHE {
            if let Some(old) = state.order.pop_front() {
                state.tokens.remove(&old);
            }
        }
        Ok(token)
    }

    fn reset(&self) {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = State::default();
        self.vm.release();
    }

    fn expire_attestation(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.ready_until.is_some() {
            state.ready_until = Some(Instant::now());
        }
    }
}

fn fetch_text(url: &str, json_body: Option<&str>, user_agent: &str) -> Result<String> {
    let mut resp = match json_body {
        None => agent()
            .get(url)
            .header("User-Agent", user_agent)
            .header("Accept", "*/*")
            .header("Accept-Language", "en-US,en;q=0.7")
            .call()?,
        Some(body) => agent()
            .post(url)
            .header("User-Agent", user_agent)
            .header("Accept", "*/*")
            .header("Accept-Language", "en-US,en;q=0.7")
            .header("Content-Type", "application/json+protobuf")
            .header("x-goog-api-key", API_KEY)
            .header("x-user-agent", "grpc-web-javascript/0.1")
            .send(body)?,
    };
    Ok(String::from_utf8_lossy(&read_body(&mut resp)?).into_owned())
}

/// The page's merged `ytcfg` and its BotGuard challenge.
pub(crate) struct PageChallenge {
    pub configuration: Map<String, Value>,
    pub challenge: Map<String, Value>,
}

impl PageChallenge {
    /// The interpreter script URL, only from Google's own script hosts.
    fn interpreter_url(&self) -> Result<String> {
        let raw = self
            .challenge
            .get("interpreterUrl")
            .and_then(|v| v.get("privateDoNotAccessOrElseTrustedResourceUrlWrappedValue"))
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Parse("BotGuard: no interpreter URL".into()))?;
        let full = if raw.starts_with("//") {
            format!("https:{raw}")
        } else {
            raw.to_owned()
        };
        let url = url::Url::parse(&full)
            .map_err(|_| Error::Parse("BotGuard: bad interpreter URL".into()))?;
        let allowed = url.scheme() == "https"
            && matches!(
                url.host_str(),
                Some("www.youtube.com" | "www.google.com" | "www.gstatic.com")
            );
        if !allowed {
            return Err(Error::Parse(format!(
                "BotGuard: unexpected interpreter host {url}"
            )));
        }
        Ok(full)
    }
}

pub(crate) fn parse_page_challenge(html: &str) -> Result<PageChallenge> {
    static YTCFG: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static ATTEST: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let ytcfg = YTCFG.get_or_init(|| Regex::new(r"ytcfg\.set\s*\(\s*\{").unwrap());
    let attest = ATTEST.get_or_init(|| Regex::new(r"window\.ytAtN\s*\(\s*\{").unwrap());

    let mut configuration = Map::new();
    for m in ytcfg.find_iter(html) {
        let start = m.end() - 1;
        if let Ok(Value::Object(obj)) = extract_object(html, start).and_then(parse_js_object) {
            configuration.extend(obj);
        }
    }
    if configuration
        .get("EVENT_ID")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err(Error::Parse("BotGuard page: no EVENT_ID".into()));
    }
    let m = attest
        .find(html)
        .ok_or_else(|| Error::Parse("BotGuard page: no challenge".into()))?;
    let attestation = parse_js_object(extract_object(html, m.end() - 1)?)?;
    let response = match attestation.get("R") {
        Some(Value::Object(_)) => attestation["R"].clone(),
        Some(Value::String(s)) => parse_js_object(s)?,
        _ => {
            return Err(Error::Parse(
                "BotGuard page: bad attestation response".into(),
            ));
        }
    };
    let challenge = response
        .get("bgChallenge")
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| Error::Parse("BotGuard page: no bgChallenge".into()))?;
    let has = |k: &str| {
        challenge
            .get(k)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
    };
    if !has("program") || !has("globalName") {
        return Err(Error::Parse("BotGuard page: incomplete challenge".into()));
    }
    Ok(PageChallenge {
        configuration,
        challenge,
    })
}

/// The `{…}` starting at byte `start`, respecting quoted strings.
fn extract_object(source: &str, start: usize) -> Result<&str> {
    let bytes = source.as_bytes();
    if bytes.get(start) != Some(&b'{') {
        return Err(Error::Parse("BotGuard page: expected an object".into()));
    }
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    for (i, &c) in bytes.iter().enumerate().skip(start) {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            b'\'' | b'"' => quote = Some(c),
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(&source[start..=i]);
                }
            }
            _ => {}
        }
    }
    Err(Error::Parse("BotGuard page: unterminated object".into()))
}

/// A JavaScript object literal as JSON: single-quoted strings, `\xNN` escapes and
/// trailing commas are normalized.
fn parse_js_object(source: &str) -> Result<Value> {
    let mut out = String::with_capacity(source.len());
    let mut quote: Option<char> = None;
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            match c {
                '\\' => {
                    let escaped = chars
                        .next()
                        .ok_or_else(|| Error::Parse("BotGuard page: dangling escape".into()))?;
                    match escaped {
                        'x' => {
                            let hex: String = chars.by_ref().take(2).collect();
                            if hex.len() != 2 || !hex.chars().all(|h| h.is_ascii_hexdigit()) {
                                return Err(Error::Parse("BotGuard page: bad \\x escape".into()));
                            }
                            out.push_str("\\u00");
                            out.push_str(&hex);
                        }
                        '\'' => out.push('\''),
                        other => {
                            out.push('\\');
                            out.push(other);
                        }
                    }
                }
                c if c == q => {
                    quote = None;
                    out.push('"');
                }
                '"' => out.push_str("\\\""),
                c => out.push(c),
            }
            continue;
        }
        match c {
            '\'' | '"' => {
                quote = Some(c);
                out.push('"');
            }
            ',' => {
                let mut rest = chars.clone();
                while rest.peek().is_some_and(|c| c.is_whitespace()) {
                    rest.next();
                }
                if !matches!(rest.peek(), Some('}' | ']')) {
                    out.push(',');
                }
            }
            c => out.push(c),
        }
    }
    if quote.is_some() {
        return Err(Error::Parse("BotGuard page: unterminated string".into()));
    }
    serde_json::from_str(&out).map_err(|e| Error::Parse(format!("BotGuard page object: {e}")))
}

/// `[base64 token, lifetime seconds, …]` from GenerateIT.
pub(crate) fn parse_integrity_token(raw: &str) -> Result<(Vec<u8>, Duration)> {
    let arr: Vec<Value> =
        serde_json::from_str(raw).map_err(|e| Error::Parse(format!("GenerateIT response: {e}")))?;
    let token = arr
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Parse("GenerateIT: no token".into()))?;
    let normalized: String = token
        .chars()
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            '.' => '=',
            c => c,
        })
        .collect();
    let bytes = STANDARD
        .decode(normalized)
        .map_err(|_| Error::Parse("GenerateIT: token is not base64".into()))?;
    let lifetime = arr.get(1).and_then(Value::as_u64).unwrap_or(3600);
    Ok((bytes, Duration::from_secs(lifetime)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"<html><script>ytcfg.set({"EVENT_ID":"ev1","INNERTUBE_API_KEY":'k\x3d1',});
ytcfg.set({"VISITOR_DATA":"vd"});</script><script>window.ytAtN({'R':'{"bgChallenge":{"program":"prog","globalName":"trayride","interpreterUrl":{"privateDoNotAccessOrElseTrustedResourceUrlWrappedValue":"//www.google.com/js/th/x.js"},}}'});</script>"#;

    #[test]
    fn page_challenge() {
        let c = parse_page_challenge(PAGE).unwrap();
        assert_eq!(c.configuration["EVENT_ID"], "ev1");
        assert_eq!(c.configuration["INNERTUBE_API_KEY"], "k=1");
        assert_eq!(c.configuration["VISITOR_DATA"], "vd");
        assert_eq!(c.challenge["globalName"], "trayride");
        assert_eq!(
            c.interpreter_url().unwrap(),
            "https://www.google.com/js/th/x.js"
        );
    }

    #[test]
    fn rejects_foreign_interpreter_and_missing_parts() {
        let evil = PAGE.replace("www.google.com", "evil.example");
        let c = parse_page_challenge(&evil).unwrap();
        assert!(c.interpreter_url().is_err());
        assert!(parse_page_challenge("ytcfg.set({\"A\":1})").is_err());
        let no_program = PAGE.replace(r#""program":"prog","#, "");
        assert!(parse_page_challenge(&no_program).is_err());
    }

    #[test]
    fn js_object_normalization() {
        let v = parse_js_object(r#"{'a':'it\'s "x"','b':[1,2,],"c":'\x41',}"#).unwrap();
        assert_eq!(v["a"], "it's \"x\"");
        assert_eq!(v["b"], json!([1, 2]));
        assert_eq!(v["c"], "A");
    }

    #[test]
    fn integrity_token() {
        let (bytes, lifetime) = parse_integrity_token(r#"["AQID-_8.",43200,"x"]"#).unwrap();
        assert_eq!(bytes, vec![1, 2, 3, 251, 255]);
        assert_eq!(lifetime, Duration::from_secs(43200));
        assert!(parse_integrity_token("{}").is_err());
    }

    /// A fake VM: counts bootstraps, mints `id` reversed (plus the minter's
    /// generation, so tokens from a renewed attestation differ).
    #[derive(Default)]
    struct FakeVm {
        runs: std::sync::atomic::AtomicUsize,
        minters: std::sync::atomic::AtomicUsize,
        mints: std::sync::atomic::AtomicUsize,
    }

    impl BotGuardVm for FakeVm {
        fn run_challenge(&self, _: &str, _: &str) -> Result<String> {
            self.runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok("bg".into())
        }
        fn create_minter(&self, _: &[u8]) -> Result<()> {
            self.minters
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        fn mint(&self, id: &[u8]) -> Result<Vec<u8>> {
            self.mints.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let generation = self.minters.load(std::sync::atomic::Ordering::SeqCst) as u8;
            let mut token: Vec<u8> = id.iter().rev().copied().collect();
            token.push(generation);
            Ok(token)
        }
    }

    /// Bootstrap without the network: the challenge and minter on the fake VM,
    /// an integrity token valid for 12 h.
    fn offline_bootstrap(vm: &FakeVm) -> Result<Duration> {
        vm.run_challenge("{}", "{}")?;
        vm.create_minter(b"it")?;
        Ok(Duration::from_secs(12 * 3600))
    }

    #[test]
    fn attestation_is_renewed_when_the_integrity_token_expires() {
        use std::sync::atomic::Ordering::SeqCst;
        let mut provider = BotGuardTokenProvider::new(FakeVm::default());
        provider.fake_bootstrap = Some(offline_bootstrap);
        let first = provider.content_token("abc").unwrap();
        assert_eq!(provider.vm.runs.load(SeqCst), 1);
        // Valid for its lifetime minus the 10 min margin; tokens are cached.
        let until = provider.state.lock().unwrap().ready_until.unwrap();
        let left = until.duration_since(Instant::now());
        assert!(
            left > Duration::from_secs(11 * 3600) && left < Duration::from_secs(12 * 3600 - 590)
        );
        assert_eq!(provider.content_token("abc").unwrap(), first);
        assert_eq!(provider.vm.mints.load(SeqCst), 1);

        // The lifetime runs out: the next token renews the attestation first,
        // and tokens minted under the old one are not handed out again.
        provider.expire_attestation();
        let renewed = provider.content_token("abc").unwrap();
        assert_eq!(provider.vm.runs.load(SeqCst), 2);
        assert_ne!(renewed, first);
        assert_eq!(provider.state.lock().unwrap().tokens.len(), 1);
        assert!(provider.state.lock().unwrap().ready_until.unwrap() > Instant::now());
        provider.content_token("def").unwrap();
        assert_eq!(provider.vm.runs.load(SeqCst), 2, "no renewal while valid");

        // A short lifetime still keeps the minter for at least a minute.
        provider.fake_bootstrap = Some(|vm| {
            offline_bootstrap(vm)?;
            Ok(Duration::from_secs(300))
        });
        provider.expire_attestation();
        provider.content_token("abc").unwrap();
        let left = provider
            .state
            .lock()
            .unwrap()
            .ready_until
            .unwrap()
            .duration_since(Instant::now());
        assert!(left > Duration::from_secs(55) && left <= Duration::from_secs(60));

        // A failed renewal is an error now and retried next time.
        provider.fake_bootstrap = Some(|_| Err(Error::Parse("BotGuard page: no challenge".into())));
        provider.expire_attestation();
        assert!(provider.content_token("abc").is_err());
        assert!(provider.state.lock().unwrap().ready_until.is_none());
        provider.fake_bootstrap = Some(offline_bootstrap);
        provider.content_token("abc").unwrap();
    }

    #[test]
    fn provider_caches_tokens_per_video() {
        let provider = BotGuardTokenProvider::new(FakeVm::default());
        // Pretend bootstrap already happened (no network in unit tests).
        provider.state.lock().unwrap().ready_until = Some(Instant::now() + Duration::from_secs(60));
        let a = provider.content_token("abc").unwrap();
        assert_eq!(URL_SAFE.decode(&a).unwrap(), b"cba\0");
        assert_eq!(provider.content_token("abc").unwrap(), a);
        assert_eq!(provider.state.lock().unwrap().tokens.len(), 1);
        assert_eq!(
            provider.vm.runs.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }
}
