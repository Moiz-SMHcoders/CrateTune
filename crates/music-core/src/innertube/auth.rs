// Ported from ArchiveTune core/src/main/kotlin/moe/rukamori/archivetune/innertube/{InnerTube.kt (ytClient), PlaybackAuthState.kt, utils/Utils.kt} — © Rukamori and ArchiveTune contributors, GPL-3.0
//! Cookie-based YouTube Music sign-in, as used by the web client.
//!
//! The user's browser cookies for `music.youtube.com` (at least `SAPISID` or
//! `__Secure-3PAPISID`, plus the session cookies) authenticate every request; the
//! `Authorization: SAPISIDHASH …` header proves the request comes from that origin.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Credentials for the account layer. Treat every field as a secret.
#[derive(Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Auth {
    /// Raw `Cookie` header value copied from a signed-in `music.youtube.com` session.
    pub cookie: String,
    /// `X-Goog-Visitor-Id` / `context.client.visitorData`.
    pub visitor_data: Option<String>,
    /// `DATASYNC_ID` (`<delegated>||<user>` for brand accounts); selects the channel.
    pub data_sync_id: Option<String>,
    /// `X-Goog-AuthUser`: index of the Google account in a multi-login browser session.
    pub auth_user: u32,
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Auth")
            .field("cookie", &format_args!("<{} bytes>", self.cookie.len()))
            .field("has_login_cookie", &self.sapisid().is_some())
            .field("visitor_data", &self.visitor_data.as_ref().map(|_| "<set>"))
            .field("data_sync_id", &self.data_sync_id.as_ref().map(|_| "<set>"))
            .field("auth_user", &self.auth_user)
            .finish()
    }
}

/// Cookies that carry the SAPISID secret, in preference order.
const LOGIN_COOKIES: [&str; 3] = ["SAPISID", "__Secure-3PAPISID", "__Secure-1PAPISID"];

impl Auth {
    pub fn new(cookie: impl Into<String>) -> Self {
        Self {
            cookie: cookie.into(),
            ..Self::default()
        }
    }

    /// `name=value` pairs of the cookie header.
    pub fn cookies(&self) -> impl Iterator<Item = (&str, &str)> {
        self.cookie.split(';').filter_map(|part| {
            let (k, v) = part.split_once('=')?;
            let k = k.trim();
            (!k.is_empty()).then(|| (k, v.trim()))
        })
    }

    pub fn cookie_value(&self, name: &str) -> Option<&str> {
        self.cookies()
            .find(|(k, v)| *k == name && !v.is_empty())
            .map(|(_, v)| v)
    }

    /// `SAPISID`, falling back to `__Secure-3PAPISID` / `__Secure-1PAPISID`.
    pub fn sapisid(&self) -> Option<&str> {
        LOGIN_COOKIES
            .iter()
            .find_map(|name| self.cookie_value(name))
    }

    /// True when the cookie can sign requests (a SAPISID-type cookie is present).
    pub fn is_signed_in(&self) -> bool {
        self.sapisid().is_some()
    }

    /// `Authorization` header value for `origin` at unix time `timestamp`.
    pub fn authorization(&self, origin: &str, timestamp: u64) -> Option<String> {
        Some(sapisid_hash(self.sapisid()?, origin, timestamp))
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
        let mut out = vec![("Cookie", self.cookie.clone())];
        if let Some(authz) = self.authorization(origin, timestamp) {
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
    let digest = sha1_smol::Sha1::from(format!("{timestamp} {sapisid} {origin}"))
        .digest()
        .to_string();
    format!("SAPISIDHASH {timestamp}_{digest}")
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
