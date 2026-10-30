//! [`WebResolver`]: full-length YouTube audio through the TV client (JS challenges
//! only) or the mobile web client (JS challenges + a content-bound PoToken).
//!
//! What a stream URL needs today (verified against googlevideo, Oct 2026; the
//! client definitions and the ad-wait rule follow yt-dlp, which is public domain):
//! * the player request carries the visitor id and the player's `signatureTimestamp`;
//! * `n` (throttling) and, for ciphered formats, the signature are transformed
//!   with the player script's own functions ([`ChallengeSolver`]);
//! * web clients additionally need `pot=<PoToken bound to the video id>` on the URL
//!   and the same token in the player request;
//! * googlevideo rejects the URL (403) until the pre-roll ad could have been
//!   skipped: [`ResolvedStream::not_before`] carries that time and the downloader
//!   waits for it.

use std::path::PathBuf;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

use super::jsc::ChallengeSolver;
use super::potoken::PoTokenProvider;
use super::session::{BROWSER_UA, SessionCache, WebSession};
use crate::http::agent;
use crate::innertube::Auth;
use crate::stream::{
    CHUNK_BYTES, IosClientResolver, PrefetchedHead, Quality, ResolvedStream, StreamPrefix,
    StreamResolver, StreamSource,
};
use crate::{Error, Result};

/// googlevideo serves web/TV clients in ranges of this size (yt-dlp's chunk size).
pub(crate) const WEB_CHUNK_BYTES: u64 = 10 * 1024 * 1024;

/// Which InnerTube client asks for the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum WebClient {
    /// Samsung Tizen TV client: no PoToken, JS challenges only.
    Tv,
    /// Mobile web: JS challenges plus a PoToken (needs a [`PoTokenProvider`]).
    MobileWeb,
}

impl WebClient {
    pub fn label(self) -> &'static str {
        match self {
            WebClient::Tv => "TV",
            WebClient::MobileWeb => "Web + PoToken",
        }
    }

    fn context(self) -> Value {
        match self {
            WebClient::Tv => json!({
                "clientName": "TVHTML5",
                "clientVersion": "5.20260707",
                "deviceMake": "Samsung",
                "deviceModel": "UKS9800",
                "userAgent": TV_UA,
                "osName": "Tizen",
                "osVersion": "2.4.0",
            }),
            WebClient::MobileWeb => json!({
                "clientName": "MWEB",
                "clientVersion": "2.20260708.05.00",
                "userAgent": MWEB_UA,
            }),
        }
    }

    fn client_id(self) -> &'static str {
        match self {
            WebClient::Tv => "7",
            WebClient::MobileWeb => "2",
        }
    }

    fn user_agent(self) -> &'static str {
        match self {
            WebClient::Tv => TV_UA,
            WebClient::MobileWeb => MWEB_UA,
        }
    }

    fn origin(self) -> &'static str {
        match self {
            WebClient::Tv => "https://www.youtube.com",
            WebClient::MobileWeb => "https://m.youtube.com",
        }
    }
}

const TV_UA: &str = "Mozilla/5.0 (SMART-TV; Linux; Tizen 2.4.0) AppleWebKit/538.1 (KHTML, like Gecko) Version/2.4.0 TV Safari/538.1";
const MWEB_UA: &str = "Mozilla/5.0 (iPad; CPU OS 16_7_10 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/16.6 Mobile/15E148 Safari/604.1,gzip(gfe)";

/// Resolves through [`WebClient`]s in order, then an optional fallback resolver.
pub struct WebResolver {
    clients: RwLock<Vec<WebClient>>,
    potoken: Option<Arc<dyn PoTokenProvider>>,
    fallback: Option<Arc<dyn StreamResolver>>,
    auth: RwLock<Option<Auth>>,
    session: Arc<SessionCache>,
    solver: Arc<ChallengeSolver>,
}

impl WebResolver {
    /// `cache_dir` keeps preprocessed player scripts between runs (recommended).
    pub fn new(
        clients: Vec<WebClient>,
        potoken: Option<Arc<dyn PoTokenProvider>>,
        fallback: Option<Arc<dyn StreamResolver>>,
        cache_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            clients: RwLock::new(clients),
            potoken,
            fallback,
            auth: RwLock::new(None),
            session: Arc::new(SessionCache::new(cache_dir.clone())),
            solver: Arc::new(ChallengeSolver::new(cache_dir)),
        }
    }

    /// Clients to try, in order (empty: only the fallback).
    pub fn set_clients(&self, clients: Vec<WebClient>) {
        *self.clients.write().unwrap_or_else(PoisonError::into_inner) = clients;
    }

    pub fn clients(&self) -> Vec<WebClient> {
        self.clients
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Account cookies for the player request (signed-in playback), or none.
    pub fn set_auth(&self, auth: Option<Auth>) {
        *self.auth.write().unwrap_or_else(PoisonError::into_inner) =
            auth.filter(Auth::is_signed_in);
    }

    pub fn has_potoken(&self) -> bool {
        self.potoken.is_some()
    }

    /// Does the slow one-time work up front so the first song starts quickly:
    /// fetches the session, preprocesses the player script, and (if the web
    /// client is enabled) bootstraps BotGuard. Blocking, a few seconds; call it
    /// from a background thread (never the main thread on macOS). Errors are
    /// only logged: resolving retries everything on demand.
    pub fn prewarm(&self) {
        // Any public video works for the watch page; this is YouTube's first one.
        const ANY_VIDEO: &str = "jNQXAC9IVRw";
        let clients = self.clients();
        if clients.is_empty() {
            return;
        }
        let warm = || -> Result<()> {
            let mut session = self.session.get(ANY_VIDEO)?;
            preprocess(&self.session, &self.solver, &mut session)?;
            if clients.contains(&WebClient::MobileWeb)
                && let Some(provider) = &self.potoken
            {
                provider.content_token(ANY_VIDEO)?;
            }
            Ok(())
        };
        let started = std::time::Instant::now();
        match warm() {
            Ok(()) => log::info!("youtube: stream resolver ready in {:?}", started.elapsed()),
            Err(e) => log::warn!("youtube: prewarm failed (will retry on demand): {e}"),
        }
    }

    fn resolve_with(
        &self,
        client: WebClient,
        video_id: &str,
        quality: Quality,
    ) -> Result<ResolvedStream> {
        // The iOS client's preview URL is fetched meanwhile: if it's the same file,
        // its first MiB lets playback start before the web URL becomes usable.
        // Its head starts downloading as soon as the iOS client answers, while
        // the web client is still busy (player request, challenges), so the
        // first bytes are usually in by the time the web URL is confirmed.
        std::thread::scope(|scope| {
            let started = std::time::Instant::now();
            let preview = scope.spawn(move || {
                let ios = IosClientResolver.resolve_with_quality(video_id, quality)?;
                log::debug!(
                    "youtube: {video_id}: iOS client after {:?}",
                    started.elapsed()
                );
                let bytes = CHUNK_BYTES.min(ios.content_length.unwrap_or(CHUNK_BYTES));
                let head = PrefetchedHead::start(&ios.url, ios.user_agent, bytes);
                Ok::<_, Error>((ios, head))
            });
            let mut stream = self.resolve_web(client, video_id, quality)?;
            if let Ok(Ok((ios, head))) = preview.join() {
                stream.prefix = same_file_prefix(&stream, &ios).map(|prefix| {
                    log::debug!(
                        "youtube: {video_id}: same file as the iOS head ({} bytes prefetched)",
                        head.len()
                    );
                    StreamPrefix {
                        head: Some(head),
                        ..prefix
                    }
                });
            }
            Ok(stream)
        })
    }

    /// Renews the PoToken attestation on a background thread when it ends within
    /// 30 minutes, so the ~1 s re-attestation never delays a song.
    fn renew_potoken_in_background(&self) {
        const RENEW_WITHIN: Duration = Duration::from_secs(30 * 60);
        let Some(provider) = self.potoken.clone() else {
            return;
        };
        if !self.clients().contains(&WebClient::MobileWeb)
            || !provider.attestation_expiring(RENEW_WITHIN)
        {
            return;
        }
        let _ = std::thread::Builder::new()
            .name("music-core-botguard".into())
            .spawn(move || {
                if let Err(e) = provider.renew_if_expiring(RENEW_WITHIN) {
                    log::warn!("youtube: renewing BotGuard ahead of expiry failed: {e}");
                }
            });
    }

    /// Fetches the next session (and preprocesses a new player) on a
    /// background thread once the current one gets old, see [`SessionCache`].
    fn refresh_session_in_background(&self, video_id: &str) {
        if !self.session.claim_refresh() {
            return;
        }
        let (cache, solver, video_id) = (
            self.session.clone(),
            self.solver.clone(),
            video_id.to_owned(),
        );
        let spawned = std::thread::Builder::new()
            .name("music-core-session".into())
            .spawn(move || {
                let started = std::time::Instant::now();
                let fresh = cache.fetch_fresh(&video_id).and_then(|mut session| {
                    preprocess(&cache, &solver, &mut session)?;
                    Ok(session)
                });
                match &fresh {
                    Ok(_) => log::info!("youtube: session renewed in {:?}", started.elapsed()),
                    Err(e) => log::warn!("youtube: renewing the session failed: {e}"),
                }
                cache.finish_refresh(fresh.ok());
            });
        if spawned.is_err() {
            self.session.finish_refresh(None);
        }
    }

    fn resolve_web(
        &self,
        client: WebClient,
        video_id: &str,
        quality: Quality,
    ) -> Result<ResolvedStream> {
        let started = std::time::Instant::now();
        let mut session = self.session.get(video_id)?;
        // Load the player into the solver while the player request is in flight.
        self.solver.prepare(&session.player_id);
        self.refresh_session_in_background(video_id);
        log::debug!("youtube: {video_id}: session after {:?}", started.elapsed());
        let pot = match client {
            WebClient::MobileWeb => Some(
                self.potoken
                    .as_ref()
                    .ok_or_else(|| Error::Unplayable("no PoToken provider".into()))?
                    .content_token(video_id)?,
            ),
            WebClient::Tv => None,
        };
        let player = match self.player_request(client, video_id, &session, pot.as_deref()) {
            Err(Error::Unplayable(reason)) if is_bot_check(&reason) => {
                // A stale visitor id is the usual cause: start a fresh session once.
                self.session.invalidate();
                session = self.session.get(video_id)?;
                self.player_request(client, video_id, &session, pot.as_deref())?
            }
            other => other?,
        };
        let requested_at = SystemTime::now();
        log::debug!(
            "youtube: {video_id}: player response after {:?}",
            started.elapsed()
        );
        let format = select_format(&player, quality)?;
        let (url, n, sig) = format.cipher_parts()?;

        let script = if self.solver.has_preprocessed(&session.player_id) {
            None
        } else {
            Some(self.session.script(&mut session)?)
        };
        let had_script = script.is_some();
        let solutions = self.solver.solve(
            &session.player_id,
            script,
            n.iter().cloned().collect(),
            sig.iter().map(|(s, _)| s.clone()).collect(),
        )?;
        if had_script {
            // Preprocessed now; the full script isn't needed again.
            self.session.drop_script(&session.player_id);
        }
        log::debug!(
            "youtube: {video_id}: challenges solved after {:?}",
            started.elapsed()
        );
        let mut url =
            url::Url::parse(&url).map_err(|e| Error::Parse(format!("stream URL: {e}")))?;
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .map(|(k, v)| {
                let v = if k == "n" {
                    solutions
                        .n
                        .get(v.as_ref())
                        .cloned()
                        .unwrap_or_else(|| v.into_owned())
                } else {
                    v.into_owned()
                };
                (k.into_owned(), v)
            })
            .collect();
        if let Some(n) = &n
            && !solutions.n.contains_key(n)
        {
            return Err(Error::Parse(
                "challenge solver returned no n solution".into(),
            ));
        }
        {
            let mut query = url.query_pairs_mut();
            query.clear().extend_pairs(&pairs);
            if let Some((s, param)) = &sig {
                let solved = solutions
                    .sig
                    .get(s)
                    .ok_or_else(|| Error::Parse("challenge solver returned no signature".into()))?;
                query.append_pair(param, solved);
            }
            if let Some(pot) = &pot {
                query.append_pair("pot", pot);
            }
        }
        let wait = ad_wait(&player);
        let mut source = StreamSource::youtube(format.itag);
        source.variant = Some(client.label().to_owned());
        log::info!(
            "youtube: {video_id} itag {} via {} (player {}, wait {:?})",
            format.itag,
            client.label(),
            session.player_id,
            wait
        );
        Ok(ResolvedStream {
            url: url.into(),
            itag: format.itag,
            mime_type: format.mime_type,
            content_length: format.content_length,
            user_agent: BROWSER_UA,
            loudness_db: format.loudness_db,
            source: Some(source),
            not_before: Some(requested_at + wait + Duration::from_millis(500)),
            chunk_size: Some(WEB_CHUNK_BYTES),
            full_access: true,
            ..Default::default()
        })
    }

    fn player_request(
        &self,
        client: WebClient,
        video_id: &str,
        session: &WebSession,
        pot: Option<&str>,
    ) -> Result<Value> {
        let mut client_ctx = client.context();
        client_ctx["hl"] = json!("en");
        client_ctx["timeZone"] = json!("UTC");
        client_ctx["utcOffsetMinutes"] = json!(0);
        let mut body = json!({
            "context": { "client": client_ctx },
            "videoId": video_id,
            "playbackContext": { "contentPlaybackContext": {
                "html5Preference": "HTML5_PREF_WANTS",
                "signatureTimestamp": session.sts,
            }},
            "contentCheckOk": true,
            "racyCheckOk": true,
        });
        if let Some(pot) = pot {
            body["serviceIntegrityDimensions"] = json!({ "poToken": pot });
        }
        let auth = self
            .auth
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let visitor = auth
            .as_ref()
            .and_then(|a| a.visitor_data.clone())
            .unwrap_or_else(|| session.visitor_data.clone());
        let mut req = agent()
            .post("https://www.youtube.com/youtubei/v1/player?prettyPrint=false")
            .header("User-Agent", client.user_agent())
            .header("Content-Type", "application/json")
            .header("X-Youtube-Client-Name", client.client_id())
            .header(
                "X-Youtube-Client-Version",
                client.context()["clientVersion"]
                    .as_str()
                    .unwrap_or_default(),
            )
            .header("Origin", client.origin())
            .header("X-Goog-Visitor-Id", visitor.as_str());
        if let Some(auth) = &auth {
            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            // The session's current cookie (shared with the browse client).
            let (cookie, _) = auth.signing_cookie();
            if let Some(authorization) =
                crate::innertube::authorization_for(&cookie, client.origin(), now)
            {
                req = req
                    .header("Cookie", cookie)
                    .header("Authorization", authorization)
                    .header("X-Goog-AuthUser", auth.auth_user.to_string())
                    .header("X-Origin", client.origin());
            }
        }
        let mut resp = req.send_json(&body)?;
        if let Some(auth) = &auth {
            // Rotated session cookies, as for every signed-in InnerTube response.
            let lines = resp
                .headers()
                .get_all(ureq::http::header::SET_COOKIE)
                .iter()
                .filter_map(|v| v.to_str().ok());
            auth.absorb_set_cookies("https://www.youtube.com", false, lines);
        }
        let player: Value = resp.body_mut().read_json()?;
        let status = player
            .pointer("/playabilityStatus/status")
            .and_then(Value::as_str)
            .unwrap_or("?");
        if status != "OK" {
            let reason = player
                .pointer("/playabilityStatus/reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown reason");
            return Err(Error::Unplayable(format!("{status}: {reason}")));
        }
        Ok(player)
    }
}

impl StreamResolver for WebResolver {
    fn resolve(&self, video_id: &str) -> Result<ResolvedStream> {
        self.resolve_with_quality(video_id, Quality::Normal)
    }

    fn resolve_with_quality(&self, video_id: &str, quality: Quality) -> Result<ResolvedStream> {
        self.renew_potoken_in_background();
        let mut last = None;
        for client in self.clients() {
            match self.resolve_with(client, video_id, quality) {
                Ok(stream) => return Ok(stream),
                Err(e) => {
                    log::warn!("youtube: {} failed for {video_id}: {e}", client.label());
                    if client == WebClient::MobileWeb
                        && let Some(p) = &self.potoken
                    {
                        // A rejected token or a broken page: start BotGuard over next time.
                        if matches!(e, Error::Audio(_)) {
                            p.reset();
                        }
                    }
                    last = Some(e);
                }
            }
        }
        match &self.fallback {
            Some(fallback) => fallback
                .resolve_with_quality(video_id, quality)
                .map_err(|e| match last {
                    Some(web) => {
                        Error::Unplayable(format!("{}; fallback: {}", reason(&web), reason(&e)))
                    }
                    None => e,
                }),
            None => {
                Err(last.unwrap_or_else(|| Error::Unplayable("no stream clients enabled".into())))
            }
        }
    }
}

/// Makes sure the solver has `session`'s player preprocessed (seconds, once per player).
fn preprocess(
    cache: &SessionCache,
    solver: &ChallengeSolver,
    session: &mut WebSession,
) -> Result<()> {
    if !solver.has_preprocessed(&session.player_id) {
        let script = cache.script(session)?;
        // Any challenge makes the solver preprocess (and cache) the player.
        solver.solve(
            &session.player_id,
            Some(script),
            vec!["AAAAAAAAAAAAAAAA".into()],
            vec![],
        )?;
        session.script = None;
        cache.drop_script(&session.player_id);
    }
    Ok(())
}

/// `ios` as a [`StreamPrefix`] of `web`, if both are provably the same file
/// (format, size and last-modified time).
fn same_file_prefix(web: &ResolvedStream, ios: &ResolvedStream) -> Option<StreamPrefix> {
    let lmt = |url: &str| {
        url::Url::parse(url)
            .ok()?
            .query_pairs()
            .find(|(k, _)| k == "lmt")
            .map(|(_, v)| v.into_owned())
    };
    let same = web.itag == ios.itag
        && web.content_length.is_some()
        && web.content_length == ios.content_length
        && lmt(&web.url).is_some()
        && lmt(&web.url) == lmt(&ios.url);
    same.then(|| StreamPrefix {
        url: ios.url.clone(),
        user_agent: ios.user_agent,
        bytes: CHUNK_BYTES,
        head: None,
    })
}

/// YouTube's "confirm you're not a bot" (a stale session), not other sign-in
/// requirements such as "Sign in to confirm your age".
fn is_bot_check(reason: &str) -> bool {
    let r = reason.to_lowercase();
    r.contains("not a bot") || (r.contains("sign in to confirm you") && !r.contains("your age"))
}

/// An error's message without the "not playable: " wrapper.
fn reason(e: &Error) -> String {
    match e {
        Error::Unplayable(reason) => reason.clone(),
        other => other.to_string(),
    }
}

/// A format's base URL, its `n` challenge, and its `(s, param)` signature challenge.
pub(crate) type CipherParts = (String, Option<String>, Option<(String, String)>);

/// An audio format from a player response.
#[derive(Debug, Clone)]
pub(crate) struct Format {
    pub itag: u32,
    pub mime_type: String,
    pub content_length: Option<u64>,
    pub loudness_db: Option<f32>,
    url: Option<String>,
    signature_cipher: Option<String>,
}

impl Format {
    /// The base URL, its `n` challenge, and the `(s, param)` signature challenge.
    pub(crate) fn cipher_parts(&self) -> Result<CipherParts> {
        let (url, sig) = match (&self.url, &self.signature_cipher) {
            (Some(url), _) => (url.clone(), None),
            (None, Some(cipher)) => {
                let fields: std::collections::HashMap<String, String> =
                    url::form_urlencoded::parse(cipher.as_bytes())
                        .into_owned()
                        .collect();
                let url = fields
                    .get("url")
                    .cloned()
                    .ok_or_else(|| Error::Parse("signatureCipher without url".into()))?;
                let s = fields
                    .get("s")
                    .cloned()
                    .ok_or_else(|| Error::Parse("signatureCipher without s".into()))?;
                let param = fields
                    .get("sp")
                    .cloned()
                    .unwrap_or_else(|| "signature".into());
                (url, Some((s, param)))
            }
            (None, None) => return Err(Error::Parse("format without URL".into())),
        };
        let n = url::Url::parse(&url).ok().and_then(|u| {
            u.query_pairs()
                .find(|(k, _)| k == "n")
                .map(|(_, v)| v.into_owned())
        });
        Ok((url, n, sig))
    }
}

/// The preferred audio-only format for `quality` (same itag order as the iOS resolver).
pub(crate) fn select_format(player: &Value, quality: Quality) -> Result<Format> {
    let formats: Vec<Format> = player
        .pointer("/streamingData/adaptiveFormats")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|f| {
            f["mimeType"]
                .as_str()
                .is_some_and(|m| m.starts_with("audio/"))
                && (f.get("url").is_some() || f.get("signatureCipher").is_some())
                && f.get("drmFamilies").is_none()
        })
        .map(|f| Format {
            itag: f["itag"].as_u64().unwrap_or_default() as u32,
            mime_type: f["mimeType"].as_str().unwrap_or_default().to_owned(),
            content_length: f["contentLength"].as_str().and_then(|s| s.parse().ok()),
            loudness_db: f["loudnessDb"].as_f64().map(|db| db as f32),
            url: f["url"].as_str().map(str::to_owned),
            signature_cipher: f["signatureCipher"].as_str().map(str::to_owned),
        })
        .collect();
    quality
        .preferred_itags()
        .iter()
        .find_map(|itag| formats.iter().find(|f| f.itag == *itag).cloned())
        .ok_or_else(|| Error::Unplayable("no audio format in the player response".into()))
}

/// How long googlevideo makes clients wait before a stream URL works: the
/// pre-roll ad's length, or its skip offset if skippable (yt-dlp's rule).
pub(crate) fn ad_wait(player: &Value) -> Duration {
    let mut renderers: Vec<&Value> = Vec::new();
    for placement in player["adPlacements"].as_array().into_iter().flatten() {
        let r = &placement["adPlacementRenderer"];
        if r.pointer("/config/adPlacementConfig/kind")
            .and_then(Value::as_str)
            == Some("AD_PLACEMENT_KIND_START")
        {
            renderers.push(&r["renderer"]);
        }
    }
    for slot in player["adSlots"].as_array().into_iter().flatten() {
        let r = &slot["adSlotRenderer"];
        if r.pointer("/adSlotMetadata/triggerEvent")
            .and_then(Value::as_str)
            == Some("SLOT_TRIGGER_EVENT_BEFORE_CONTENT")
        {
            let content = &r["fulfillmentContent"]["fulfilledLayout"]["playerBytesAdLayoutRenderer"]
                ["renderingContent"];
            renderers.push(content);
            for layout in content
                .pointer("/playerBytesSequentialLayoutRenderer/sequentialLayouts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                renderers.push(&layout["playerBytesAdLayoutRenderer"]["renderingContent"]);
            }
        }
    }
    let mut total = 0.0f64;
    for renderer in renderers {
        let Some(ad) = renderer
            .get("instreamVideoAdRenderer")
            .filter(|a| a.is_object())
        else {
            continue;
        };
        let length = ad["playerVars"].as_str().and_then(|vars| {
            url::form_urlencoded::parse(vars.as_bytes())
                .filter(|(k, _)| k == "length_seconds")
                .last()
                .and_then(|(_, v)| v.parse::<f64>().ok())
        });
        let skip = ad["skipOffsetMilliseconds"]
            .as_f64()
            .or_else(|| {
                ad["skipOffsetMilliseconds"]
                    .as_str()
                    .and_then(|s| s.parse().ok())
            })
            .map(|ms| ms / 1000.0);
        if let Some(seconds) = skip.or(length) {
            total += seconds.max(0.0);
        }
    }
    Duration::from_secs_f64(total.min(120.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ad_wait_from_placements_and_slots() {
        let placement = json!({ "adPlacements": [
            { "adPlacementRenderer": {
                "config": { "adPlacementConfig": { "kind": "AD_PLACEMENT_KIND_START" } },
                "renderer": { "instreamVideoAdRenderer": { "playerVars": "a=1&length_seconds=15", "skipOffsetMilliseconds": 5000 } }
            }},
            { "adPlacementRenderer": {
                "config": { "adPlacementConfig": { "kind": "AD_PLACEMENT_KIND_MILLISECONDS" } },
                "renderer": { "instreamVideoAdRenderer": { "playerVars": "length_seconds=30" } }
            }}
        ]});
        assert_eq!(ad_wait(&placement), Duration::from_secs(5));

        let content = json!({ "instreamVideoAdRenderer": { "playerVars": "length_seconds=6" } });
        let slots = json!({ "adSlots": [
            { "adSlotRenderer": {
                "adSlotMetadata": { "triggerEvent": "SLOT_TRIGGER_EVENT_BEFORE_CONTENT" },
                "fulfillmentContent": { "fulfilledLayout": { "playerBytesAdLayoutRenderer": {
                    "renderingContent": { "playerBytesSequentialLayoutRenderer": { "sequentialLayouts": [
                        { "playerBytesAdLayoutRenderer": { "renderingContent": content } }
                    ]}}
                }}}
            }}
        ]});
        assert_eq!(ad_wait(&slots), Duration::from_secs(6));
        assert_eq!(ad_wait(&json!({})), Duration::ZERO);
    }

    fn player(formats: Value) -> Value {
        json!({ "streamingData": { "adaptiveFormats": formats } })
    }

    #[test]
    fn selects_by_quality_and_parses_ciphers() {
        let p = player(json!([
            { "itag": 137, "mimeType": "video/mp4", "url": "https://x/v" },
            { "itag": 140, "mimeType": "audio/mp4; codecs=\"mp4a.40.2\"", "contentLength": "100",
              "signatureCipher": "s=SIGX&sp=sig&url=https%3A%2F%2Fr.googlevideo.com%2Fvideoplayback%3Fitag%3D140%26n%3DNNN%26c%3DTVHTML5" },
            { "itag": 251, "mimeType": "audio/webm; codecs=\"opus\"", "contentLength": "120", "loudnessDb": 2.5,
              "url": "https://r.googlevideo.com/videoplayback?itag=251&n=MMM" }
        ]));
        let normal = select_format(&p, Quality::Normal).unwrap();
        assert_eq!(normal.itag, 140);
        let (url, n, sig) = normal.cipher_parts().unwrap();
        assert!(url.starts_with("https://r.googlevideo.com/videoplayback?itag=140"));
        assert_eq!(n.as_deref(), Some("NNN"));
        assert_eq!(sig, Some(("SIGX".into(), "sig".into())));

        let high = select_format(&p, Quality::High).unwrap();
        assert_eq!((high.itag, high.loudness_db), (251, Some(2.5)));
        let (_, n, sig) = high.cipher_parts().unwrap();
        assert_eq!((n.as_deref(), sig), (Some("MMM"), None));

        assert!(select_format(&player(json!([])), Quality::Normal).is_err());
    }

    #[test]
    fn prefix_only_for_the_same_file() {
        let web = ResolvedStream {
            url: "https://r/videoplayback?itag=251&lmt=111&clen=9".into(),
            itag: 251,
            content_length: Some(9),
            ..Default::default()
        };
        let ios = ResolvedStream {
            url: "https://q/videoplayback?itag=251&lmt=111&c=IOS".into(),
            ..web.clone()
        };
        assert_eq!(same_file_prefix(&web, &ios).unwrap().url, ios.url);
        let other_encode = ResolvedStream {
            url: "https://q/videoplayback?itag=251&lmt=222".into(),
            ..ios.clone()
        };
        assert!(same_file_prefix(&web, &other_encode).is_none());
        let other_itag = ResolvedStream {
            itag: 140,
            ..ios.clone()
        };
        assert!(same_file_prefix(&web, &other_itag).is_none());
        let other_size = ResolvedStream {
            content_length: Some(8),
            ..ios
        };
        assert!(same_file_prefix(&web, &other_size).is_none());
    }

    #[test]
    fn bot_check_detection() {
        assert!(is_bot_check(
            "LOGIN_REQUIRED: Sign in to confirm you’re not a bot"
        ));
        assert!(!is_bot_check("UNPLAYABLE: Video unavailable"));
        // Age-restricted videos must not cost the shared session a refetch.
        assert!(!is_bot_check("LOGIN_REQUIRED: Sign in to confirm your age"));
        assert!(is_bot_check(
            "LOGIN_REQUIRED: Sign in to confirm you're not a bot"
        ));
        assert_eq!(
            reason(&Error::Unplayable(
                "ERROR: This video is unavailable".into()
            )),
            "ERROR: This video is unavailable"
        );
    }
}
