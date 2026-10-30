// Ported from ArchiveTune core/src/main/kotlin/moe/rukamori/archivetune/innertube/{InnerTube.kt, models/Context.kt, models/YouTubeClient.kt} — © Rukamori and ArchiveTune contributors, GPL-3.0
//! Request plumbing: client context, headers, auth, continuations.

use std::sync::{Mutex, PoisonError};

use serde_json::{Value, json};

use super::auth::unix_now;
use super::{Auth, InnerTube};
use crate::http::{agent, user_agent_override};
use crate::{Error, Result};

pub(crate) const ORIGIN: &str = "https://music.youtube.com";
/// Path of the InnerTube API under [`ORIGIN`] (or an [`InnerTube::with_api_base`] origin).
const API_PATH: &str = "/youtubei/v1";
pub(crate) const CLIENT_NAME: &str = "WEB_REMIX";
/// `X-YouTube-Client-Name` for WEB_REMIX.
const CLIENT_ID: &str = "67";
pub(crate) const CLIENT_VERSION: &str = "1.20251020.01.00";
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// Whether a request carries the account cookie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Login {
    /// Use the account if one is configured (personalized pages), else anonymous.
    IfAvailable,
    /// Fail with an error unless signed in.
    Required,
}

/// `visitorData` YouTube assigned to anonymous requests in this process. Continuation
/// tokens (home feed, …) are bound to it, like the `VISITOR_INFO1_LIVE` cookie in a browser.
static ANON_VISITOR: Mutex<Option<String>> = Mutex::new(None);

fn anon_visitor() -> Option<String> {
    ANON_VISITOR
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// `MUSIC_INNERTUBE_API=http://127.0.0.1:PORT`: a stand-in for music.youtube.com, so
/// the app's account flows can be driven against a fake server. Only loopback
/// origins are accepted (the cookie is sent along); anything else is ignored.
pub(crate) fn env_api_base() -> Option<String> {
    static BASE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BASE.get_or_init(|| {
        let raw = std::env::var("MUSIC_INNERTUBE_API").ok()?;
        let base = loopback_base(&raw);
        if base.is_none() && !raw.trim().is_empty() {
            eprintln!("music-core: MUSIC_INNERTUBE_API must be http://127.0.0.1:PORT (or localhost); ignored");
        }
        base
    })
    .clone()
}

/// `raw` without a trailing slash if it is an `http://` loopback origin.
pub(crate) fn loopback_base(raw: &str) -> Option<String> {
    let raw = raw.trim().trim_end_matches('/');
    let rest = raw.strip_prefix("http://")?;
    let host_port = rest.split('/').next().unwrap_or_default();
    let host = match host_port.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => host_port.split(':').next().unwrap_or_default(),
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1").then(|| raw.to_owned())
}

fn remember_anon_visitor(resp: &Value) {
    let Some(vd) = resp
        .pointer("/responseContext/visitorData")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
    else {
        return;
    };
    let mut slot = ANON_VISITOR.lock().unwrap_or_else(PoisonError::into_inner);
    if slot.is_none() {
        *slot = Some(vd.to_owned());
    }
}

/// `error.code` of an InnerTube error object (0 if missing).
fn err_code(err: &Value) -> u16 {
    err.get("code")
        .and_then(Value::as_u64)
        .and_then(|c| u16::try_from(c).ok())
        .unwrap_or(0)
}

/// Whether a failed signed-in request means "your sign-in is no longer valid":
/// 401, or a 403 / error body whose status says unauthenticated, or whose response
/// context reports the request as signed out.
pub(crate) fn signed_out_status(status: u16, body: Option<&Value>) -> bool {
    if status == 401 {
        return true;
    }
    let Some(body) = body else {
        return false;
    };
    let error_status = body.pointer("/error/status").and_then(Value::as_str);
    if error_status == Some("UNAUTHENTICATED") {
        return true;
    }
    status == 403 && logged_out_context(body)
}

/// `responseContext.serviceTrackingParams[*].params` has `logged_in` = `"0"`.
fn logged_out_context(body: &Value) -> bool {
    body.pointer("/responseContext/serviceTrackingParams")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|svc| svc.get("params").and_then(Value::as_array))
        .flatten()
        .any(|p| {
            p.get("key").and_then(Value::as_str) == Some("logged_in")
                && p.get("value").and_then(Value::as_str) == Some("0")
        })
}

impl InnerTube {
    /// The anonymous `visitorData` of this process (assigned by YouTube on the first
    /// anonymous request, or restored with [`InnerTube::restore_visitor_data`]), for the
    /// app to keep across launches.
    pub fn anonymous_visitor_data() -> Option<String> {
        anon_visitor()
    }

    /// Reuses a `visitorData` saved by an earlier launch for anonymous requests (and as
    /// the `X-Goog-Visitor-Id` of signed-in ones without their own). YouTube ties its
    /// anonymous recommendations to the visitor; without this, every launch starts as a
    /// new visitor and the first request (usually the home feed) carries none. Ignored
    /// if this process already has one or `vd` is empty. Returns whether it was taken.
    pub fn restore_visitor_data(vd: &str) -> bool {
        let vd = vd.trim();
        if vd.is_empty() || !vd.is_ascii() {
            return false;
        }
        let mut slot = ANON_VISITOR.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.is_some() {
            return false;
        }
        *slot = Some(vd.to_owned());
        true
    }

    /// Forgets the anonymous `visitorData`, so the next anonymous request starts as a new
    /// visitor. YouTube ties its anonymous feed to the visitor as well as to `gl`, so after a
    /// change of content country the old visitor keeps the old region's recommendations.
    pub fn reset_anonymous_visitor() {
        *ANON_VISITOR.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    fn signed_in(&self) -> bool {
        self.auth.as_ref().is_some_and(|a| a.is_signed_in())
    }

    /// `context` object for a request body.
    pub(crate) fn context(&self, login: bool) -> Value {
        let mut client = json!({
            "clientName": CLIENT_NAME,
            "clientVersion": CLIENT_VERSION,
            "hl": self.hl,
            "gl": self.gl,
        });
        let mut user = json!({ "lockedSafetyMode": false });
        if let Some(vd) = self.visitor_data() {
            client["visitorData"] = json!(vd);
        }
        if login && let Some(delegated) = self.auth.as_ref().and_then(|a| a.delegated_session_id())
        {
            user["onBehalfOfUser"] = json!(delegated);
        }
        json!({ "client": client, "user": user })
    }

    /// Full request body: `extra` fields plus `context`.
    pub(crate) fn body(&self, mut extra: Value, login: bool) -> Value {
        if !extra.is_object() {
            extra = json!({});
        }
        extra["context"] = self.context(login);
        extra
    }

    /// Request headers (without `Content-Type`).
    pub(crate) fn headers(&self, login: bool) -> Vec<(&'static str, String)> {
        let mut h = self.anonymous_headers();
        if login && let Some(auth) = &self.auth {
            h.extend(auth.headers(ORIGIN, unix_now()));
        }
        h
    }

    /// [`InnerTube::headers`] without the account's.
    fn anonymous_headers(&self) -> Vec<(&'static str, String)> {
        let mut h = vec![
            (
                "User-Agent",
                user_agent_override().unwrap_or_else(|| USER_AGENT.to_owned()),
            ),
            ("Origin", ORIGIN.to_owned()),
            ("X-Origin", ORIGIN.to_owned()),
            ("Referer", format!("{ORIGIN}/")),
            ("X-Goog-Api-Format-Version", "1".to_owned()),
            ("X-YouTube-Client-Name", CLIENT_ID.to_owned()),
            ("X-YouTube-Client-Version", CLIENT_VERSION.to_owned()),
        ];
        if let Some(vd) = self.visitor_data() {
            h.push(("X-Goog-Visitor-Id", vd));
        }
        h
    }

    /// The account's `visitorData`, else the one remembered for anonymous requests.
    pub(crate) fn visitor_data(&self) -> Option<String> {
        match self.auth.as_ref().and_then(|a| a.visitor_data.as_deref()) {
            Some(vd) if !vd.is_empty() => Some(vd.to_owned()),
            _ => anon_visitor(),
        }
    }

    /// POSTs to `youtubei/v1/<endpoint>`; `query` is appended to the URL.
    ///
    /// Signed in, a response's `Set-Cookie` lines update the session (`cookies.rs`).
    /// When YouTube rejects the sign-in ([`Error::Auth`]), the session gets one chance
    /// to recover (cookie changed meanwhile, or a silent re-login through the app's
    /// hooks) and the request is retried once. If that fails, a request that only
    /// *may* be signed in ([`Login::IfAvailable`]) is sent anonymously instead; one
    /// that must be fails with [`Error::Auth`].
    pub(crate) fn request(
        &self,
        endpoint: &str,
        extra: Value,
        query: &[(&str, &str)],
        login: Login,
    ) -> Result<Value> {
        let expired = self.auth.as_ref().is_some_and(Auth::is_expired);
        let use_login = match login {
            // A session known to be signed out browses anonymously until it works again.
            Login::IfAvailable => self.signed_in() && !expired,
            Login::Required if self.signed_in() => true,
            Login::Required => {
                return Err(Error::Parse(format!(
                    "{endpoint}: needs a signed-in account (set InnerTube::auth with YouTube Music cookies)"
                )));
            }
        };
        let body = self.body(extra, use_login);
        if let Some(dir) = crate::fixtures::dir() {
            return crate::fixtures::innertube(dir, endpoint, &body);
        }
        if !use_login {
            return self.send(endpoint, &body, query, false);
        }
        let session = self.auth.as_ref().and_then(|a| a.session.clone());
        let revision = session.as_ref().map(|s| s.revision());
        let first = self.send(endpoint, &body, query, true);
        let err = match first {
            Err(e) if e.is_auth() => e,
            other => {
                if other.is_ok()
                    && let Some(s) = &session
                {
                    s.note_success();
                }
                return other;
            }
        };
        let Some(session) = session else {
            return Err(err);
        };
        let retried = if session.recover(revision.unwrap_or_default()) {
            match self.send(endpoint, &body, query, true) {
                Ok(v) => {
                    session.note_success();
                    return Ok(v);
                }
                Err(e) if e.is_auth() => {
                    session.set_expired(true);
                    e
                }
                Err(e) => return Err(e),
            }
        } else {
            err
        };
        match login {
            Login::IfAvailable => {
                let anonymous = self.body(body, false);
                self.send(endpoint, &anonymous, query, false)
            }
            Login::Required => Err(retried),
        }
    }

    /// One POST. Errors: [`Error::Auth`] for a rejected sign-in (401; 403 or an error
    /// body saying unauthenticated), [`Error::Http`] for other HTTP statuses.
    fn send(
        &self,
        endpoint: &str,
        body: &Value,
        query: &[(&str, &str)],
        use_login: bool,
    ) -> Result<Value> {
        let origin = self.api_base.as_deref().unwrap_or(ORIGIN);
        let mut req = agent()
            .post(format!("{origin}{API_PATH}/{endpoint}"))
            .config()
            .http_status_as_error(false)
            .build()
            .query("prettyPrint", "false");
        for (k, v) in query {
            req = req.query(*k, *v);
        }
        let auth = self.auth.as_ref().filter(|_| use_login);
        for (k, v) in self.anonymous_headers() {
            req = req.header(k, v);
        }
        if let Some(auth) = auth {
            let (cookie, _) = auth.signing_cookie();
            for (k, v) in auth.headers_with(&cookie, ORIGIN, unix_now()) {
                req = req.header(k, v);
            }
        }
        let mut resp = req.send_json(body)?;
        if let Some(auth) = auth {
            let lines = resp
                .headers()
                .get_all(ureq::http::header::SET_COOKIE)
                .iter()
                .filter_map(|v| v.to_str().ok());
            // Only the configured stand-in may play music.youtube.com.
            let loopback_ok = self.api_base.is_some();
            auth.absorb_set_cookies(origin, loopback_ok, lines);
        }
        let status = resp.status().as_u16();
        let bytes = crate::http::read_body(&mut resp)?;
        let value: Option<Value> = serde_json::from_slice(&bytes).ok();
        if status >= 400 {
            if use_login && signed_out_status(status, value.as_ref()) {
                return Err(Error::Auth(status));
            }
            return Err(Error::Http(ureq::Error::StatusCode(status)));
        }
        let value = match value {
            Some(v) => v,
            None => serde_json::from_slice::<Value>(&bytes)
                .map_err(|e| Error::Parse(format!("{endpoint}: {e}")))?,
        };
        if let Some(err) = value.get("error") {
            if use_login && signed_out_status(err_code(err), Some(&value)) {
                return Err(Error::Auth(err_code(err)));
            }
            let msg = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Err(Error::Parse(format!("{endpoint}: {msg}")));
        }
        if !use_login {
            remember_anon_visitor(&value);
        }
        Ok(value)
    }

    pub(crate) fn post(&self, endpoint: &str, extra: Value) -> Result<Value> {
        self.request(endpoint, extra, &[], Login::IfAvailable)
    }

    pub(crate) fn post_authed(&self, endpoint: &str, extra: Value) -> Result<Value> {
        self.request(endpoint, extra, &[], Login::Required)
    }

    /// `browse` with a browse id (+ optional params).
    pub(crate) fn browse_raw(&self, browse_id: &str, params: Option<&str>) -> Result<Value> {
        let mut body = json!({ "browseId": browse_id });
        if let Some(p) = params {
            body["params"] = json!(p);
        }
        self.post("browse", body)
    }

    /// `browse` continuation. The token goes in the body: as a query parameter ureq would
    /// percent-encode the `%3D` already in it.
    pub(crate) fn browse_continuation_raw(&self, token: &str, login: Login) -> Result<Value> {
        self.request("browse", json!({ "continuation": token }), &[], login)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::innertube::Auth;

    #[test]
    fn anonymous_body_has_locale_and_no_user() {
        let it = InnerTube::default().with_locale("de", "AT");
        let body = it.body(json!({ "browseId": "FEmusic_home" }), false);
        assert_eq!(body["browseId"], "FEmusic_home");
        assert_eq!(body["context"]["client"]["clientName"], "WEB_REMIX");
        assert_eq!(body["context"]["client"]["hl"], "de");
        assert_eq!(body["context"]["client"]["gl"], "AT");
        assert!(body["context"]["user"].get("onBehalfOfUser").is_none());
        let headers = it.headers(false);
        assert!(
            headers
                .iter()
                .all(|(k, _)| *k != "Authorization" && *k != "Cookie")
        );
    }

    #[test]
    fn authed_body_and_headers() {
        let mut auth = Auth::new("SAPISID=abc; HSID=x");
        auth.visitor_data = Some("CgtWSVNJVE9S".into());
        auth.data_sync_id = Some("111||222".into());
        auth.auth_user = 1;
        let it = InnerTube::default().with_auth(auth);
        let body = it.body(json!({}), true);
        assert_eq!(body["context"]["client"]["visitorData"], "CgtWSVNJVE9S");
        assert_eq!(body["context"]["user"]["onBehalfOfUser"], "111");
        let headers = it.headers(true);
        let get = |k: &str| {
            headers
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(get("Cookie").as_deref(), Some("SAPISID=abc; HSID=x"));
        assert_eq!(get("X-Goog-AuthUser").as_deref(), Some("1"));
        assert_eq!(get("X-Goog-Visitor-Id").as_deref(), Some("CgtWSVNJVE9S"));
        assert_eq!(get("X-Origin").as_deref(), Some(ORIGIN));
        assert!(get("Authorization").unwrap().starts_with("SAPISIDHASH "));
    }

    #[test]
    fn api_base_only_accepts_loopback() {
        assert_eq!(
            loopback_base("http://127.0.0.1:8080/").as_deref(),
            Some("http://127.0.0.1:8080")
        );
        assert!(loopback_base("http://localhost:9").is_some());
        assert!(loopback_base("http://[::1]:9").is_some());
        assert!(
            loopback_base("https://127.0.0.1:9").is_none(),
            "plain http only"
        );
        assert!(loopback_base("http://127.0.0.1.evil.com").is_none());
        assert!(loopback_base("http://music.youtube.com").is_none());
        assert!(loopback_base("").is_none());
    }

    #[test]
    fn restored_visitor_data_is_sent_and_kept() {
        // Process-wide slot: this is the only test that fills it before a request.
        assert!(!InnerTube::restore_visitor_data("  "));
        let had = InnerTube::anonymous_visitor_data();
        let taken = InnerTube::restore_visitor_data("CgtSRVNUT1JFRA");
        assert_eq!(taken, had.is_none());
        let vd = InnerTube::anonymous_visitor_data().unwrap();
        // A second restore never replaces the visitor of this process.
        assert!(!InnerTube::restore_visitor_data("CgtPVEhFUg"));
        assert_eq!(
            InnerTube::anonymous_visitor_data().as_deref(),
            Some(vd.as_str())
        );
        let it = InnerTube::default();
        assert_eq!(
            it.body(json!({}), false)["context"]["client"]["visitorData"],
            vd
        );
        assert!(
            it.headers(false)
                .contains(&("X-Goog-Visitor-Id", vd.clone()))
        );
    }

    #[test]
    fn required_login_fails_fast_without_cookie() {
        let err = InnerTube::default()
            .request("account/account_menu", json!({}), &[], Login::Required)
            .unwrap_err();
        assert!(err.to_string().contains("signed-in"));
    }

    // ---- session: Set-Cookie, refresh, retry (fake server) -------------------------

    use crate::innertube::{Session, SessionHooks};
    use crate::testutil::{Reply, Request, TestServer};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    fn json_reply(status: u16, headers: &[&str], body: &str) -> Reply {
        let mut h = vec!["Content-Type: application/json".to_owned()];
        h.extend(headers.iter().map(|s| (*s).to_owned()));
        Reply::Full {
            status,
            headers: h,
            body: body.as_bytes().to_vec(),
        }
    }

    /// Cookies the fake server saw, in order.
    type Seen = Arc<Mutex<Vec<Option<String>>>>;

    fn seen_cookie(req: &Request, seen: &Seen) -> Option<String> {
        let c = req.header("Cookie").map(str::to_owned);
        seen.lock().unwrap().push(c.clone());
        c
    }

    /// Hooks whose refresh puts `fresh` in (or fails when `None`).
    struct Refresher {
        fresh: Option<String>,
        refreshes: AtomicUsize,
        expired: Mutex<Vec<bool>>,
    }

    impl Refresher {
        fn new(fresh: Option<&str>) -> Arc<Self> {
            Arc::new(Self {
                fresh: fresh.map(str::to_owned),
                refreshes: AtomicUsize::new(0),
                expired: Mutex::new(Vec::new()),
            })
        }
    }

    impl SessionHooks for Refresher {
        fn refresh(&self, session: &Session) -> bool {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            match &self.fresh {
                Some(c) => {
                    session.replace_cookie(c.clone());
                    true
                }
                None => false,
            }
        }
        fn expired_changed(&self, _: &Session, expired: bool) {
            self.expired.lock().unwrap().push(expired);
        }
    }

    fn signed_in(
        base: &str,
        cookie: &str,
        hooks: Option<Arc<Refresher>>,
    ) -> (InnerTube, Arc<Session>) {
        let session = Session::new(cookie);
        if let Some(h) = hooks {
            session.set_hooks(h);
        }
        let it = InnerTube::default()
            .with_api_base(base)
            .with_auth(Auth::new(cookie).with_session(session.clone()));
        (it, session)
    }

    #[test]
    fn set_cookie_rotates_the_shared_session() {
        let seen: Seen = Default::default();
        let log = seen.clone();
        let server = TestServer::start_http(move |req| {
            seen_cookie(req, &log);
            json_reply(
                200,
                &[
                    "Set-Cookie: __Secure-1PSIDTS=ts-new; Domain=.youtube.com; Path=/; Secure; HttpOnly",
                    "Set-Cookie: SIDCC=; Domain=.youtube.com; Path=/; Max-Age=0",
                    "Set-Cookie: NID=foreign; Domain=.google.com; Path=/",
                ],
                "{}",
            )
        });
        let (it, session) = signed_in(
            &server.base,
            "SAPISID=s; __Secure-1PSIDTS=ts-old; SIDCC=cc",
            None,
        );
        let other_clone = it.clone();
        it.request("browse", json!({}), &[], Login::Required)
            .unwrap();
        assert_eq!(session.cookie(), "SAPISID=s; __Secure-1PSIDTS=ts-new");
        // Every clone (and the next request) uses the rotated cookie.
        other_clone
            .request("browse", json!({}), &[], Login::Required)
            .unwrap();
        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen,
            [
                Some("SAPISID=s; __Secure-1PSIDTS=ts-old; SIDCC=cc".to_owned()),
                Some("SAPISID=s; __Secure-1PSIDTS=ts-new".to_owned()),
            ]
        );
        assert_eq!(
            session.take_persist().as_deref(),
            Some("SAPISID=s; __Secure-1PSIDTS=ts-new")
        );
    }

    #[test]
    fn anonymous_responses_never_touch_the_session() {
        let server = TestServer::start_http(|_| {
            json_reply(
                200,
                &["Set-Cookie: SAPISID=evil; Domain=.youtube.com"],
                "{}",
            )
        });
        let (it, session) = signed_in(&server.base, "SAPISID=s", None);
        let mut anon = it.clone();
        anon.auth = None;
        anon.request("browse", json!({}), &[], Login::IfAvailable)
            .unwrap();
        assert_eq!(session.cookie(), "SAPISID=s");
    }

    #[test]
    fn unauthorized_refreshes_and_retries_once() {
        let seen: Seen = Default::default();
        let log = seen.clone();
        let server = TestServer::start_http(move |req| match seen_cookie(req, &log).as_deref() {
            Some("SAPISID=fresh") => json_reply(200, &[], r#"{"ok":true}"#),
            _ => json_reply(
                401,
                &[],
                r#"{"error":{"code":401,"status":"UNAUTHENTICATED"}}"#,
            ),
        });
        let hooks = Refresher::new(Some("SAPISID=fresh"));
        let (it, session) = signed_in(&server.base, "SAPISID=stale", Some(hooks.clone()));
        let v = it
            .request("like/like", json!({}), &[], Login::Required)
            .unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(hooks.refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(
            *seen.lock().unwrap(),
            [
                Some("SAPISID=stale".to_owned()),
                Some("SAPISID=fresh".to_owned())
            ]
        );
        // The retry was signed with the new cookie's hash.
        assert!(!session.is_expired());
        assert!(hooks.expired.lock().unwrap().is_empty());
    }

    #[test]
    fn failed_refresh_expires_once_and_browsing_goes_anonymous() {
        let seen: Seen = Default::default();
        let log = seen.clone();
        let server = TestServer::start_http(move |req| match seen_cookie(req, &log) {
            Some(_) => json_reply(401, &[], "{}"),
            None => json_reply(200, &[], r#"{"anonymous":true}"#),
        });
        let hooks = Refresher::new(None);
        let (it, session) = signed_in(&server.base, "SAPISID=stale", Some(hooks.clone()));

        let err = it
            .request("like/like", json!({}), &[], Login::Required)
            .unwrap_err();
        assert!(err.is_auth(), "{err}");
        assert!(!err.is_network());
        assert!(!err.to_string().contains("stale"));
        assert!(session.is_expired());
        assert_eq!(hooks.refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "no retry after a failed refresh"
        );

        // Within ten minutes: no second refresh; still one expiry event.
        let err = it
            .request("like/like", json!({}), &[], Login::Required)
            .unwrap_err();
        assert!(err.is_auth());
        assert_eq!(hooks.refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(*hooks.expired.lock().unwrap(), [true]);

        // Pages that only may be signed in are fetched anonymously meanwhile.
        seen.lock().unwrap().clear();
        let v = it
            .request("browse", json!({}), &[], Login::IfAvailable)
            .unwrap();
        assert_eq!(v["anonymous"], true);
        assert_eq!(*seen.lock().unwrap(), [None]);
    }

    #[test]
    fn retry_that_fails_again_is_not_repeated() {
        let count = Arc::new(AtomicUsize::new(0));
        let n = count.clone();
        let server = TestServer::start_http(move |_| {
            n.fetch_add(1, Ordering::SeqCst);
            json_reply(401, &[], "{}")
        });
        let hooks = Refresher::new(Some("SAPISID=fresh-but-revoked"));
        let (it, session) = signed_in(&server.base, "SAPISID=stale", Some(hooks.clone()));
        let err = it
            .request("like/like", json!({}), &[], Login::Required)
            .unwrap_err();
        assert!(err.is_auth());
        assert_eq!(count.load(Ordering::SeqCst), 2, "the request and one retry");
        assert_eq!(hooks.refreshes.load(Ordering::SeqCst), 1);
        assert!(session.is_expired());
        assert_eq!(*hooks.expired.lock().unwrap(), [true]);
    }

    #[test]
    fn forbidden_is_auth_only_when_signed_out() {
        let server = TestServer::start_http(|req| {
            if req.path.contains("signed_out") {
                json_reply(
                    403,
                    &[],
                    r#"{"responseContext":{"serviceTrackingParams":[{"service":"GFEEDBACK","params":[{"key":"logged_in","value":"0"}]}]}}"#,
                )
            } else if req.path.contains("server") {
                Reply::Status(503)
            } else {
                json_reply(
                    403,
                    &[],
                    r#"{"error":{"code":403,"status":"PERMISSION_DENIED"}}"#,
                )
            }
        });
        let hooks = Refresher::new(None);
        let (it, _) = signed_in(&server.base, "SAPISID=s", Some(hooks.clone()));
        let err = it
            .request("playlist/denied", json!({}), &[], Login::Required)
            .unwrap_err();
        assert!(!err.is_auth() && !err.is_network(), "{err}");
        assert_eq!(hooks.refreshes.load(Ordering::SeqCst), 0);
        let err = it
            .request("server", json!({}), &[], Login::Required)
            .unwrap_err();
        assert!(err.is_network() && !err.is_auth(), "{err}");
        assert_eq!(hooks.refreshes.load(Ordering::SeqCst), 0);
        let err = it
            .request("signed_out", json!({}), &[], Login::Required)
            .unwrap_err();
        assert!(err.is_auth(), "{err}");
        assert_eq!(hooks.refreshes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn signed_out_classification() {
        assert!(signed_out_status(401, None));
        assert!(!signed_out_status(403, None));
        assert!(!signed_out_status(500, None));
        let unauth = json!({"error": {"code": 400, "status": "UNAUTHENTICATED"}});
        assert!(signed_out_status(400, Some(&unauth)));
        let logged_in = json!({"responseContext": {"serviceTrackingParams": [
            {"params": [{"key": "logged_in", "value": "1"}]}]}});
        assert!(!signed_out_status(403, Some(&logged_in)));
    }
}
