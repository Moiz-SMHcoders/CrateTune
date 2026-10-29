// Ported from ArchiveTune core/src/main/kotlin/moe/rukamori/archivetune/innertube/{InnerTube.kt, models/Context.kt, models/YouTubeClient.kt} — © Rukamori and ArchiveTune contributors, GPL-3.0
//! Request plumbing: client context, headers, auth, continuations.

use std::sync::{Mutex, PoisonError};

use serde_json::{Value, json};

use super::InnerTube;
use super::auth::unix_now;
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

impl InnerTube {
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
        if login && let Some(auth) = &self.auth {
            h.extend(auth.headers(ORIGIN, unix_now()));
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
    pub(crate) fn request(
        &self,
        endpoint: &str,
        extra: Value,
        query: &[(&str, &str)],
        login: Login,
    ) -> Result<Value> {
        let use_login = match login {
            Login::IfAvailable => self.signed_in(),
            Login::Required if self.signed_in() => true,
            Login::Required => {
                return Err(Error::Parse(format!(
                    "{endpoint}: needs a signed-in account (set InnerTube::auth with YouTube Music cookies)"
                )));
            }
        };
        let body = self.body(extra, use_login);
        let origin = self.api_base.as_deref().unwrap_or(ORIGIN);
        let mut req = agent()
            .post(format!("{origin}{API_PATH}/{endpoint}"))
            .query("prettyPrint", "false");
        for (k, v) in query {
            req = req.query(*k, *v);
        }
        for (k, v) in self.headers(use_login) {
            req = req.header(k, v);
        }
        let mut resp = req.send_json(&body)?;
        let value: Value = resp.body_mut().read_json()?;
        if let Some(err) = value.get("error") {
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
    fn required_login_fails_fast_without_cookie() {
        let err = InnerTube::default()
            .request("account/account_menu", json!({}), &[], Login::Required)
            .unwrap_err();
        assert!(err.to_string().contains("signed-in"));
    }
}
