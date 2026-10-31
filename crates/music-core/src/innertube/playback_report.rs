// Ported from ArchiveTune core/src/main/kotlin/moe/rukamori/archivetune/innertube/{InnerTube.kt (registerPlayback, ytPlaybackTrackingClient), YouTube.kt (registerPlayback)} — © Rukamori and ArchiveTune contributors, GPL-3.0
// Watch-time pings after Zuno src/datasource/youtube/YouTubeMusicDataSource.ts (beginPlayReport, updatePlayReport, pingPlaybackStats) — © noFAYZ and Zuno contributors, Apache-2.0
//! Reporting plays to the account's YouTube Music history ("Add plays to my YouTube
//! history").
//!
//! A signed-in `player` response carries `playbackTracking`: a
//! `videostatsPlaybackUrl` ("a play began") and a `videostatsWatchtimeUrl` ("this much
//! of it was heard"). YouTube Music's own clients, ArchiveTune and Zuno ping them; the
//! play then shows up in the account's history on every device and feeds its
//! recommendations. Both pings are GETs to the given URL plus a few parameters
//! (`ver=2`, `c=WEB_REMIX`, `cpn`, …), signed like an InnerTube call (cookie +
//! `SAPISIDHASH`); an unsigned one is accepted and attributed to nobody.
//!
//! * [`InnerTube::playback_tracking`] asks `player` for the URLs (signed in; the same
//!   body shape YouTube's web player sends, or YouTube answers with a stub without
//!   `playbackTracking`).
//! * [`playback_ping_url`] / [`watchtime_ping_url`] build the ping URLs;
//!   [`InnerTube::ping_stats`] sends one. The cookie only ever goes to a
//!   `*.youtube.com` HTTPS URL (or the configured loopback stand-in in tests).
//!
//! **Not verified against a real account:** the request shapes follow ArchiveTune and
//! Zuno and are unit-tested here; whether YouTube files these plays into the history
//! was not checked with a real Google account.

use serde_json::{Value, json};

use super::InnerTube;
use super::client::{CLIENT_NAME, CLIENT_VERSION, Login};
use crate::{Error, Result};

/// The alphabet YouTube's player draws a client playback nonce from.
const CPN_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
/// `ver` of the stats pings (ArchiveTune's `PLAYBACK_TELEMETRY_VER`).
const STATS_VERSION: &str = "2";
/// The audio format reported (Opus 160 kbps, what the web player streams by default).
const STATS_FORMAT: &str = "251";

/// The two tracking URLs of one `player` response.
#[derive(Clone, PartialEq, Eq)]
pub struct PlaybackTracking {
    /// `videostatsPlaybackUrl.baseUrl`: pinged once when the play starts.
    pub playback_url: String,
    /// `videostatsWatchtimeUrl.baseUrl`: pinged with the played ranges.
    pub watchtime_url: Option<String>,
}

impl std::fmt::Debug for PlaybackTracking {
    // The URLs carry per-play tokens; not worth logging.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaybackTracking")
            .field("watchtime", &self.watchtime_url.is_some())
            .finish_non_exhaustive()
    }
}

/// `playbackTracking` of a `player` response (`None` without a playback URL: an
/// anonymous or stub response).
pub fn parse_playback_tracking(player: &Value) -> Option<PlaybackTracking> {
    let tracking = player.get("playbackTracking")?;
    let url = |key: &str| {
        tracking
            .pointer(&format!("/{key}/baseUrl"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(str::to_owned)
    };
    Some(PlaybackTracking {
        playback_url: url("videostatsPlaybackUrl")?,
        watchtime_url: url("videostatsWatchtimeUrl"),
    })
}

/// A fresh client playback nonce: 16 characters, one per play (it ties the watch-time
/// pings to the play; reusing one would merge plays).
pub fn new_cpn() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        // No OS randomness: the time is unique enough for a nonce.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        bytes = nanos.to_le_bytes();
    }
    bytes
        .iter()
        .map(|b| CPN_ALPHABET[(*b & 63) as usize] as char)
        .collect()
}

/// Seconds as the stats endpoints write them (milliseconds precision, no trailing
/// zeros: `12`, `12.5`, `12.345`).
fn secs(value: f64) -> String {
    let ms = (value.max(0.0) * 1000.0).round() as u64;
    let (whole, frac) = (ms / 1000, ms % 1000);
    if frac == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{frac:03}")
            .trim_end_matches('0')
            .to_owned()
    }
}

/// `base` with `params` appended (`?` or `&`), percent-encoded.
fn with_params(base: &str, params: &[(&str, String)]) -> String {
    let mut out = base.to_owned();
    for (i, (k, v)) in params.iter().enumerate() {
        let sep = if i == 0 && !base.contains('?') {
            '?'
        } else {
            '&'
        };
        out.push(sep);
        out.push_str(k);
        out.push('=');
        out.push_str(&url::form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>());
    }
    out
}

/// Parameters every stats ping carries.
fn common(cpn: &str, since_start: f64) -> Vec<(&'static str, String)> {
    vec![
        ("ver", STATS_VERSION.to_owned()),
        ("c", CLIENT_NAME.to_owned()),
        ("cver", CLIENT_VERSION.to_owned()),
        ("cpn", cpn.to_owned()),
        ("fmt", STATS_FORMAT.to_owned()),
        ("rt", secs(since_start)),
    ]
}

/// The "a play began" ping (ArchiveTune's `registerPlayback`): `playlist_id` adds
/// `list` and the playlist page as `referrer`.
pub fn playback_ping_url(base: &str, cpn: &str, playlist_id: Option<&str>) -> String {
    let mut params = common(cpn, 0.0);
    params.push(("rtn", "0".to_owned()));
    if let Some(list) = playlist_id.filter(|l| !l.is_empty()) {
        params.push(("list", list.to_owned()));
        params.push((
            "referrer",
            format!("https://music.youtube.com/playlist?list={list}"),
        ));
    }
    with_params(base, &params)
}

/// Whether the player is playing or paused at a watch-time ping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchState {
    Playing,
    Paused,
}

/// One watch-time ping: the `ranges` of the track heard since the last ping (start,
/// end in track seconds), `position` now (`cmt`), the track `length` if known,
/// `since_start` (wall seconds since the play began, `rt`) and whether it is the
/// play's `last` ping.
#[derive(Debug, Clone, PartialEq)]
pub struct Watchtime<'a> {
    pub cpn: &'a str,
    pub ranges: &'a [(f64, f64)],
    pub position: f64,
    pub length: Option<f64>,
    pub since_start: f64,
    pub state: WatchState,
    pub last: bool,
}

/// The watch-time ping URL (`st` / `et`: comma-separated range starts and ends, as
/// YouTube's web player sends them). Ranges are clamped so they never claim more
/// listening than the wall time since the play began (Zuno: YouTube ignores such
/// pings).
pub fn watchtime_ping_url(base: &str, w: &Watchtime<'_>) -> String {
    let mut budget = w.since_start.max(0.0);
    let mut st = Vec::new();
    let mut et = Vec::new();
    for &(start, end) in w.ranges {
        if budget <= 0.0 {
            break;
        }
        let start = start.max(0.0);
        let end = end.max(start).min(start + budget);
        if end <= start {
            continue;
        }
        budget -= end - start;
        st.push(secs(start));
        et.push(secs(end));
    }
    if st.is_empty() {
        st.push(secs(w.position));
        et.push(secs(w.position));
    }
    let mut params = common(w.cpn, w.since_start);
    params.push(("st", st.join(",")));
    params.push(("et", et.join(",")));
    params.push(("cmt", secs(w.position)));
    if let Some(len) = w.length.filter(|l| *l > 0.0) {
        params.push(("len", secs(len)));
    }
    params.push((
        "state",
        match w.state {
            WatchState::Playing => "playing",
            WatchState::Paused => "paused",
        }
        .to_owned(),
    ));
    if w.last {
        params.push(("final", "1".to_owned()));
    }
    with_params(base, &params)
}

/// Whether `url` may carry the account's cookie: HTTPS on youtube.com (or, with a
/// loopback stand-in configured, that origin).
fn trusted_stats_url(url: &str, stand_in: Option<&str>) -> bool {
    if let Some(base) = stand_in
        && url.starts_with(&format!("{base}/"))
    {
        return true;
    }
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    parsed.scheme() == "https"
        && parsed
            .host_str()
            .is_some_and(|h| h == "youtube.com" || h.ends_with(".youtube.com"))
}

impl InnerTube {
    /// The tracking URLs for playing `video_id` signed in. `sts` is the player
    /// script's `signatureTimestamp` if known (YouTube may answer a request without it
    /// with a stub that has no tracking URLs). Blocking.
    pub fn playback_tracking(&self, video_id: &str, sts: Option<u32>) -> Result<PlaybackTracking> {
        let mut playback = json!({
            "contentPlaybackContext": {
                "vis": 0,
                "splay": false,
                "lactMilliseconds": "-1",
            }
        });
        if let Some(sts) = sts {
            playback["contentPlaybackContext"]["signatureTimestamp"] = json!(sts);
        }
        let body = json!({
            "videoId": video_id,
            "racyCheckOk": true,
            "contentCheckOk": true,
            "playbackContext": playback,
        });
        let resp = self.request("player", body, &[], Login::Required)?;
        parse_playback_tracking(&resp)
            .ok_or_else(|| Error::Parse("player: no playbackTracking".into()))
    }

    /// Sends one stats ping (a URL from [`playback_ping_url`] / [`watchtime_ping_url`])
    /// signed with the account. Blocking. Refuses URLs off youtube.com.
    pub fn ping_stats(&self, url: &str) -> Result<()> {
        if !self.is_signed_in() {
            return Err(Error::Parse("stats ping: not signed in".into()));
        }
        if !trusted_stats_url(url, self.api_base.as_deref()) {
            return Err(Error::Parse("stats ping: not a youtube.com URL".into()));
        }
        let mut req = crate::http::agent()
            .get(url)
            .config()
            .http_status_as_error(false)
            .build();
        for (k, v) in self.headers(true) {
            req = req.header(k, v);
        }
        let mut resp = req.call()?;
        let status = resp.status().as_u16();
        let _ = crate::http::read_body(&mut resp);
        match status {
            200..=299 => Ok(()),
            401 => Err(Error::Auth(status)),
            _ => Err(Error::Http(ureq::Error::StatusCode(status))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::innertube::Auth;

    fn query(url: &str) -> Vec<(String, String)> {
        url::Url::parse(url)
            .unwrap()
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    fn get<'a>(q: &'a [(String, String)], k: &str) -> Option<&'a str> {
        q.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }

    #[test]
    fn tracking_urls_are_read_from_the_player_response() {
        let player = json!({
            "playbackTracking": {
                "videostatsPlaybackUrl": {"baseUrl": "https://s.youtube.com/api/stats/playback?cl=1&docid=abc"},
                "videostatsWatchtimeUrl": {"baseUrl": "https://s.youtube.com/api/stats/watchtime?cl=1&docid=abc"},
                "atrUrl": {"baseUrl": "https://s.youtube.com/api/stats/atr?docid=abc"}
            }
        });
        let t = parse_playback_tracking(&player).unwrap();
        assert!(t.playback_url.ends_with("playback?cl=1&docid=abc"));
        assert!(t.watchtime_url.unwrap().contains("/watchtime?"));
        assert_eq!(parse_playback_tracking(&json!({})), None);
        assert_eq!(
            parse_playback_tracking(
                &json!({"playbackTracking": {"videostatsPlaybackUrl": {"baseUrl": " "}}})
            ),
            None
        );
    }

    #[test]
    fn playback_ping_carries_client_and_nonce() {
        let cpn = new_cpn();
        assert_eq!(cpn.len(), 16);
        assert!(cpn.bytes().all(|b| CPN_ALPHABET.contains(&b)));
        assert_ne!(cpn, new_cpn(), "one per play");

        let url = playback_ping_url(
            "https://s.youtube.com/api/stats/playback?docid=v1",
            &cpn,
            None,
        );
        let q = query(&url);
        assert_eq!(
            get(&q, "docid"),
            Some("v1"),
            "the base URL's own parameters stay"
        );
        assert_eq!(get(&q, "ver"), Some("2"));
        assert_eq!(get(&q, "c"), Some("WEB_REMIX"));
        assert_eq!(get(&q, "cver"), Some(CLIENT_VERSION));
        assert_eq!(get(&q, "cpn"), Some(cpn.as_str()));
        assert_eq!(get(&q, "rtn"), Some("0"));
        assert_eq!(get(&q, "list"), None);

        let url = playback_ping_url(
            "https://s.youtube.com/api/stats/playback",
            "n",
            Some("RDAMVMx"),
        );
        assert!(url.starts_with("https://s.youtube.com/api/stats/playback?ver=2&"));
        let q = query(&url);
        assert_eq!(get(&q, "list"), Some("RDAMVMx"));
        assert_eq!(
            get(&q, "referrer"),
            Some("https://music.youtube.com/playlist?list=RDAMVMx")
        );
    }

    #[test]
    fn watchtime_ping_reports_ranges_within_wall_time() {
        let base = "https://s.youtube.com/api/stats/watchtime?docid=v1";
        let ranges = [(0.0, 30.0), (95.5, 120.25)];
        let w = Watchtime {
            cpn: "abc",
            ranges: &ranges,
            position: 120.25,
            length: Some(200.0),
            since_start: 60.0,
            state: WatchState::Playing,
            last: false,
        };
        let q = query(&watchtime_ping_url(base, &w));
        assert_eq!(get(&q, "st"), Some("0,95.5"));
        assert_eq!(get(&q, "et"), Some("30,120.25"));
        assert_eq!(get(&q, "cmt"), Some("120.25"));
        assert_eq!(get(&q, "len"), Some("200"));
        assert_eq!(get(&q, "rt"), Some("60"));
        assert_eq!(get(&q, "state"), Some("playing"));
        assert_eq!(get(&q, "final"), None);

        // More than the wall time allows: cut to it (YouTube ignores such pings).
        let w = Watchtime {
            since_start: 40.0,
            state: WatchState::Paused,
            last: true,
            length: None,
            ..w
        };
        let q = query(&watchtime_ping_url(base, &w));
        assert_eq!(get(&q, "st"), Some("0,95.5"));
        assert_eq!(get(&q, "et"), Some("30,105.5"));
        assert_eq!(get(&q, "state"), Some("paused"));
        assert_eq!(get(&q, "final"), Some("1"));
        assert_eq!(get(&q, "len"), None);

        // Nothing heard: one empty range at the position.
        let w = Watchtime {
            ranges: &[],
            position: 3.0,
            ..w
        };
        let q = query(&watchtime_ping_url(base, &w));
        assert_eq!((get(&q, "st"), get(&q, "et")), (Some("3"), Some("3")));
    }

    #[test]
    fn the_cookie_only_goes_to_youtube() {
        assert!(trusted_stats_url(
            "https://s.youtube.com/api/stats/playback?x=1",
            None
        ));
        assert!(trusted_stats_url(
            "https://music.youtube.com/api/stats/watchtime",
            None
        ));
        assert!(!trusted_stats_url(
            "http://s.youtube.com/api/stats/playback",
            None
        ));
        assert!(!trusted_stats_url(
            "https://youtube.com.evil.example/api",
            None
        ));
        assert!(!trusted_stats_url(
            "https://evil.example/?u=youtube.com",
            None
        ));
        assert!(trusted_stats_url(
            "http://127.0.0.1:9/api/stats/playback",
            Some("http://127.0.0.1:9")
        ));
        let anonymous = InnerTube::default();
        assert!(
            anonymous
                .ping_stats("https://s.youtube.com/api/stats/playback")
                .is_err()
        );
        let signed = InnerTube::default().with_auth(Auth::new("SAPISID=x"));
        assert!(
            signed
                .ping_stats("https://example.org/api/stats/playback")
                .is_err()
        );
    }

    /// The `player` request and both pings against a fake music.youtube.com: the
    /// request body has the playback context, every ping is signed.
    #[test]
    fn report_a_play_against_a_fake_server() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::sync::{Arc, Mutex};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::<(String, String, bool)>::new()));
        let log = seen.clone();
        let tracking_base = base.clone();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let (mut len, mut signed) = (0usize, false);
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
                        break;
                    }
                    let lower = header.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                    signed |= lower.starts_with("authorization: sapisidhash ");
                }
                let mut body = vec![0; len];
                let _ = reader.read_exact(&mut body);
                log.lock().unwrap().push((
                    path.clone(),
                    String::from_utf8_lossy(&body).into_owned(),
                    signed,
                ));
                let json = if path.starts_with("/youtubei/v1/player") {
                    json!({"playbackTracking": {
                        "videostatsPlaybackUrl": {"baseUrl": format!("{tracking_base}/api/stats/playback?docid=v1")},
                        "videostatsWatchtimeUrl": {"baseUrl": format!("{tracking_base}/api/stats/watchtime?docid=v1")}
                    }})
                    .to_string()
                } else {
                    String::new()
                };
                let status = if path.starts_with("/api/stats/") {
                    204
                } else {
                    200
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                    json.len()
                );
            }
        });
        let it = InnerTube::default()
            .with_api_base(&base)
            .with_auth(Auth::new("SAPISID=fake; SID=fake"));
        let tracking = it.playback_tracking("v1", Some(20_000)).unwrap();
        let cpn = new_cpn();
        it.ping_stats(&playback_ping_url(&tracking.playback_url, &cpn, None))
            .unwrap();
        let w = Watchtime {
            cpn: &cpn,
            ranges: &[(0.0, 31.0)],
            position: 31.0,
            length: Some(180.0),
            since_start: 32.0,
            state: WatchState::Paused,
            last: true,
        };
        it.ping_stats(&watchtime_ping_url(
            tracking.watchtime_url.as_deref().unwrap(),
            &w,
        ))
        .unwrap();

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3, "{seen:?}");
        let player: Value = serde_json::from_str(&seen[0].1).unwrap();
        assert_eq!(player["videoId"], "v1");
        assert_eq!(
            player["playbackContext"]["contentPlaybackContext"]["signatureTimestamp"],
            20_000
        );
        assert_eq!(player["context"]["client"]["clientName"], "WEB_REMIX");
        assert!(seen[1].0.starts_with("/api/stats/playback?docid=v1&ver=2"));
        assert!(seen[1].0.contains(&format!("cpn={cpn}")));
        assert!(seen[2].0.starts_with("/api/stats/watchtime?docid=v1"));
        assert!(seen[2].0.contains("final=1"));
        assert!(
            seen.iter().all(|(_, _, signed)| *signed),
            "every request is signed"
        );
    }
}
