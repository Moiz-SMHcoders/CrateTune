// Client identities partly from refryt (crates/refryt-resolve/src/guest.rs, VISIONOS) and
// Limusic (crates/innertube/clients.json, TVHTML5_SIMPLY) — © their contributors, GPL-3.0
//! The InnerTube clients [`super::WebResolver`] asks for streams, fastest first.
//!
//! | client | needs | cost |
//! |---|---|---|
//! | [`WebClient::VisionOs`] | a visitor id | one `player` request (~0.4 s): direct URLs, no JS, no ad wait |
//! | [`WebClient::Tv`] | the player script (`sts`, `n`/`s` challenges) | + the challenge solver, + the pre-roll ad wait |
//! | [`WebClient::TvSimply`] | the above + a PoToken | + BotGuard |
//! | [`WebClient::MobileWeb`] | the above + a PoToken | + BotGuard |
//!
//! The identity strings (versions, user agents, devices) can be replaced by the
//! signed player-fixes manifest ([`super::fixes`]) without an app update.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};

use serde_json::{Map, Value, json};

/// Which InnerTube client asks for the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum WebClient {
    /// Apple Vision Pro client: direct URLs, no JS challenges, no PoToken, no ad
    /// wait. Needs a visitor id (YouTube hands one out with its first refusal).
    VisionOs,
    /// Samsung Tizen TV client: no PoToken, JS challenges only.
    Tv,
    /// The "simply" TV client: JS challenges plus a PoToken.
    TvSimply,
    /// Mobile web: JS challenges plus a PoToken (needs a [`super::PoTokenProvider`]).
    MobileWeb,
}

impl WebClient {
    /// Every client, fastest first: the order Automatic uses.
    pub const ALL: [WebClient; 4] = [
        WebClient::VisionOs,
        WebClient::Tv,
        WebClient::TvSimply,
        WebClient::MobileWeb,
    ];

    /// Automatic's clients, fastest first (those needing a PoToken only with a provider).
    pub fn automatic(has_potoken: bool) -> Vec<WebClient> {
        Self::ALL
            .into_iter()
            .filter(|c| has_potoken || !c.needs_potoken())
            .collect()
    }

    pub fn label(self) -> &'static str {
        match self {
            WebClient::VisionOs => "visionOS",
            WebClient::Tv => "TV",
            WebClient::TvSimply => "TV Simply + PoToken",
            WebClient::MobileWeb => "Web + PoToken",
        }
    }

    /// The `clientName` InnerTube knows it by; also its key in the player-fixes manifest.
    pub fn key(self) -> &'static str {
        match self {
            WebClient::VisionOs => "VISIONOS",
            WebClient::Tv => "TVHTML5",
            WebClient::TvSimply => "TVHTML5_SIMPLY",
            WebClient::MobileWeb => "MWEB",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.key() == key)
    }

    /// Needs a PoToken (BotGuard) for its player request and stream URLs.
    pub fn needs_potoken(self) -> bool {
        matches!(self, WebClient::TvSimply | WebClient::MobileWeb)
    }

    /// Needs the player script: its `signatureTimestamp` in the request and its
    /// functions for the URLs' `n` / `s` challenges. Such URLs also wait out the
    /// pre-roll ad.
    pub fn needs_player(self) -> bool {
        !matches!(self, WebClient::VisionOs)
    }

    pub(crate) fn client_id(self) -> &'static str {
        match self {
            WebClient::VisionOs => "101",
            WebClient::Tv => "7",
            WebClient::TvSimply => "75",
            WebClient::MobileWeb => "2",
        }
    }

    pub(crate) fn origin(self) -> &'static str {
        match self {
            WebClient::MobileWeb => "https://m.youtube.com",
            _ => "https://www.youtube.com",
        }
    }

    /// The built-in identity (`context.client` without `hl` & co.).
    fn builtin(self) -> Identity {
        let fields: &[(&str, &str)] = match self {
            WebClient::VisionOs => &[
                ("clientVersion", "1.02"),
                ("userAgent", VISIONOS_UA),
                ("deviceMake", "Apple"),
                ("deviceModel", "RealityDevice17,1"),
                ("osName", "visionOS"),
                ("osVersion", "26.5.23O471"),
            ],
            WebClient::Tv => &[
                ("clientVersion", "5.20260707"),
                ("userAgent", TV_UA),
                ("deviceMake", "Samsung"),
                ("deviceModel", "UKS9800"),
                ("osName", "Tizen"),
                ("osVersion", "2.4.0"),
            ],
            WebClient::TvSimply => &[("clientVersion", "1.0"), ("userAgent", COBALT_UA)],
            WebClient::MobileWeb => &[
                ("clientVersion", "2.20260708.05.00"),
                ("userAgent", MWEB_UA),
            ],
        };
        Identity {
            fields: fields
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }

    /// The identity to send: the built-in one with the manifest's overrides.
    pub(crate) fn identity(self, overrides: &ClientOverrides) -> Identity {
        let mut identity = self.builtin();
        if let Some(fields) = overrides.0.get(&self) {
            for (k, v) in fields {
                identity.fields.insert(k.clone(), v.clone());
            }
        }
        identity
    }
}

/// `context.client` fields of one client (strings only).
#[derive(Debug, Clone)]
pub(crate) struct Identity {
    fields: HashMap<String, String>,
}

impl Identity {
    pub(crate) fn version(&self) -> &str {
        self.fields
            .get("clientVersion")
            .map(String::as_str)
            .unwrap_or_default()
    }

    /// The user agent, as a `'static` str (see [`intern`]).
    pub(crate) fn user_agent(&self) -> &'static str {
        intern(
            self.fields
                .get("userAgent")
                .map(String::as_str)
                .unwrap_or_default(),
        )
    }

    /// `context.client` for `client`.
    pub(crate) fn context(&self, client: WebClient) -> Value {
        let mut ctx = Map::new();
        ctx.insert("clientName".into(), json!(client.key()));
        for (k, v) in &self.fields {
            ctx.insert(k.clone(), json!(v));
        }
        Value::Object(ctx)
    }
}

/// Context fields the manifest may replace, per client (anything else is ignored).
pub(crate) const OVERRIDABLE: [&str; 6] = [
    "clientVersion",
    "userAgent",
    "deviceMake",
    "deviceModel",
    "osName",
    "osVersion",
];

/// Identity fields replaced by the player-fixes manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClientOverrides(pub(crate) HashMap<WebClient, HashMap<String, String>>);

/// A `'static` copy of `s`. User agents end up in [`crate::ResolvedStream::user_agent`];
/// the few distinct ones a manifest can introduce are kept for the process.
fn intern(s: &str) -> &'static str {
    static KNOWN: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    for builtin in [VISIONOS_UA, TV_UA, COBALT_UA, MWEB_UA] {
        if s == builtin {
            return builtin;
        }
    }
    let mut known = KNOWN
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some(s) = known.get(s) {
        return s;
    }
    // Bounded: a manifest has at most a few clients, and it changes rarely.
    if known.len() >= 64 {
        return VISIONOS_UA;
    }
    let leaked: &'static str = Box::leak(s.to_owned().into_boxed_str());
    known.insert(s.to_owned(), leaked);
    leaked
}

pub(crate) const VISIONOS_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";
const TV_UA: &str = "Mozilla/5.0 (SMART-TV; Linux; Tizen 2.4.0) AppleWebKit/538.1 (KHTML, like Gecko) Version/2.4.0 TV Safari/538.1";
const COBALT_UA: &str = "Mozilla/5.0 (ChromiumStylePlatform) Cobalt/25.lts.30.1034943-gold (unlike Gecko), Unknown_TV_Unknown_0/Unknown (Unknown, Unknown)";
const MWEB_UA: &str = "Mozilla/5.0 (iPad; CPU OS 16_7_10 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/16.6 Mobile/15E148 Safari/604.1,gzip(gfe)";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_order_is_fastest_first() {
        assert_eq!(
            WebClient::automatic(true),
            [
                WebClient::VisionOs,
                WebClient::Tv,
                WebClient::TvSimply,
                WebClient::MobileWeb
            ]
        );
        // Without a PoToken provider only the clients that need none.
        assert_eq!(
            WebClient::automatic(false),
            [WebClient::VisionOs, WebClient::Tv]
        );
        for c in WebClient::ALL {
            assert_eq!(WebClient::from_key(c.key()), Some(c));
        }
    }

    #[test]
    fn overrides_replace_identity_fields() {
        let builtin = WebClient::VisionOs.identity(&ClientOverrides::default());
        assert_eq!(builtin.version(), "1.02");
        assert_eq!(builtin.user_agent(), VISIONOS_UA);
        let ctx = builtin.context(WebClient::VisionOs);
        assert_eq!(ctx["clientName"], "VISIONOS");
        assert_eq!(ctx["osName"], "visionOS");

        let mut o = ClientOverrides::default();
        o.0.insert(
            WebClient::VisionOs,
            HashMap::from([
                ("clientVersion".to_owned(), "1.03".to_owned()),
                ("userAgent".to_owned(), "Test/1.0".to_owned()),
            ]),
        );
        let id = WebClient::VisionOs.identity(&o);
        assert_eq!(id.version(), "1.03");
        assert_eq!(id.user_agent(), "Test/1.0");
        // Interned once.
        assert!(std::ptr::eq(id.user_agent(), id.user_agent()));
        assert_eq!(id.context(WebClient::VisionOs)["deviceMake"], "Apple");
        // Other clients keep theirs.
        assert_eq!(WebClient::Tv.identity(&o).version(), "5.20260707");
    }
}
