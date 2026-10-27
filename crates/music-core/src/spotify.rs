//! Spotify playlists through the official Web API, for importing them.
//!
//! The user registers their own app at <https://developer.spotify.com/dashboard>
//! (redirect URI `http://127.0.0.1:<port>/callback`) and enters its Client ID; the
//! sign-in is the Authorization Code flow with PKCE (no client secret), the redirect
//! is caught by a one-shot loopback listener ([`LoopbackRedirect`]). Scopes are
//! read-only: `playlist-read-private playlist-read-collaborative user-library-read`.
//!
//! Then [`SpotifyClient`] lists the user's playlists and liked songs and fetches a
//! playlist's tracks as [`SourceTrack`]s for [`crate::playlist_import::resolve`].
//!
//! ArchiveTune instead uses the web player's internal token endpoint with an `sp_dc`
//! cookie, which needs a TOTP secret scraped from Spotify's web player; that is not
//! done here. Playlists can also be imported without any account from an
//! [Exportify](https://exportify.app) CSV ([`crate::playlist_import::parse_csv`]).

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::http::{agent, read_body};
use crate::playlist_import::SourceTrack;
use crate::{Error, Result};

pub const AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
pub const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
pub const API_URL: &str = "https://api.spotify.com/v1";
pub const SCOPES: &str = "playlist-read-private playlist-read-collaborative user-library-read";
/// Default loopback port for the redirect (`http://127.0.0.1:43819/callback`).
pub const DEFAULT_REDIRECT_PORT: u16 = 43819;
const PAGE_LIMIT: u32 = 50;
const TIMEOUT: Duration = Duration::from_secs(20);

/// `n` random bytes from the OS.
pub(crate) fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    if getrandom::fill(&mut buf).is_err() {
        // Extremely unlikely; fall back to time-derived bytes rather than panicking.
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let digest = Sha256::digest(seed.to_le_bytes());
        for (i, b) in buf.iter_mut().enumerate() {
            *b = digest[i % digest.len()];
        }
    }
    buf
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// RFC 3986 unreserved characters pass, everything else is `%XX`.
pub(crate) fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub(crate) fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = |b: u8| (b as char).to_digit(16);
                match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    (Some(hi), Some(lo)) => {
                        out.push((hi * 16 + lo) as u8);
                        i += 3;
                        continue;
                    }
                    _ => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `a=1&b=2` → pairs (decoded).
pub(crate) fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect()
}

/// PKCE verifier + S256 challenge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn new() -> Self {
        Self::from_verifier(b64url(&random_bytes(48)))
    }

    pub fn from_verifier(verifier: String) -> Self {
        let challenge = b64url(&Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
        }
    }
}

impl Default for Pkce {
    fn default() -> Self {
        Self::new()
    }
}

/// Random `state` for the authorize request.
pub fn new_state() -> String {
    b64url(&random_bytes(16))
}

pub fn redirect_uri(port: u16) -> String {
    format!("http://127.0.0.1:{port}/callback")
}

/// The page to open in the browser.
pub fn authorize_url(client_id: &str, redirect_uri: &str, pkce: &Pkce, state: &str) -> String {
    format!(
        "{AUTHORIZE_URL}?response_type=code&client_id={}&scope={}&redirect_uri={}&state={}&code_challenge_method=S256&code_challenge={}",
        percent_encode(client_id.trim()),
        percent_encode(SCOPES),
        percent_encode(redirect_uri),
        percent_encode(state),
        percent_encode(&pkce.challenge),
    )
}

/// An access token (plus the refresh token to get the next one).
#[derive(Clone, PartialEq, Eq)]
pub struct Token {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix seconds.
    pub expires_at: u64,
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Token")
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| "…"))
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl Token {
    /// Expired or about to (within a minute).
    pub fn is_expired(&self) -> bool {
        now_secs() + 60 >= self.expires_at
    }

    fn from_response(value: &Value, previous_refresh: Option<&str>) -> Result<Self> {
        let access_token = value["access_token"]
            .as_str()
            .filter(|t| !t.is_empty())
            .ok_or_else(|| Error::Parse("Spotify token response without access_token".into()))?
            .to_owned();
        let expires_in = value["expires_in"].as_u64().unwrap_or(3600);
        Ok(Self {
            access_token,
            // Spotify may or may not rotate the refresh token.
            refresh_token: value["refresh_token"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| previous_refresh.map(str::to_owned)),
            expires_at: now_secs() + expires_in,
        })
    }
}

fn token_request(form: &[(&str, &str)]) -> Result<Value> {
    let mut resp = agent()
        .post(TOKEN_URL)
        .config()
        .http_status_as_error(false)
        .timeout_global(Some(TIMEOUT))
        .build()
        .send_form(form.iter().copied())?;
    let status = resp.status().as_u16();
    let body: Value = serde_json::from_slice(&read_body(&mut resp)?).unwrap_or(Value::Null);
    if !(200..300).contains(&status) {
        let detail = body["error_description"]
            .as_str()
            .or(body["error"].as_str())
            .unwrap_or("request failed");
        return Err(Error::Parse(format!(
            "Spotify sign-in: HTTP {status}: {detail}"
        )));
    }
    Ok(body)
}

/// Swaps the authorization `code` for tokens.
pub fn exchange_code(
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<Token> {
    let body = token_request(&[
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", client_id.trim()),
        ("code_verifier", verifier),
    ])?;
    Token::from_response(&body, None)
}

pub fn refresh(client_id: &str, refresh_token: &str) -> Result<Token> {
    let body = token_request(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id.trim()),
    ])?;
    Token::from_response(&body, Some(refresh_token))
}

/// One-shot HTTP listener on 127.0.0.1 for the OAuth redirect.
pub struct LoopbackRedirect {
    listener: TcpListener,
    port: u16,
}

impl LoopbackRedirect {
    /// Port 0 picks a free port (Spotify needs the exact redirect URI registered, so
    /// normally pass the configured port).
    pub fn bind(port: u16) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let port = listener.local_addr()?.port();
        Ok(Self { listener, port })
    }

    pub fn redirect_uri(&self) -> String {
        redirect_uri(self.port)
    }

    /// Waits for `GET /callback?code=…&state=…`, answers with a small page, and
    /// returns the code (checking `state`). Blocking, up to `timeout`.
    pub fn wait(self, expected_state: &str, timeout: Duration) -> Result<String> {
        self.listener.set_nonblocking(true)?;
        let deadline = Instant::now() + timeout;
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if let Some(result) = handle_redirect(stream, expected_state)? {
                        return result;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(Error::Parse("Spotify sign-in timed out".into()));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

/// `Some(result)` once the callback arrived; `None` for other requests (favicon).
fn handle_redirect(mut stream: TcpStream, expected_state: &str) -> Result<Option<Result<String>>> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let target = line.split_whitespace().nth(1).unwrap_or("");
    let Some(query) = target
        .strip_prefix("/callback")
        .map(|rest| rest.trim_start_matches('?'))
    else {
        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        return Ok(None);
    };
    let params = parse_query(query);
    let get = |k: &str| {
        params
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.clone())
    };
    let result = if let Some(error) = get("error") {
        Err(Error::Parse(format!("Spotify sign-in refused: {error}")))
    } else if get("state").as_deref() != Some(expected_state) {
        Err(Error::Parse("Spotify sign-in: state mismatch".into()))
    } else {
        get("code")
            .filter(|c| !c.is_empty())
            .ok_or_else(|| Error::Parse("Spotify sign-in: no code".into()))
    };
    let message = if result.is_ok() {
        "Signed in to Spotify. You can close this tab and return to the app."
    } else {
        "Spotify sign-in failed. Return to the app for details."
    };
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>Spotify</title><body style=\"font:16px system-ui;margin:3em\">{message}</body>"
    );
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    Ok(Some(result))
}

/// A playlist in the user's library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotifyPlaylist {
    pub id: String,
    pub name: String,
    pub owner: String,
    pub track_count: Option<u32>,
    pub image_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotifyUser {
    pub id: String,
    pub display_name: String,
}

/// The playlist id of `https://open.spotify.com/playlist/<id>?si=…`,
/// `spotify:playlist:<id>` or a bare id.
pub fn parse_playlist_id(input: &str) -> Option<String> {
    let input = input.trim();
    let id = if let Some(rest) = input.strip_prefix("spotify:playlist:") {
        rest
    } else if let Some((_, rest)) = input.split_once("/playlist/") {
        rest.split(['?', '/', '#']).next()?
    } else {
        input
    };
    (id.len() >= 16 && id.bytes().all(|b| b.is_ascii_alphanumeric())).then(|| id.to_owned())
}

/// A playlist item / saved track (`{ "track": {...} }` or `{ "item": {...} }`) as a
/// [`SourceTrack`]; `None` for episodes, local files without a name, removed tracks.
pub fn source_track(item: &Value) -> Option<SourceTrack> {
    let track = if item["item"].is_object() {
        &item["item"]
    } else if item["track"].is_object() {
        &item["track"]
    } else {
        item
    };
    if track["type"].as_str().is_some_and(|t| t != "track") {
        return None;
    }
    let title = track["name"].as_str().filter(|n| !n.trim().is_empty())?;
    let images = track["album"]["images"].as_array();
    Some(SourceTrack {
        title: title.to_owned(),
        artists: track["artists"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|a| a["name"].as_str())
                    .filter(|n| !n.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        album: track["album"]["name"].as_str().map(str::to_owned),
        duration_secs: track["duration_ms"]
            .as_u64()
            .map(|ms| ((ms + 500) / 1000) as u32)
            .filter(|&s| s > 0),
        explicit: track["explicit"].as_bool(),
        isrc: track["external_ids"]["isrc"].as_str().map(str::to_owned),
        source_id: track["uri"].as_str().map(str::to_owned),
        video_id: None,
        thumbnail_url: images.and_then(|imgs| {
            imgs.iter()
                .find(|i| {
                    i["width"]
                        .as_u64()
                        .is_some_and(|w| (200..=400).contains(&w))
                })
                .or_else(|| imgs.first())
                .and_then(|i| i["url"].as_str())
                .map(str::to_owned)
        }),
    })
}

fn playlist_from(value: &Value) -> Option<SpotifyPlaylist> {
    Some(SpotifyPlaylist {
        id: value["id"].as_str()?.to_owned(),
        name: value["name"].as_str().unwrap_or("Playlist").to_owned(),
        owner: value["owner"]["display_name"]
            .as_str()
            .or(value["owner"]["id"].as_str())
            .unwrap_or("")
            .to_owned(),
        // `tracks.total` (renamed `items.total` in newer API versions).
        track_count: value["tracks"]["total"]
            .as_u64()
            .or(value["items"]["total"].as_u64())
            .map(|n| n as u32),
        image_url: value["images"]
            .as_array()
            .and_then(|i| i.first())
            .and_then(|i| i["url"].as_str())
            .map(str::to_owned),
    })
}

/// Blocking Web API client for one access token.
pub struct SpotifyClient {
    token: String,
}

impl SpotifyClient {
    pub fn new(access_token: impl Into<String>) -> Self {
        Self {
            token: access_token.into(),
        }
    }

    fn get(&self, url: &str) -> Result<(u16, Value)> {
        let url = if url.starts_with("https://") {
            url.to_owned()
        } else {
            format!("{API_URL}{url}")
        };
        let mut resp = agent()
            .get(&url)
            .config()
            .http_status_as_error(false)
            .timeout_global(Some(TIMEOUT))
            .build()
            .header("Authorization", &format!("Bearer {}", self.token))
            .call()?;
        let status = resp.status().as_u16();
        let body = serde_json::from_slice(&read_body(&mut resp)?).unwrap_or(Value::Null);
        Ok((status, body))
    }

    fn get_ok(&self, url: &str) -> Result<Value> {
        let (status, body) = self.get(url)?;
        if (200..300).contains(&status) {
            Ok(body)
        } else {
            let detail = body["error"]["message"]
                .as_str()
                .unwrap_or("request failed");
            Err(Error::Parse(format!("Spotify: HTTP {status}: {detail}")))
        }
    }

    /// Follows `next` links; `progress(done, total)` per page.
    fn paged(
        &self,
        first: &str,
        progress: &dyn Fn(usize, Option<usize>),
        mut each: impl FnMut(&Value),
    ) -> Result<()> {
        let mut url = Some(first.to_owned());
        let mut done = 0usize;
        while let Some(next) = url.take() {
            let page = self.get_ok(&next)?;
            for item in page["items"].as_array().into_iter().flatten() {
                each(item);
                done += 1;
            }
            progress(done, page["total"].as_u64().map(|t| t as usize));
            url = page["next"].as_str().map(str::to_owned);
        }
        Ok(())
    }

    pub fn me(&self) -> Result<SpotifyUser> {
        let v = self.get_ok("/me")?;
        Ok(SpotifyUser {
            id: v["id"].as_str().unwrap_or("").to_owned(),
            display_name: v["display_name"]
                .as_str()
                .or(v["id"].as_str())
                .unwrap_or("Spotify user")
                .to_owned(),
        })
    }

    /// The user's playlists (own, followed, collaborative).
    pub fn playlists(&self) -> Result<Vec<SpotifyPlaylist>> {
        let mut out = Vec::new();
        self.paged(
            &format!("/me/playlists?limit={PAGE_LIMIT}"),
            &|_, _| {},
            |item| out.extend(playlist_from(item)),
        )?;
        Ok(out)
    }

    pub fn playlist(&self, id: &str) -> Result<SpotifyPlaylist> {
        playlist_from(&self.get_ok(&format!(
            "/playlists/{id}?fields=id,name,owner,images,tracks.total"
        ))?)
        .ok_or_else(|| Error::Parse("Spotify: playlist without id".into()))
    }

    /// Every track of a playlist (`/items`, falling back to the older `/tracks`).
    pub fn playlist_tracks(
        &self,
        id: &str,
        progress: &dyn Fn(usize, Option<usize>),
    ) -> Result<Vec<SourceTrack>> {
        let mut out = Vec::new();
        let items = format!("/playlists/{id}/items?limit={PAGE_LIMIT}&additional_types=track");
        match self.paged(&items, progress, |item| out.extend(source_track(item))) {
            Ok(()) => Ok(out),
            Err(_) if out.is_empty() => {
                let tracks = format!("/playlists/{id}/tracks?limit={PAGE_LIMIT}");
                self.paged(&tracks, progress, |item| out.extend(source_track(item)))?;
                Ok(out)
            }
            Err(e) => Err(e),
        }
    }

    /// Liked songs ("Your Library → Liked Songs").
    pub fn liked_tracks(
        &self,
        progress: &dyn Fn(usize, Option<usize>),
    ) -> Result<Vec<SourceTrack>> {
        let mut out = Vec::new();
        self.paged(
            &format!("/me/tracks?limit={PAGE_LIMIT}"),
            progress,
            |item| out.extend(source_track(item)),
        )?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc7636_example() {
        // RFC 7636, appendix B.
        let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
        assert_eq!(
            pkce.challenge,
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let a = Pkce::new();
        assert!(a.verifier.len() >= 43 && a.verifier.len() <= 128);
        assert_ne!(a.verifier, Pkce::new().verifier);
    }

    #[test]
    fn authorize_url_is_encoded() {
        let pkce = Pkce::from_verifier("v".into());
        let url = authorize_url("abc", &redirect_uri(43819), &pkce, "s t");
        assert!(url.starts_with("https://accounts.spotify.com/authorize?response_type=code&client_id=abc&scope=playlist-read-private%20playlist-read-collaborative%20user-library-read&redirect_uri=http%3A%2F%2F127.0.0.1%3A43819%2Fcallback&state=s%20t&code_challenge_method=S256&code_challenge="));
        assert_eq!(percent_decode("a%20b+c%2"), "a b c%2");
        assert_eq!(
            parse_query("code=x%2Fy&state=1"),
            vec![("code".into(), "x/y".into()), ("state".into(), "1".into())]
        );
    }

    #[test]
    fn playlist_ids() {
        for input in [
            "https://open.spotify.com/playlist/37i9dQZF1DXcBWIGoYBM5M?si=abc",
            "spotify:playlist:37i9dQZF1DXcBWIGoYBM5M",
            "37i9dQZF1DXcBWIGoYBM5M",
            "https://open.spotify.com/intl-de/playlist/37i9dQZF1DXcBWIGoYBM5M",
        ] {
            assert_eq!(
                parse_playlist_id(input).as_deref(),
                Some("37i9dQZF1DXcBWIGoYBM5M"),
                "{input}"
            );
        }
        assert_eq!(parse_playlist_id("https://open.spotify.com/track/x"), None);
    }

    #[test]
    fn parses_playlist_items() {
        let page: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/import/spotify_playlist_items.json"
        ))
        .unwrap();
        let tracks: Vec<SourceTrack> = page["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(source_track)
            .collect();
        assert_eq!(tracks.len(), 2, "episode and null track skipped");
        assert_eq!(tracks[0].title, "Instant Crush (feat. Julian Casablancas)");
        assert_eq!(tracks[0].artists, vec!["Daft Punk", "Julian Casablancas"]);
        assert_eq!(tracks[0].duration_secs, Some(338));
        assert_eq!(tracks[0].isrc.as_deref(), Some("USQX91300105"));
        assert_eq!(tracks[0].explicit, Some(false));
        assert_eq!(
            tracks[0].thumbnail_url.as_deref(),
            Some("https://i.scdn.co/300")
        );
        // Newer API shape: `item` instead of `track`.
        assert_eq!(tracks[1].title, "Halo");
        let playlist = playlist_from(&page["playlist"]).unwrap();
        assert_eq!(playlist.track_count, Some(4));
        assert_eq!(playlist.owner, "Someone");
    }

    #[test]
    fn loopback_redirect_returns_the_code() {
        let redirect = LoopbackRedirect::bind(0).unwrap();
        let uri = redirect.redirect_uri();
        let port: u16 = uri
            .trim_start_matches("http://127.0.0.1:")
            .trim_end_matches("/callback")
            .parse()
            .unwrap();
        let client = std::thread::spawn(move || {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.write_all(b"GET /favicon.ico HTTP/1.1\r\n\r\n").unwrap();
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.write_all(b"GET /callback?code=abc%2B1&state=st HTTP/1.1\r\nHost: x\r\n\r\n")
                .unwrap();
            let mut body = String::new();
            std::io::Read::read_to_string(&mut s, &mut body).unwrap();
            body
        });
        let code = redirect.wait("st", Duration::from_secs(5)).unwrap();
        assert_eq!(code, "abc+1");
        assert!(client.join().unwrap().contains("Signed in to Spotify"));

        let redirect = LoopbackRedirect::bind(0).unwrap();
        let port = redirect.port;
        std::thread::spawn(move || {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.write_all(b"GET /callback?code=abc&state=evil HTTP/1.1\r\n\r\n")
                .unwrap();
        });
        assert!(redirect.wait("st", Duration::from_secs(5)).is_err());
    }
}
