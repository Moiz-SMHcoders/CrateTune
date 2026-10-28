//! The browser-like state the web/TV player requests need: a visitor id and the
//! current player script (for its `signatureTimestamp` and the challenge solver).
//!
//! Both come from a watch page, as yt-dlp does: YouTube hands out the long
//! `VISITOR_DATA` there, and the page names the player script its stream URLs are
//! built for. They are cached for a while and shared by every video.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use regex::Regex;

use crate::http::{agent, read_body};
use crate::{Error, Result};

/// Desktop browser identity used for page and player-script fetches and for
/// googlevideo downloads (yt-dlp does the same).
pub(crate) const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36";
/// Consent + language cookies, so the page isn't an EU consent interstitial.
const PAGE_COOKIES: &str = "PREF=hl=en&tz=UTC; SOCS=CAI";
/// Refetch the page (new visitor id, maybe a new player) after this long.
const SESSION_TTL: Duration = Duration::from_secs(45 * 60);

/// A visitor id plus the player script it goes with.
#[derive(Clone)]
pub(crate) struct WebSession {
    pub visitor_data: String,
    pub player_id: String,
    /// `signatureTimestamp` of the player script.
    pub sts: u32,
    /// The full script, if it had to be downloaded (no preprocessed copy cached).
    pub script: Option<Arc<str>>,
    fetched: Instant,
}

/// Caches the [`WebSession`] and the downloaded player scripts.
pub(crate) struct SessionCache {
    current: Mutex<Option<WebSession>>,
    /// Directory holding `player-<id>.sts` next to the solver's preprocessed players.
    dir: Option<std::path::PathBuf>,
}

impl SessionCache {
    pub(crate) fn new(dir: Option<std::path::PathBuf>) -> Self {
        Self {
            current: Mutex::new(None),
            dir,
        }
    }

    /// The cached session, or a fresh one built from `video_id`'s watch page.
    pub(crate) fn get(&self, video_id: &str) -> Result<WebSession> {
        if let Some(s) = self
            .current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .filter(|s| s.fetched.elapsed() < SESSION_TTL)
        {
            return Ok(s.clone());
        }
        let session = self.fetch(video_id)?;
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = Some(session.clone());
        Ok(session)
    }

    /// Forget the session (e.g. after YouTube rejected it).
    pub(crate) fn invalidate(&self) {
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// The player script for `session`, downloading it if it isn't attached.
    pub(crate) fn script(&self, session: &mut WebSession) -> Result<Arc<str>> {
        if let Some(script) = &session.script {
            return Ok(script.clone());
        }
        let script: Arc<str> = download_player(&session.player_id)?.into();
        session.script = Some(script.clone());
        if let Some(cur) = self
            .current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
            .filter(|c| c.player_id == session.player_id)
        {
            cur.script = Some(script.clone());
        }
        Ok(script)
    }

    fn fetch(&self, video_id: &str) -> Result<WebSession> {
        let url =
            format!("https://www.youtube.com/watch?v={video_id}&bpctr=9999999999&has_verified=1");
        let mut resp = agent()
            .get(&url)
            .header("User-Agent", BROWSER_UA)
            .header(
                "Accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .header("Accept-Language", "en-us,en;q=0.5")
            .header("Cookie", PAGE_COOKIES)
            .call()?;
        let page = String::from_utf8_lossy(&read_body(&mut resp)?).into_owned();
        let (visitor_data, player_id) = parse_watch_page(&page)?;

        let sts_file = self
            .dir
            .as_ref()
            .map(|d| d.join(format!("player-{}.sts", sanitize(&player_id))));
        let cached_sts = sts_file
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| s.trim().parse().ok());
        let (sts, script) = match cached_sts {
            Some(sts) => (sts, None),
            None => {
                let script = download_player(&player_id)?;
                let sts = signature_timestamp(&script)
                    .ok_or_else(|| Error::Parse("player script: no signatureTimestamp".into()))?;
                if let (Some(path), Some(dir)) = (&sts_file, &self.dir) {
                    let _ = std::fs::create_dir_all(dir);
                    let _ = std::fs::write(path, sts.to_string());
                }
                (sts, Some(Arc::from(script)))
            }
        };
        log::info!("youtube: web session with player {player_id} (sts {sts})");
        Ok(WebSession {
            visitor_data,
            player_id,
            sts,
            script,
            fetched: Instant::now(),
        })
    }
}

fn sanitize(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect()
}

/// `player_ias` ("main" variant), the one yt-dlp's solver is tested against.
fn download_player(player_id: &str) -> Result<String> {
    let url = format!(
        "https://www.youtube.com/s/player/{}/player_ias.vflset/en_US/base.js",
        sanitize(player_id)
    );
    let mut resp = agent().get(&url).header("User-Agent", BROWSER_UA).call()?;
    let script = String::from_utf8_lossy(&read_body(&mut resp)?).into_owned();
    if script.len() < 100_000 {
        return Err(Error::Parse(format!(
            "player script {player_id} is suspiciously small ({} bytes)",
            script.len()
        )));
    }
    Ok(script)
}

/// `VISITOR_DATA` and the player id from a watch page.
pub(crate) fn parse_watch_page(page: &str) -> Result<(String, String)> {
    static VISITOR: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static PLAYER: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let visitor = VISITOR.get_or_init(|| Regex::new(r#""VISITOR_DATA"\s*:\s*"([^"]+)""#).unwrap());
    let player = PLAYER.get_or_init(|| {
        Regex::new(
            r#""(?:PLAYER_JS_URL|jsUrl)"\s*:\s*"[^"]*/s/player/([A-Za-z0-9._-]+)/[^"]*base\.js""#,
        )
        .unwrap()
    });
    let visitor_data = visitor
        .captures(page)
        .map(|c| c[1].to_owned())
        .ok_or_else(|| Error::Parse("watch page: no VISITOR_DATA".into()))?;
    let player_id = player
        .captures(page)
        .map(|c| c[1].to_owned())
        .ok_or_else(|| Error::Parse("watch page: no player script".into()))?;
    Ok((visitor_data, player_id))
}

/// The player's `signatureTimestamp` (sent so stream URLs match its challenges).
pub(crate) fn signature_timestamp(script: &str) -> Option<u32> {
    static STS: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    STS.get_or_init(|| Regex::new(r"(?:signatureTimestamp|sts)\s*:\s*([0-9]{5})").unwrap())
        .captures(script)
        .and_then(|c| c[1].parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_page_fields() {
        let page = r#"<script>ytcfg.set({"VISITOR_DATA":"CgtABC%3D%3D","PLAYER_JS_URL":"/s/player/950316ef/player_ias.vflset/en_US/base.js","X":1});</script>"#;
        let (visitor, player) = parse_watch_page(page).unwrap();
        assert_eq!(visitor, "CgtABC%3D%3D");
        assert_eq!(player, "950316ef");
        let alt = r#"{"jsUrl":"/s/player/6b2fce74/player_ias.vflset/en_US/base.js","VISITOR_DATA" : "Cgs"}"#;
        assert_eq!(
            parse_watch_page(alt).unwrap(),
            ("Cgs".into(), "6b2fce74".into())
        );
        assert!(parse_watch_page("<html></html>").is_err());
    }

    #[test]
    fn sts() {
        assert_eq!(
            signature_timestamp("a={signatureTimestamp:20751,b:1}"),
            Some(20751)
        );
        assert_eq!(signature_timestamp("x;sts:19876;y"), Some(19876));
        assert_eq!(signature_timestamp("nothing"), None);
    }
}
