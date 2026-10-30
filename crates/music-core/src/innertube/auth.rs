// Ported from ArchiveTune core/src/main/kotlin/moe/rukamori/archivetune/innertube/{InnerTube.kt (ytClient), PlaybackAuthState.kt, utils/Utils.kt} — © Rukamori and ArchiveTune contributors, GPL-3.0
//! Cookie-based YouTube Music sign-in, as used by the web client.
//!
//! The user's browser cookies for `music.youtube.com` (at least `SAPISID` or
//! `__Secure-3PAPISID`, plus the session cookies) authenticate every request; the
//! `Authorization` header proves the request comes from that origin. Like YouTube's
//! own web client it carries up to three hashes:
//! `SAPISIDHASH <ts>_<h> SAPISID1PHASH <ts>_<h1> SAPISID3PHASH <ts>_<h3>`, where each
//! is `sha1("<ts> <secret> <origin>")` of `SAPISID`, `__Secure-1PAPISID` and
//! `__Secure-3PAPISID` (the 1P / 3P parts only when those cookies exist).
//!
//! With a [`Session`] attached ([`Auth::with_session`]) the cookie is the session's
//! current one (rotated by `Set-Cookie`, renewed by a silent re-login), shared by
//! every clone.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::session::Session;

/// Credentials for the account layer. Treat every field as a secret.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Auth {
    /// Raw `Cookie` header value copied from a signed-in `music.youtube.com` session
    /// (with a [`Auth::session`], the value the session started with; requests use
    /// [`Auth::cookie_header`]).
    pub cookie: String,
    /// `X-Goog-Visitor-Id` / `context.client.visitorData`.
    pub visitor_data: Option<String>,
    /// `DATASYNC_ID` (`<delegated>||<user>` for brand accounts); selects the channel.
    pub data_sync_id: Option<String>,
    /// `X-Goog-AuthUser`: index of the Google account in a multi-login browser session.
    pub auth_user: u32,
    /// The live session (current cookie, refresh, expiry); shared by clones.
    #[serde(skip)]
    pub session: Option<Arc<Session>>,
}

impl PartialEq for Auth {
    fn eq(&self, other: &Self) -> bool {
        self.cookie == other.cookie
            && self.visitor_data == other.visitor_data
            && self.data_sync_id == other.data_sync_id
            && self.auth_user == other.auth_user
            && match (&self.session, &other.session) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            }
    }
}

impl Eq for Auth {}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Auth")
            .field("cookie", &format_args!("<{} bytes>", self.cookie.len()))
            .field("has_login_cookie", &self.sapisid().is_some())
            .field("visitor_data", &self.visitor_data.as_ref().map(|_| "<set>"))
            .field("data_sync_id", &self.data_sync_id.as_ref().map(|_| "<set>"))
            .field("auth_user", &self.auth_user)
            .field("session", &self.session)
            .finish()
    }
}

/// Cookies that carry the SAPISID secret, in preference order.
const LOGIN_COOKIES: [&str; 3] = ["SAPISID", "__Secure-3PAPISID", "__Secure-1PAPISID"];
const SAPISID_1P: &str = "__Secure-1PAPISID";
const SAPISID_3P: &str = "__Secure-3PAPISID";

/// `name=value` pairs of a `Cookie` header.
fn cookie_pairs(cookie: &str) -> impl Iterator<Item = (&str, &str)> {
    cookie.split(';').filter_map(|part| {
        let (k, v) = part.split_once('=')?;
        let k = k.trim();
        (!k.is_empty()).then(|| (k, v.trim()))
    })
}

fn value_in<'a>(cookie: &'a str, name: &str) -> Option<&'a str> {
    cookie_pairs(cookie)
        .find(|(k, v)| *k == name && !v.is_empty())
        .map(|(_, v)| v)
}

fn sapisid_in(cookie: &str) -> Option<&str> {
    LOGIN_COOKIES.iter().find_map(|name| value_in(cookie, name))
}

/// The `Authorization` value for a `Cookie` header (see the module docs).
pub(crate) fn authorization_for(cookie: &str, origin: &str, timestamp: u64) -> Option<String> {
    let mut value = sapisid_hash(sapisid_in(cookie)?, origin, timestamp);
    for (name, label) in [(SAPISID_1P, "SAPISID1PHASH"), (SAPISID_3P, "SAPISID3PHASH")] {
        if let Some(secret) = value_in(cookie, name) {
            value.push(' ');
            value.push_str(&hash_part(label, secret, origin, timestamp));
        }
    }
    Some(value)
}

impl Auth {
    pub fn new(cookie: impl Into<String>) -> Self {
        Self {
            cookie: cookie.into(),
            ..Self::default()
        }
    }

    /// Signs with `session`'s current cookie from now on (shared by clones).
    pub fn with_session(mut self, session: Arc<Session>) -> Self {
        self.session = Some(session);
        self
    }

    /// `name=value` pairs of [`Auth::cookie`].
    pub fn cookies(&self) -> impl Iterator<Item = (&str, &str)> {
        cookie_pairs(&self.cookie)
    }

    pub fn cookie_value(&self, name: &str) -> Option<&str> {
        value_in(&self.cookie, name)
    }

    /// `SAPISID`, falling back to `__Secure-3PAPISID` / `__Secure-1PAPISID`.
    pub fn sapisid(&self) -> Option<&str> {
        sapisid_in(&self.cookie)
    }

    /// True when the cookie can sign requests (a SAPISID-type cookie is present).
    pub fn is_signed_in(&self) -> bool {
        self.sapisid().is_some()
    }

    /// The `Cookie` header to send: the session's current one, else [`Auth::cookie`].
    pub fn cookie_header(&self) -> String {
        match &self.session {
            Some(s) => s.cookie(),
            None => self.cookie.clone(),
        }
    }

    /// The cookie to sign with and the session revision it belongs to.
    pub(crate) fn signing_cookie(&self) -> (String, Option<u64>) {
        match &self.session {
            Some(s) => {
                let (cookie, rev) = s.snapshot();
                (cookie, Some(rev))
            }
            None => (self.cookie.clone(), None),
        }
    }

    /// Whether the attached session is known to be signed out.
    pub fn is_expired(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.is_expired())
    }

    /// `Authorization` header value for `origin` at unix time `timestamp`, from the
    /// current cookie.
    pub fn authorization(&self, origin: &str, timestamp: u64) -> Option<String> {
        authorization_for(&self.cookie_header(), origin, timestamp)
    }

    /// Merges a response's `Set-Cookie` lines into the session (no-op without one,
    /// or for untrusted origins). `loopback_ok`: `origin` is a test stand-in for
    /// music.youtube.com.
    pub(crate) fn absorb_set_cookies<'a>(
        &self,
        origin: &str,
        loopback_ok: bool,
        lines: impl IntoIterator<Item = &'a str>,
    ) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        let updates = super::cookies::applicable(origin, loopback_ok, lines, unix_now());
        session.absorb(&updates)
    }

    /// The delegated (brand account) session id: the part of `data_sync_id` before `||`.
    pub fn delegated_session_id(&self) -> Option<&str> {
        let id = self.data_sync_id.as_deref()?.trim();
        let (delegated, user) = id.split_once("||")?;
        let delegated = delegated.trim();
        (!delegated.is_empty() && !user.trim().is_empty()).then_some(delegated)
    }

    /// Normalized `data_sync_id`: trimmed, `%7C%7C` decoded, empty halves dropped.
    pub fn normalized_data_sync_id(&self) -> Option<String> {
        normalize_data_sync_id(self.data_sync_id.as_deref()?)
    }

    /// Request headers for an authenticated call from `origin` (at `timestamp`).
    pub(crate) fn headers(&self, origin: &str, timestamp: u64) -> Vec<(&'static str, String)> {
        self.headers_with(&self.cookie_header(), origin, timestamp)
    }

    /// [`Auth::headers`] signing with `cookie` (from [`Auth::signing_cookie`]).
    pub(crate) fn headers_with(
        &self,
        cookie: &str,
        origin: &str,
        timestamp: u64,
    ) -> Vec<(&'static str, String)> {
        let mut out = vec![("Cookie", cookie.to_owned())];
        if let Some(authz) = authorization_for(cookie, origin, timestamp) {
            out.push(("Authorization", authz));
            out.push(("X-Goog-AuthUser", self.auth_user.to_string()));
            if let Some(page) = self.delegated_session_id() {
                out.push(("X-Goog-PageId", page.to_owned()));
            }
        }
        out
    }
}

/// Trims a `DATASYNC_ID`, decodes `%7C`, and drops an empty half of `<delegated>||<user>`.
pub(crate) fn normalize_data_sync_id(raw: &str) -> Option<String> {
    let raw = raw.trim().replace("%7C", "|").replace("%7c", "|");
    if raw.is_empty() || raw.eq_ignore_ascii_case("null") {
        return None;
    }
    Some(match raw.split_once("||") {
        None => raw,
        Some((d, u)) => match (d.trim(), u.trim()) {
            ("", "") => return None,
            ("", u) => u.to_owned(),
            (d, "") => d.to_owned(),
            (d, u) => format!("{d}||{u}"),
        },
    })
}

/// `SAPISIDHASH <ts>_<sha1("<ts> <sapisid> <origin>")>`.
pub fn sapisid_hash(sapisid: &str, origin: &str, timestamp: u64) -> String {
    hash_part("SAPISIDHASH", sapisid, origin, timestamp)
}

/// `<label> <ts>_<sha1("<ts> <secret> <origin>")>`.
fn hash_part(label: &str, secret: &str, origin: &str, timestamp: u64) -> String {
    let digest = sha1_smol::Sha1::from(format!("{timestamp} {secret} {origin}"))
        .digest()
        .to_string();
    format!("{label} {timestamp}_{digest}")
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sapisidhash_matches_reference() {
        // sha1("1700000000 abc123/XYZ https://music.youtube.com")
        let expected = sha1_smol::Sha1::from("1700000000 abc123/XYZ https://music.youtube.com")
            .digest()
            .to_string();
        assert_eq!(
            sapisid_hash("abc123/XYZ", "https://music.youtube.com", 1_700_000_000),
            format!("SAPISIDHASH 1700000000_{expected}")
        );
        // Known vector computed independently (python hashlib.sha1).
        assert_eq!(
            sapisid_hash("SAPISIDVALUE", "https://music.youtube.com", 1234567890),
            "SAPISIDHASH 1234567890_265bac307c2b60c66d11fab82fc553709a898863"
        );
    }

    #[test]
    fn three_part_authorization() {
        // Vectors computed by hand: printf '%s' "<ts> <secret> <origin>" | shasum -a 1
        const ORIGIN: &str = "https://music.youtube.com";
        const TS: u64 = 1_700_000_000;
        const H: &str = "7ef7126262ab644136fc1ceef792ef2dac969ed5"; // SAPISIDVALUE
        const H1: &str = "832eb9e8158b164c5c5b4d1134d43959c9f463fd"; // 1PVALUE
        const H3: &str = "40ef720d494e87bbdefbe7b4654dc72a0fecfb64"; // 3PVALUE
        let all = Auth::new(
            "SID=x; __Secure-3PAPISID=3PVALUE; SAPISID=SAPISIDVALUE; __Secure-1PAPISID=1PVALUE",
        );
        assert_eq!(
            all.authorization(ORIGIN, TS).unwrap(),
            format!("SAPISIDHASH {TS}_{H} SAPISID1PHASH {TS}_{H1} SAPISID3PHASH {TS}_{H3}")
        );
        // Only SAPISID: the plain form.
        assert_eq!(
            Auth::new("SAPISID=SAPISIDVALUE; SID=x")
                .authorization(ORIGIN, TS)
                .unwrap(),
            format!("SAPISIDHASH {TS}_{H}")
        );
        // Only the 3P cookie: it stands in for SAPISID and adds its own part.
        assert_eq!(
            Auth::new("__Secure-3PAPISID=3PVALUE")
                .authorization(ORIGIN, TS)
                .unwrap(),
            format!("SAPISIDHASH {TS}_{H3} SAPISID3PHASH {TS}_{H3}")
        );
        // An empty 1P cookie is left out.
        assert_eq!(
            Auth::new("SAPISID=SAPISIDVALUE; __Secure-1PAPISID=")
                .authorization(ORIGIN, TS)
                .unwrap(),
            format!("SAPISIDHASH {TS}_{H}")
        );
        assert_eq!(Auth::new("SID=x").authorization(ORIGIN, TS), None);
    }

    #[test]
    fn session_cookie_is_shared_by_clones() {
        let session = Session::new("SAPISID=old; X=1");
        let a = Auth::new("SAPISID=old; X=1").with_session(session.clone());
        let b = a.clone();
        assert_eq!(a, b);
        assert_ne!(a, Auth::new("SAPISID=old; X=1"), "another session");
        assert!(a.absorb_set_cookies(
            "https://music.youtube.com",
            false,
            ["X=2; Domain=.youtube.com; Path=/"]
        ));
        assert_eq!(b.cookie_header(), "SAPISID=old; X=2");
        assert_eq!(
            b.headers("https://music.youtube.com", 1)[0].1,
            "SAPISID=old; X=2"
        );
        // Untrusted origins change nothing.
        assert!(!a.absorb_set_cookies("https://evil.example", false, ["X=3"]));
        assert!(!a.absorb_set_cookies("http://127.0.0.1:1", false, ["X=3"]));
        assert!(a.absorb_set_cookies("http://127.0.0.1:1", true, ["X=3"]));
        session.replace_cookie("SAPISID=new");
        assert_eq!(
            b.authorization("https://music.youtube.com", 1),
            Some(sapisid_hash("new", "https://music.youtube.com", 1))
        );
        // Without a session nothing is merged.
        assert!(!Auth::new("SAPISID=s").absorb_set_cookies(
            "https://music.youtube.com",
            false,
            ["X=3"]
        ));
        assert!(!format!("{b:?}").contains("SAPISID"));
    }

    #[test]
    fn cookie_parsing_and_fallback() {
        let a = Auth::new("PREF=f6=8; __Secure-3PAPISID=three; LOGIN_INFO=x=y=z; HSID=h");
        assert_eq!(a.sapisid(), Some("three"));
        assert_eq!(a.cookie_value("LOGIN_INFO"), Some("x=y=z"));
        let b = Auth::new("SAPISID=one; __Secure-3PAPISID=three");
        assert_eq!(b.sapisid(), Some("one"));
        let c = Auth::new("SAPISID=; HSID=h");
        assert_eq!(c.sapisid(), None);
        assert!(!c.is_signed_in());
    }

    #[test]
    fn data_sync_id_and_headers() {
        let mut a = Auth::new("SAPISID=s");
        a.auth_user = 2;
        a.data_sync_id = Some("1234%7C%7C5678".into());
        assert_eq!(a.normalized_data_sync_id().as_deref(), Some("1234||5678"));
        a.data_sync_id = Some("1234||5678".into());
        assert_eq!(a.delegated_session_id(), Some("1234"));
        let h = a.headers("https://music.youtube.com", 1);
        let get = |k: &str| h.iter().find(|(n, _)| *n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("Cookie"), Some("SAPISID=s"));
        assert!(get("Authorization").unwrap().starts_with("SAPISIDHASH 1_"));
        assert_eq!(get("X-Goog-AuthUser"), Some("2"));
        assert_eq!(get("X-Goog-PageId"), Some("1234"));
        a.data_sync_id = Some("||5678".into());
        assert_eq!(a.delegated_session_id(), None);
        assert_eq!(a.normalized_data_sync_id().as_deref(), Some("5678"));
        assert!(!format!("{a:?}").contains("SAPISID=s"));
    }
}
