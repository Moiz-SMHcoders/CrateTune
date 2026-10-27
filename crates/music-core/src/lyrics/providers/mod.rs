//! Lyrics providers, ported from ArchiveTune's `lyrics/**` modules and
//! `app/.../lyrics/*Provider.kt`. Each one is blocking and stateless apart from
//! credentials; [`crate::lyrics::LyricsService`] runs them on worker threads.

mod betterlyrics;
mod binimum;
mod kugou;
mod lrclib;
mod megalobiz;
mod paxsenix;
mod simpmusic;
mod unison;
mod youlyplus;
mod youtube;

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

pub use betterlyrics::{BetterLyrics, BetterLyricsVariant};
pub use binimum::Binimum;
pub use kugou::KuGou;
pub use lrclib::LrcLib;
pub use megalobiz::Megalobiz;
pub use paxsenix::{Paxsenix, PaxsenixSource};
pub use simpmusic::SimpMusic;
pub use unison::Unison;
pub use youlyplus::YouLyPlus;
pub use youtube::YouTubeLyrics;

use super::types::{LyricsQuery, LyricsResult};
use crate::http::agent;
use crate::{Error, Result};

/// A lyrics source. Implementations are blocking and must be cheap to share
/// between threads.
pub trait LyricsProvider: Send + Sync {
    /// Display name; also stored as [`LyricsResult::provider`].
    fn name(&self) -> &str;

    /// `Ok(None)` when the provider has nothing for this track.
    fn fetch(&self, query: &LyricsQuery) -> Result<Option<LyricsResult>>;

    /// False when a required credential (API key) is missing; the service skips it.
    fn is_configured(&self) -> bool {
        true
    }
}

/// The built-in providers, for ordering/enabling in settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ProviderId {
    BetterLyrics,
    BetterLyricsPortato,
    YouLyPlus,
    Binimum,
    LrcLib,
    KuGou,
    Megalobiz,
    SimpMusic,
    Unison,
    PaxsenixAppleMusic,
    PaxsenixSpotify,
    PaxsenixMusixmatch,
    /// Paxsenix's generic `lrcget` endpoint (not in ArchiveTune's default list).
    Paxsenix,
    YouTube,
}

impl ProviderId {
    /// Every built-in provider.
    pub const ALL: [ProviderId; 14] = [
        Self::BetterLyrics,
        Self::BetterLyricsPortato,
        Self::YouLyPlus,
        Self::Binimum,
        Self::LrcLib,
        Self::KuGou,
        Self::Megalobiz,
        Self::SimpMusic,
        Self::Unison,
        Self::PaxsenixAppleMusic,
        Self::PaxsenixSpotify,
        Self::PaxsenixMusixmatch,
        Self::Paxsenix,
        Self::YouTube,
    ];

    /// ArchiveTune's default priority (`DefaultLyricsProviderOrder` + YouTube Music last).
    pub const DEFAULT_ORDER: [ProviderId; 13] = [
        Self::BetterLyrics,
        Self::BetterLyricsPortato,
        Self::YouLyPlus,
        Self::Binimum,
        Self::LrcLib,
        Self::KuGou,
        Self::Megalobiz,
        Self::SimpMusic,
        Self::Unison,
        Self::PaxsenixAppleMusic,
        Self::PaxsenixSpotify,
        Self::PaxsenixMusixmatch,
        Self::YouTube,
    ];

    /// Stable identifier for settings files (`"betterlyrics"`, `"lrclib"`, …).
    pub fn key(self) -> &'static str {
        match self {
            Self::BetterLyrics => "betterlyrics",
            Self::BetterLyricsPortato => "betterlyrics-portato",
            Self::YouLyPlus => "youlyplus",
            Self::Binimum => "binimum",
            Self::LrcLib => "lrclib",
            Self::KuGou => "kugou",
            Self::Megalobiz => "megalobiz",
            Self::SimpMusic => "simpmusic",
            Self::Unison => "unison",
            Self::PaxsenixAppleMusic => "paxsenix-applemusic",
            Self::PaxsenixSpotify => "paxsenix-spotify",
            Self::PaxsenixMusixmatch => "paxsenix-musixmatch",
            Self::Paxsenix => "paxsenix",
            Self::YouTube => "youtube",
        }
    }

    /// Display name (matches the provider's [`LyricsProvider::name`]).
    pub fn display_name(self) -> &'static str {
        match self {
            Self::BetterLyrics => "BetterLyrics",
            Self::BetterLyricsPortato => "BetterLyrics Portato",
            Self::YouLyPlus => "YouLyPlus",
            Self::Binimum => "Binimum Lyrics",
            Self::LrcLib => "LrcLib",
            Self::KuGou => "Kugou",
            Self::Megalobiz => "Megalobiz",
            Self::SimpMusic => "SimpMusic",
            Self::Unison => "Unison",
            Self::PaxsenixAppleMusic => "Paxsenix: Apple Music",
            Self::PaxsenixSpotify => "Paxsenix: Spotify",
            Self::PaxsenixMusixmatch => "Paxsenix: Musixmatch",
            Self::Paxsenix => "Paxsenix (Auto)",
            Self::YouTube => "YouTube Music",
        }
    }

    /// Needs an API key before it is used.
    pub fn requires_api_key(self) -> bool {
        matches!(
            self,
            Self::PaxsenixAppleMusic
                | Self::PaxsenixSpotify
                | Self::PaxsenixMusixmatch
                | Self::Paxsenix
        )
    }

    /// Builds the provider with the given credentials.
    pub fn create(self, keys: &ProviderKeys) -> Arc<dyn LyricsProvider> {
        let pax = |source| Arc::new(Paxsenix::new(source, keys.paxsenix.clone())) as Arc<_>;
        match self {
            Self::BetterLyrics => Arc::new(BetterLyrics::new(
                BetterLyricsVariant::Default,
                keys.betterlyrics.clone(),
            )),
            Self::BetterLyricsPortato => Arc::new(BetterLyrics::new(
                BetterLyricsVariant::Portato,
                keys.betterlyrics.clone(),
            )),
            Self::YouLyPlus => Arc::new(YouLyPlus::default()),
            Self::Binimum => Arc::new(Binimum),
            Self::LrcLib => Arc::new(LrcLib::default()),
            Self::KuGou => Arc::new(KuGou),
            Self::Megalobiz => Arc::new(Megalobiz),
            Self::SimpMusic => Arc::new(SimpMusic),
            Self::Unison => Arc::new(Unison),
            Self::PaxsenixAppleMusic => pax(PaxsenixSource::AppleMusic),
            Self::PaxsenixSpotify => pax(PaxsenixSource::Spotify),
            Self::PaxsenixMusixmatch => pax(PaxsenixSource::Musixmatch),
            Self::Paxsenix => pax(PaxsenixSource::Auto),
            Self::YouTube => Arc::new(YouTubeLyrics::default()),
        }
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

impl FromStr for ProviderId {
    type Err = Error;

    /// Accepts [`ProviderId::key`], the display name, or ArchiveTune's enum names
    /// (`BETTER_LYRICS`, `PAXSENIX_APPLE_MUSIC`, …).
    fn from_str(s: &str) -> Result<Self> {
        let norm = |v: &str| {
            v.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase()
        };
        let wanted = norm(s);
        Self::ALL
            .into_iter()
            .find(|id| norm(id.key()) == wanted || norm(id.display_name()) == wanted)
            .ok_or_else(|| Error::Parse(format!("unknown lyrics provider: {s}")))
    }
}

/// Optional credentials for providers that use them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderKeys {
    /// Paxsenix API key (`Authorization: Bearer …`); all Paxsenix sources need it.
    pub paxsenix: Option<String>,
    /// BetterLyrics API key (`X-API-Key`); without one only cached songs are served.
    pub betterlyrics: Option<String>,
}

/// A response with any status (providers decide what a 404 means).
pub(crate) struct HttpResponse {
    pub status: u16,
    pub body: String,
}

impl HttpResponse {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn json(&self) -> Result<serde_json::Value> {
        serde_json::from_str(&self.body).map_err(|e| Error::Parse(format!("json: {e}")))
    }
}

/// Lyrics bodies are small; cap them well below the audio limit.
const MAX_LYRICS_BODY: u64 = 8 * 1024 * 1024;
pub(crate) const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
pub(crate) const APP_UA: &str = "music-macos (https://github.com/)";

pub(crate) fn http_get(
    url: &str,
    params: &[(&str, &str)],
    headers: &[(&str, &str)],
    timeout: Option<Duration>,
) -> Result<HttpResponse> {
    let mut req = agent()
        .get(url)
        .config()
        .http_status_as_error(false)
        .timeout_global(timeout.or(Some(Duration::from_secs(15))))
        .build();
    for (k, v) in params {
        req = req.query(*k, *v);
    }
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    read(req.call()?)
}

pub(crate) fn http_post_json(
    url: &str,
    body: &serde_json::Value,
    headers: &[(&str, &str)],
    timeout: Option<Duration>,
) -> Result<HttpResponse> {
    let mut req = agent()
        .post(url)
        .config()
        .http_status_as_error(false)
        .timeout_global(timeout.or(Some(Duration::from_secs(15))))
        .build();
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    read(req.send_json(body)?)
}

pub(crate) fn http_post_form(
    url: &str,
    form: &[(&str, &str)],
    headers: &[(&str, &str)],
    timeout: Option<Duration>,
) -> Result<HttpResponse> {
    let mut req = agent()
        .post(url)
        .config()
        .http_status_as_error(false)
        .timeout_global(timeout.or(Some(Duration::from_secs(30))))
        .build();
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    read(req.send_form(form.iter().copied())?)
}

fn read(mut resp: ureq::http::Response<ureq::Body>) -> Result<HttpResponse> {
    let status = resp.status().as_u16();
    let body = resp
        .body_mut()
        .with_config()
        .limit(MAX_LYRICS_BODY)
        .read_to_string()?;
    Ok(HttpResponse { status, body })
}

/// Fails with a readable error for a non-2xx response.
pub(crate) fn expect_ok(resp: HttpResponse, what: &str) -> Result<HttpResponse> {
    if resp.ok() {
        Ok(resp)
    } else {
        Err(Error::Parse(format!("{what}: HTTP {}", resp.status)))
    }
}

pub(crate) fn require_title_artist(query: &LyricsQuery) -> Option<(&str, &str)> {
    let (t, a) = (query.title.trim(), query.artist.trim());
    (!t.is_empty() && !a.is_empty()).then_some((t, a))
}

/// Case-insensitive similarity in 0..=1 (ArchiveTune's LRCLIB scorer).
pub(crate) fn string_similarity(a: &str, b: &str) -> f64 {
    let (a, b) = (a.trim().to_lowercase(), b.trim().to_lowercase());
    if a == b {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a.contains(&b) || b.contains(&a) {
        return 0.8;
    }
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        prev = cur;
    }
    1.0 - prev[b.len()] as f64 / a.len().max(b.len()) as f64
}

#[cfg(test)]
pub(crate) fn fixture(name: &str) -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/lyrics/");
    std::fs::read_to_string(format!("{path}{name}"))
        .unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_ids_round_trip() {
        for id in ProviderId::ALL {
            assert_eq!(id.key().parse::<ProviderId>().unwrap(), id);
            assert_eq!(id.display_name().parse::<ProviderId>().unwrap(), id);
            assert_eq!(
                id.create(&ProviderKeys::default()).name(),
                id.display_name()
            );
        }
        assert_eq!(
            "PAXSENIX_APPLE_MUSIC".parse::<ProviderId>().unwrap(),
            ProviderId::PaxsenixAppleMusic
        );
        assert!("nope".parse::<ProviderId>().is_err());
        assert!(
            !ProviderId::PaxsenixSpotify
                .create(&ProviderKeys::default())
                .is_configured()
        );
    }

    #[test]
    fn similarity() {
        assert_eq!(string_similarity("Abc", "abc "), 1.0);
        assert_eq!(
            string_similarity("Instant Crush", "Instant Crush (feat. X)"),
            0.8
        );
        assert!(string_similarity("kitten", "sitting") > 0.5);
    }
}
