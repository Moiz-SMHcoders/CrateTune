//! [`WebResolver`]: full-length YouTube audio through a chain of InnerTube
//! clients ([`WebClient`]), fastest first, each behind a circuit breaker
//! ([`super::breaker`]).
//!
//! What a stream URL needs today (verified against googlevideo, Oct 2026; the
//! client definitions and the ad-wait rule follow yt-dlp, which is public domain):
//! * the visionOS client gets direct URLs from one `player` request, given a
//!   visitor id (YouTube issues one with its first refusal; it is kept on disk);
//! * the other clients send the visitor id and the player's `signatureTimestamp`;
//!   `n` (throttling) and, for ciphered formats, the signature are transformed
//!   with the player script's own functions ([`ChallengeSolver`]). A failed solve
//!   drops the cached player, fetches it again and solves once more (self-heal);
//! * PoToken clients additionally need `pot=<PoToken bound to the video id>` on
//!   the URL; the web client sends the same token in the player request, TV
//!   Simply one bound to the visitor id (with the video's, googlevideo refuses it);
//! * for those, googlevideo rejects the URL (403) until the pre-roll ad could have
//!   been skipped: [`ResolvedStream::not_before`] carries that time and the
//!   downloader waits for it.
//!
//! The signed player-fixes manifest ([`super::fixes`]) can change client
//! identities, order and player hints without an app update. `MUSIC_TRACE=1`
//! prints which client served each song.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};

use super::breaker::{Breakers, ResolverStatus, is_video_specific};
use super::client::{Identity, WebClient};
use super::fixes::{FixesStore, PlayerFixes};
use super::jsc::ChallengeSolver;
use super::potoken::PoTokenProvider;
use super::prefetch::ResolvedCache;
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
/// A forced check for new player fixes (after failures) at most this often.
const FORCED_FIXES_CHECK: Duration = Duration::from_secs(15 * 60);

/// Called with the resolver's status whenever it changes (see [`WebResolver::set_status_hook`]).
pub type StatusHook = Arc<dyn Fn(&ResolverStatus) + Send + Sync>;

/// Resolves through [`WebClient`]s in order, then an optional fallback resolver.
pub struct WebResolver {
    inner: Arc<Inner>,
}

struct Inner {
    clients: RwLock<Vec<WebClient>>,
    potoken: Option<Arc<dyn PoTokenProvider>>,
    fallback: Option<Arc<dyn StreamResolver>>,
    auth: RwLock<Option<Auth>>,
    session: Arc<SessionCache>,
    solver: Arc<ChallengeSolver>,
    visitor: VisitorStore,
    breakers: Breakers,
    fixes: RwLock<Arc<PlayerFixes>>,
    fixes_store: Mutex<Option<Arc<FixesStore>>>,
    fixes_checking: AtomicBool,
    fixes_forced_at: Mutex<Option<Instant>>,
    /// Resolved URLs for upcoming tracks ([`StreamResolver::prefetch_track`]).
    cache: ResolvedCache,
    prefetching: Mutex<HashSet<(String, Quality)>>,
    /// (client of the last full stream, whether the last song got a preview).
    last: Mutex<(Option<WebClient>, bool)>,
    hook: RwLock<Option<StatusHook>>,
    /// What the hook was last told (compared without the cooldown countdowns).
    reported: Mutex<Option<StatusKey>>,
    /// Tests replace the network part of [`Inner::resolve_client`].
    #[cfg(test)]
    fake: Option<FakeClient>,
}

#[cfg(test)]
type FakeClient = Box<dyn Fn(WebClient, &str) -> Result<ResolvedStream> + Send + Sync>;

impl WebResolver {
    /// `cache_dir` keeps preprocessed player scripts, the visionOS visitor id
    /// and the player-fixes manifest between runs (recommended).
    pub fn new(
        clients: Vec<WebClient>,
        potoken: Option<Arc<dyn PoTokenProvider>>,
        fallback: Option<Arc<dyn StreamResolver>>,
        cache_dir: Option<PathBuf>,
    ) -> Self {
        let fixes = PlayerFixes::bundled();
        let session = Arc::new(SessionCache::new(cache_dir.clone()));
        session.set_hints(fixes.players.clone());
        Self {
            inner: Arc::new(Inner {
                clients: RwLock::new(clients),
                potoken,
                fallback,
                auth: RwLock::new(None),
                session,
                solver: Arc::new(ChallengeSolver::new(cache_dir.clone())),
                visitor: VisitorStore::new(cache_dir.map(|d| d.join("visionos-visitor.txt"))),
                breakers: Breakers::default(),
                fixes: RwLock::new(Arc::new(fixes)),
                fixes_store: Mutex::new(None),
                fixes_checking: AtomicBool::new(false),
                fixes_forced_at: Mutex::new(None),
                cache: ResolvedCache::default(),
                prefetching: Mutex::default(),
                last: Mutex::new((None, false)),
                hook: RwLock::new(None),
                reported: Mutex::new(None),
                #[cfg(test)]
                fake: None,
            }),
        }
    }

    /// Clients to try, in order (empty: only the fallback). The player-fixes
    /// manifest may reorder them or skip some (never all).
    pub fn set_clients(&self, clients: Vec<WebClient>) {
        *self
            .inner
            .clients
            .write()
            .unwrap_or_else(PoisonError::into_inner) = clients;
        self.inner.cache.clear();
    }

    pub fn clients(&self) -> Vec<WebClient> {
        self.inner.clients()
    }

    /// Account cookies for the player request (signed-in playback), or none.
    pub fn set_auth(&self, auth: Option<Auth>) {
        *self
            .inner
            .auth
            .write()
            .unwrap_or_else(PoisonError::into_inner) = auth.filter(Auth::is_signed_in);
        self.inner.cache.clear();
    }

    pub fn has_potoken(&self) -> bool {
        self.inner.potoken.is_some()
    }

    /// Called (on a resolver thread) whenever [`Self::status`] changes: another
    /// client served a song, a client's breaker tripped or closed, new player
    /// fixes were applied.
    pub fn set_status_hook(&self, hook: impl Fn(&ResolverStatus) + Send + Sync + 'static) {
        *self
            .inner
            .hook
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(hook));
    }

    /// Which client served the last song, which ones are failing, which fixes apply.
    pub fn status(&self) -> ResolverStatus {
        self.inner.status()
    }

    /// The player fixes in use (the built-in ones, or a newer fetched manifest).
    pub fn player_fixes(&self) -> Arc<PlayerFixes> {
        self.inner.fixes()
    }

    /// Uses `fixes` from now on (already verified; see [`super::fixes`]).
    pub fn set_player_fixes(&self, fixes: PlayerFixes) {
        self.inner.apply_fixes(fixes);
    }

    /// Where newer player fixes come from (`None`: only the built-in ones).
    /// Applies the cached manifest if it is newer and, when the last check is
    /// old, checks for a new one in the background.
    pub fn use_player_fixes(&self, store: Option<FixesStore>) {
        let store = store.map(Arc::new);
        *self
            .inner
            .fixes_store
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = store.clone();
        match store {
            Some(store) => {
                if let Some(cached) = store.cached()
                    && cached.serial > self.inner.fixes().serial
                {
                    self.inner.apply_fixes(cached);
                }
                self.inner.check_fixes(false);
            }
            None => self.inner.apply_fixes(PlayerFixes::bundled()),
        }
    }

    /// Checks for new player fixes now (in the background; rate-limited).
    pub fn check_player_fixes(&self) {
        self.inner.check_fixes(true);
    }

    /// Does the slow one-time work up front so the first song starts quickly:
    /// the visionOS visitor id, the session, the preprocessed player script, and
    /// (if a PoToken client is enabled) BotGuard. Blocking, a few seconds; call
    /// it from a background thread (never the main thread on macOS). Errors are
    /// only logged: resolving retries everything on demand.
    pub fn prewarm(&self) {
        self.inner.prewarm();
    }
}

impl Inner {
    fn clients(&self) -> Vec<WebClient> {
        self.clients
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The enabled clients as the player fixes arrange them.
    fn effective_clients(&self) -> Vec<WebClient> {
        self.fixes().arrange(&self.clients())
    }

    fn fixes(&self) -> Arc<PlayerFixes> {
        self.fixes
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn identity(&self, client: WebClient) -> Identity {
        client.identity(&self.fixes().clients)
    }

    fn apply_fixes(&self, fixes: PlayerFixes) {
        log::info!(
            "youtube: player fixes {} ({}) in use",
            fixes.serial,
            fixes.issued
        );
        self.session.set_hints(fixes.players.clone());
        *self.fixes.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(fixes);
        self.cache.clear();
        self.report();
    }

    /// Fetches newer player fixes in the background: when due (`force`: now,
    /// at most every [`FORCED_FIXES_CHECK`]).
    fn check_fixes(self: &Arc<Self>, force: bool) {
        let Some(store) = self
            .fixes_store
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        else {
            return;
        };
        if force {
            let mut at = self
                .fixes_forced_at
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if at.is_some_and(|t| t.elapsed() < FORCED_FIXES_CHECK) {
                return;
            }
            *at = Some(Instant::now());
        } else if !store.due(SystemTime::now()) {
            return;
        }
        if self.fixes_checking.swap(true, Ordering::AcqRel) {
            return;
        }
        let inner = self.clone();
        let spawned = std::thread::Builder::new()
            .name("music-core-player-fixes".into())
            .spawn(move || {
                match store.fetch(inner.fixes().serial) {
                    Ok(Some(fixes)) => inner.apply_fixes(fixes),
                    Ok(None) => {}
                    Err(e) => log::info!("youtube: no player fixes fetched: {e}"),
                }
                inner.fixes_checking.store(false, Ordering::Release);
            });
        if spawned.is_err() {
            self.fixes_checking.store(false, Ordering::Release);
        }
    }

    fn status(&self) -> ResolverStatus {
        let (last_client, last_was_preview) =
            *self.last.lock().unwrap_or_else(PoisonError::into_inner);
        let fixes = self.fixes();
        ResolverStatus {
            last_client,
            last_was_preview,
            clients: self.breakers.health(Instant::now()),
            fixes_serial: (fixes.serial > PlayerFixes::bundled_serial()).then_some(fixes.serial),
        }
    }

    /// Tells the status hook, if the status changed.
    fn report(&self) {
        let status = self.status();
        let key = StatusKey::of(&status);
        {
            let mut reported = self.reported.lock().unwrap_or_else(PoisonError::into_inner);
            if reported.as_ref() == Some(&key) {
                return;
            }
            *reported = Some(key);
        }
        let hook = self
            .hook
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(hook) = hook {
            hook(&status);
        }
    }

    fn note_result(&self, client: Option<WebClient>, preview: bool) {
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = (client, preview);
        self.report();
    }

    fn prewarm(self: &Arc<Self>) {
        // Any public video works for the watch page; this is YouTube's first one.
        const ANY_VIDEO: &str = "jNQXAC9IVRw";
        let clients = self.effective_clients();
        if clients.is_empty() {
            return;
        }
        let started = Instant::now();
        if clients.contains(&WebClient::VisionOs) {
            // Earns the visitor id the visionOS client needs (one or two requests).
            match self.vision_player(ANY_VIDEO) {
                Ok(_) => log::info!("youtube: visionOS ready in {:?}", started.elapsed()),
                Err(e) => log::warn!("youtube: visionOS prewarm failed: {e}"),
            }
        }
        let warm = || -> Result<()> {
            if clients.iter().any(|c| c.needs_player()) {
                let mut session = self.session.get(ANY_VIDEO)?;
                preprocess(&self.session, &self.solver, &mut session)?;
            }
            if clients.iter().any(|c| c.needs_potoken())
                && let Some(provider) = &self.potoken
            {
                provider.content_token(ANY_VIDEO)?;
            }
            Ok(())
        };
        match warm() {
            Ok(()) => log::info!("youtube: stream resolver ready in {:?}", started.elapsed()),
            Err(e) => log::warn!("youtube: prewarm failed (will retry on demand): {e}"),
        }
    }

    /// Tries the clients in order (skipping those whose breaker is open, never
    /// the last), then the fallback. `preview`: fetch the iOS client's head
    /// meanwhile, for clients with an ad wait (not for prefetches).
    fn resolve_chain(
        self: &Arc<Self>,
        video_id: &str,
        quality: Quality,
        preview: bool,
    ) -> Result<ResolvedStream> {
        self.renew_potoken_in_background();
        let clients = self.effective_clients();
        let last_index = clients.len().saturating_sub(1);
        let mut last = None;
        let mut route_failed = false;
        for (i, client) in clients.into_iter().enumerate() {
            let now = Instant::now();
            if i < last_index && self.breakers.should_skip(client, now) {
                trace(format_args!(
                    "{video_id}: {} skipped (failed {} times in a row)",
                    client.label(),
                    super::breaker::FAILURES_BEFORE_TRIP
                ));
                continue;
            }
            match self.resolve_client(client, video_id, quality, preview) {
                Ok(stream) => {
                    let ms = now.elapsed().as_millis();
                    trace(format_args!("{video_id}: {} in {ms} ms", client.label()));
                    if self.breakers.record_success(client) {
                        log::info!("youtube: {} works again", client.label());
                    }
                    self.note_result(Some(client), false);
                    return Ok(stream);
                }
                Err(e) => {
                    log::warn!("youtube: {} failed for {video_id}: {e}", client.label());
                    trace(format_args!(
                        "{video_id}: {} failed after {} ms: {e}",
                        client.label(),
                        now.elapsed().as_millis()
                    ));
                    if client.needs_potoken()
                        && let Some(p) = &self.potoken
                        && matches!(e, Error::Audio(_))
                    {
                        // A rejected token or a broken page: start BotGuard over next time.
                        p.reset();
                    }
                    if !is_video_specific(&e) {
                        route_failed = true;
                        if self
                            .breakers
                            .record_failure(client, &reason(&e), Instant::now())
                        {
                            log::warn!(
                                "youtube: {} failed {} times in a row; skipping it for {} min",
                                client.label(),
                                super::breaker::FAILURES_BEFORE_TRIP,
                                super::breaker::COOLDOWN.as_secs() / 60
                            );
                        }
                        self.report();
                    }
                    last = Some(e);
                }
            }
        }
        if route_failed {
            // Maybe YouTube changed something the published fixes already cover.
            self.check_fixes(true);
        }
        let result = match &self.fallback {
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
        };
        if result.is_ok() {
            trace(format_args!("{video_id}: preview fallback"));
            self.note_result(None, true);
        }
        result
    }

    fn resolve_client(
        &self,
        client: WebClient,
        video_id: &str,
        quality: Quality,
        preview: bool,
    ) -> Result<ResolvedStream> {
        #[cfg(test)]
        if let Some(fake) = &self.fake {
            return fake(client, video_id);
        }
        if !client.needs_player() {
            return self.resolve_vision(video_id, quality);
        }
        let web = || {
            with_decipher_retry(
                || self.resolve_web(client, video_id, quality),
                |player_id| self.heal_player(player_id),
            )
        };
        if !preview {
            return web();
        }
        // The iOS client's preview URL is fetched meanwhile: if it's the same file,
        // its first MiB lets playback start before the web URL becomes usable.
        // Its head starts downloading as soon as the iOS client answers, while
        // the web client is still busy (player request, challenges), so the
        // first bytes are usually in by the time the web URL is confirmed.
        std::thread::scope(|scope| {
            let started = Instant::now();
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
            let mut stream = web()?;
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

    /// The self-heal after a failed solve: forget the player (solver and
    /// session), so the retry downloads and preprocesses it again; and look for
    /// new player fixes.
    fn heal_player(&self, player_id: &str) {
        self.solver.forget(player_id);
        self.session.forget_player(player_id);
        self.cache.clear();
    }

    /// Renews the PoToken attestation on a background thread when it ends within
    /// 30 minutes, so the ~1 s re-attestation never delays a song.
    fn renew_potoken_in_background(&self) {
        const RENEW_WITHIN: Duration = Duration::from_secs(30 * 60);
        let Some(provider) = self.potoken.clone() else {
            return;
        };
        if !self.clients().iter().any(|c| c.needs_potoken())
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
                let started = Instant::now();
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

    /// The visionOS `player` response for `video_id` (status OK), earning a
    /// visitor id first if YouTube asks for one.
    fn vision_player(&self, video_id: &str) -> Result<Value> {
        let client = WebClient::VisionOs;
        let identity = self.identity(client);
        let held = self.visitor.get();
        let mut player = self.player_request(
            client,
            &identity,
            video_id,
            None,
            None,
            held.as_deref(),
            None,
        )?;
        if status_of(&player).0 == "LOGIN_REQUIRED"
            && let Some(issued) = player
                .pointer("/responseContext/visitorData")
                .and_then(Value::as_str)
                .filter(|issued| Some(*issued) != held.as_deref())
                .map(str::to_owned)
        {
            let retried =
                self.player_request(client, &identity, video_id, None, None, Some(&issued), None)?;
            // Kept only when it worked: an age gate answers LOGIN_REQUIRED too,
            // and no visitor fixes that.
            if status_of(&retried).0 == "OK" {
                log::info!("youtube: visionOS: using the visitor id YouTube issued");
                self.visitor.adopt(&issued);
            }
            player = retried;
        }
        playable(&player)?;
        Ok(player)
    }

    /// visionOS: direct URLs, nothing to solve, no ad wait.
    fn resolve_vision(&self, video_id: &str, quality: Quality) -> Result<ResolvedStream> {
        let started = Instant::now();
        let player = self.vision_player(video_id)?;
        let format = select_format(&player, quality)?;
        let url = format
            .url
            .clone()
            .ok_or_else(|| Error::Parse("visionOS: the format is ciphered".into()))?;
        let user_agent = self.identity(WebClient::VisionOs).user_agent();
        // googlevideo now and then refuses a URL `player` just handed out; found
        // here, the chain moves on (in the player, the song would just fail).
        // The connection is the one the first range then reuses.
        probe(&url, user_agent)?;
        let mut source = StreamSource::youtube(format.itag);
        source.variant = Some(WebClient::VisionOs.label().to_owned());
        log::info!(
            "youtube: {video_id} itag {} via visionOS in {:?}",
            format.itag,
            started.elapsed()
        );
        Ok(ResolvedStream {
            url,
            itag: format.itag,
            mime_type: format.mime_type,
            content_length: format.content_length,
            user_agent,
            loudness_db: format.loudness_db,
            source: Some(source),
            // Ranged fetches are served at full speed (one plain GET is throttled).
            chunk_size: Some(WEB_CHUNK_BYTES),
            full_access: true,
            ..Default::default()
        })
    }

    fn resolve_web(
        &self,
        client: WebClient,
        video_id: &str,
        quality: Quality,
    ) -> std::result::Result<ResolvedStream, Attempt> {
        let started = Instant::now();
        let identity = self.identity(client);
        let mut session = self.session.get(video_id)?;
        // Load the player into the solver while the player request is in flight.
        self.solver.prepare(&session.player_id);
        self.refresh_session_in_background(video_id);
        log::debug!("youtube: {video_id}: session after {:?}", started.elapsed());
        let pot = if client.needs_potoken() {
            Some(
                self.potoken
                    .as_ref()
                    .ok_or_else(|| Error::Unplayable("no PoToken provider".into()))?
                    .content_token(video_id)?,
            )
        } else {
            None
        };
        // The TV client plays the account's own uploads and Premium songs.
        let auth = (client == WebClient::Tv || client == WebClient::MobileWeb)
            .then(|| {
                self.auth
                    .read()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone()
            })
            .flatten();
        let request = |session: &WebSession| -> Result<Value> {
            let visitor = auth
                .as_ref()
                .and_then(|a| a.visitor_data.clone())
                .unwrap_or_else(|| session.visitor_data.clone());
            // TV Simply's request carries a session token (bound to the visitor
            // id); its URL, like the web client's, the one bound to the video.
            let request_pot = match (&pot, &self.potoken) {
                (Some(_), Some(provider)) if client == WebClient::TvSimply => {
                    Some(provider.content_token(&session.visitor_data)?)
                }
                _ => pot.clone(),
            };
            let player = self.player_request(
                client,
                &identity,
                video_id,
                Some(session.sts),
                request_pot.as_deref(),
                Some(&visitor),
                auth.as_ref(),
            )?;
            playable(&player)?;
            Ok(player)
        };
        let player = match request(&session) {
            Err(Error::Unplayable(reason)) if is_bot_check(&reason) => {
                // A stale visitor id is the usual cause: start a fresh session once.
                self.session.invalidate();
                session = self.session.get(video_id)?;
                request(&session)?
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
        let decipher = |error: Error| Attempt::Decipher {
            player_id: session.player_id.clone(),
            error,
        };
        let solutions = self
            .solver
            .solve(
                &session.player_id,
                script,
                n.iter().cloned().collect(),
                sig.iter().map(|(s, _)| s.clone()).collect(),
            )
            .map_err(decipher)?;
        if had_script {
            // Preprocessed now; the full script isn't needed again.
            self.session.drop_script(&session.player_id);
        }
        log::debug!(
            "youtube: {video_id}: challenges solved after {:?}",
            started.elapsed()
        );
        if let Some(n) = &n {
            match solutions.n.get(n) {
                Some(solved) if n_solution_ok(n, solved) => {}
                Some(solved) => {
                    return Err(decipher(Error::Parse(format!(
                        "challenge solver returned an unusable n ({solved:.24})"
                    ))));
                }
                None => {
                    return Err(decipher(Error::Parse(
                        "challenge solver returned no n solution".into(),
                    )));
                }
            }
        }
        let mut url = url::Url::parse(&url)
            .map_err(|e| Attempt::Other(Error::Parse(format!("stream URL: {e}"))))?;
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
        {
            let mut query = url.query_pairs_mut();
            query.clear().extend_pairs(&pairs);
            if let Some((s, param)) = &sig {
                let solved = solutions.sig.get(s).ok_or_else(|| {
                    decipher(Error::Parse(
                        "challenge solver returned no signature".into(),
                    ))
                })?;
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

    /// One `player` request; the response whatever its playability status.
    #[allow(clippy::too_many_arguments)]
    fn player_request(
        &self,
        client: WebClient,
        identity: &Identity,
        video_id: &str,
        sts: Option<u32>,
        pot: Option<&str>,
        visitor: Option<&str>,
        auth: Option<&Auth>,
    ) -> Result<Value> {
        let mut client_ctx = identity.context(client);
        client_ctx["hl"] = json!("en");
        client_ctx["timeZone"] = json!("UTC");
        client_ctx["utcOffsetMinutes"] = json!(0);
        let mut content = json!({ "html5Preference": "HTML5_PREF_WANTS" });
        if let Some(sts) = sts {
            content["signatureTimestamp"] = json!(sts);
        }
        let mut body = json!({
            "context": { "client": client_ctx },
            "videoId": video_id,
            "playbackContext": { "contentPlaybackContext": content },
            "contentCheckOk": true,
            "racyCheckOk": true,
        });
        if client == WebClient::VisionOs {
            body["context"]["client"]["gl"] = json!("US");
            if let Some(visitor) = visitor {
                body["context"]["client"]["visitorData"] = json!(visitor);
            }
            body["context"]["user"] = json!({ "lockedSafetyMode": false });
            body["context"]["request"] = json!({ "useSsl": true });
        }
        if let Some(pot) = pot {
            body["serviceIntegrityDimensions"] = json!({ "poToken": pot });
        }
        let mut req = agent()
            .post("https://www.youtube.com/youtubei/v1/player?prettyPrint=false")
            .header("User-Agent", identity.user_agent())
            .header("Content-Type", "application/json")
            .header("X-Youtube-Client-Name", client.client_id())
            .header("X-Youtube-Client-Version", identity.version())
            .header("Origin", client.origin());
        if let Some(visitor) = visitor {
            req = req.header("X-Goog-Visitor-Id", visitor);
        }
        if let Some(auth) = auth {
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
        if let Some(auth) = auth {
            // Rotated session cookies, as for every signed-in InnerTube response.
            let lines = resp
                .headers()
                .get_all(ureq::http::header::SET_COOKIE)
                .iter()
                .filter_map(|v| v.to_str().ok());
            auth.absorb_set_cookies("https://www.youtube.com", false, lines);
        }
        Ok(resp.body_mut().read_json()?)
    }

    /// Resolves `track` for later ([`StreamResolver::prefetch_track`]), on a
    /// background thread; a no-op if a fresh URL is cached or being resolved.
    fn prefetch(self: &Arc<Self>, video_id: &str, quality: Quality) {
        if self.clients().is_empty() || self.cache.has_fresh(video_id, quality, SystemTime::now()) {
            return;
        }
        let key = (video_id.to_owned(), quality);
        if !self
            .prefetching
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.clone())
        {
            return;
        }
        let inner = self.clone();
        let spawned = std::thread::Builder::new()
            .name("music-core-prefetch-url".into())
            .spawn(move || {
                let (video_id, quality) = &key;
                match inner.resolve_chain(video_id, *quality, false) {
                    // A preview is no head start: the real resolve may do better.
                    Ok(stream) if stream.full_access => {
                        log::debug!("youtube: {video_id}: next track's URL resolved ahead");
                        inner
                            .cache
                            .put(video_id, *quality, stream, SystemTime::now());
                    }
                    Ok(_) => {}
                    Err(e) => log::debug!("youtube: {video_id}: prefetch failed: {e}"),
                }
                inner
                    .prefetching
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&key);
            });
        if spawned.is_err() {
            self.prefetching
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clear();
        }
    }
}

impl StreamResolver for WebResolver {
    fn resolve(&self, video_id: &str) -> Result<ResolvedStream> {
        self.resolve_with_quality(video_id, Quality::Normal)
    }

    fn resolve_with_quality(&self, video_id: &str, quality: Quality) -> Result<ResolvedStream> {
        if let Some(stream) = self.inner.cache.take(video_id, quality, SystemTime::now()) {
            trace(format_args!("{video_id}: URL resolved ahead"));
            return Ok(stream);
        }
        self.inner.resolve_chain(video_id, quality, true)
    }

    /// Never from the prefetch cache: `stale` stopped working.
    fn refresh_track(
        &self,
        track: &crate::Track,
        quality: Quality,
        stale: &ResolvedStream,
    ) -> Result<ResolvedStream> {
        let _ = stale;
        self.inner.resolve_chain(&track.video_id, quality, true)
    }

    fn prefetch_track(&self, track: &crate::Track, quality: Quality) {
        if !track.is_local() {
            self.inner.prefetch(&track.video_id, quality);
        }
    }
}

/// What the status hook compares (the status without its countdowns).
#[derive(Debug, PartialEq, Eq)]
struct StatusKey {
    last: (Option<WebClient>, bool),
    clients: Vec<(WebClient, u32, bool)>,
    fixes: Option<u64>,
}

impl StatusKey {
    fn of(s: &ResolverStatus) -> Self {
        Self {
            last: (s.last_client, s.last_was_preview),
            clients: s
                .clients
                .iter()
                .map(|h| (h.client, h.consecutive_failures, h.skipped_for.is_some()))
                .collect(),
            fixes: s.fixes_serial,
        }
    }
}

/// The visionOS client's visitor id, kept on disk so the handshake that earns
/// one is paid once, not every launch.
struct VisitorStore {
    current: Mutex<Option<String>>,
    file: Option<PathBuf>,
}

impl VisitorStore {
    fn new(file: Option<PathBuf>) -> Self {
        let saved = file
            .as_ref()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty() && s.len() < 4096);
        Self {
            current: Mutex::new(saved),
            file,
        }
    }

    fn get(&self) -> Option<String> {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn adopt(&self, visitor: &str) {
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = Some(visitor.to_owned());
        if let Some(file) = &self.file {
            if let Some(dir) = file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(file, visitor);
        }
    }
}

/// How one resolve through a JS client failed.
enum Attempt {
    /// Solving the player's challenges failed: worth one retry with the player
    /// fetched again.
    Decipher {
        player_id: String,
        error: Error,
    },
    Other(Error),
}

impl From<Error> for Attempt {
    fn from(e: Error) -> Self {
        Attempt::Other(e)
    }
}

/// Runs `attempt`; if solving failed, calls `heal` with the player id (drop the
/// cached player) and tries exactly once more.
fn with_decipher_retry<T>(
    mut attempt: impl FnMut() -> std::result::Result<T, Attempt>,
    mut heal: impl FnMut(&str),
) -> Result<T> {
    match attempt() {
        Ok(v) => Ok(v),
        Err(Attempt::Other(e)) => Err(e),
        Err(Attempt::Decipher { player_id, error }) => {
            log::warn!(
                "youtube: solving player {player_id} failed ({error}); fetching it again and retrying once"
            );
            trace(format_args!("player {player_id}: solve failed, self-heal"));
            heal(&player_id);
            match attempt() {
                Ok(v) => {
                    log::info!("youtube: the self-heal worked");
                    Ok(v)
                }
                Err(Attempt::Other(e) | Attempt::Decipher { error: e, .. }) => Err(e),
            }
        }
    }
}

/// A plausible `n` solution: changed, and not the solver's exception marker.
fn n_solution_ok(challenge: &str, solved: &str) -> bool {
    !solved.is_empty() && solved != challenge && !solved.starts_with("enhanced_except")
}

/// Asks googlevideo for the first byte of `url`.
fn probe(url: &str, user_agent: &str) -> Result<()> {
    let resp = agent()
        .get(url)
        .header("User-Agent", user_agent)
        .header("Range", "bytes=0-0")
        .config()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .call()?;
    match resp.status().as_u16() {
        200 | 206 => Ok(()),
        code => Err(Error::Unplayable(format!(
            "the stream answered HTTP {code}"
        ))),
    }
}

/// The playability status and reason of a `player` response.
fn status_of(player: &Value) -> (&str, &str) {
    (
        player
            .pointer("/playabilityStatus/status")
            .and_then(Value::as_str)
            .unwrap_or("?"),
        player
            .pointer("/playabilityStatus/reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown reason"),
    )
}

fn playable(player: &Value) -> Result<()> {
    match status_of(player) {
        ("OK", _) => Ok(()),
        (status, reason) => Err(Error::Unplayable(format!("{status}: {reason}"))),
    }
}

/// `MUSIC_TRACE=1`: one line per resolve on stderr (which client, how long).
fn trace(args: std::fmt::Arguments<'_>) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("MUSIC_TRACE").is_some()) {
        eprintln!("music-core: youtube {args}");
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

    // ---- the client chain (network replaced by a fake) ----

    use std::sync::atomic::AtomicUsize;

    struct Preview;
    impl StreamResolver for Preview {
        fn resolve(&self, _: &str) -> Result<ResolvedStream> {
            Ok(ResolvedStream {
                url: "https://preview".into(),
                ..Default::default()
            })
        }
    }

    fn full(url: &str) -> ResolvedStream {
        ResolvedStream {
            url: url.into(),
            itag: 251,
            full_access: true,
            ..Default::default()
        }
    }

    type Calls = Arc<Mutex<Vec<WebClient>>>;

    /// A resolver over `clients` whose network is `answer`; records every call.
    fn chain(
        clients: Vec<WebClient>,
        answer: impl Fn(WebClient, &str) -> Result<ResolvedStream> + Send + Sync + 'static,
    ) -> (WebResolver, Calls) {
        let mut r = WebResolver::new(clients, None, Some(Arc::new(Preview)), None);
        let calls: Calls = Arc::default();
        let seen = calls.clone();
        Arc::get_mut(&mut r.inner).unwrap().fake = Some(Box::new(move |c, id| {
            seen.lock().unwrap().push(c);
            answer(c, id)
        }));
        (r, calls)
    }

    fn count(calls: &Calls, client: WebClient) -> usize {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| **c == client)
            .count()
    }

    const BOT: &str = "LOGIN_REQUIRED: Sign in to confirm you’re not a bot";

    #[test]
    fn clients_are_tried_fastest_first_and_a_failing_one_is_skipped() {
        let (r, calls) = chain(WebClient::automatic(false), |c, _| match c {
            WebClient::VisionOs => Err(Error::Unplayable(BOT.into())),
            _ => Ok(full("https://tv")),
        });
        let statuses: Arc<Mutex<Vec<ResolverStatus>>> = Arc::default();
        let seen = statuses.clone();
        r.set_status_hook(move |s| seen.lock().unwrap().push(s.clone()));
        for _ in 0..6 {
            assert_eq!(r.resolve("x").unwrap().url, "https://tv");
        }
        // visionOS first, until its breaker tripped after 3 failures in a row.
        assert_eq!(
            calls.lock().unwrap()[..2],
            [WebClient::VisionOs, WebClient::Tv]
        );
        assert_eq!(
            count(&calls, WebClient::VisionOs),
            super::super::breaker::FAILURES_BEFORE_TRIP as usize
        );
        assert_eq!(count(&calls, WebClient::Tv), 6);
        let status = r.status();
        assert_eq!(status.last_client, Some(WebClient::Tv));
        assert!(!status.last_was_preview);
        let skipped: Vec<_> = status.skipped().map(|h| h.client).collect();
        assert_eq!(skipped, [WebClient::VisionOs]);
        // The hook heard about the trip, and isn't called for nothing new.
        let heard = statuses.lock().unwrap();
        assert!(heard.iter().any(|s| s.skipped().count() == 1));
        assert!(heard.len() <= 5, "{} reports", heard.len());
    }

    #[test]
    fn video_specific_failures_do_not_trip_the_breaker() {
        let (r, calls) = chain(WebClient::automatic(false), |c, _| match c {
            WebClient::VisionOs => Err(Error::Unplayable(
                "UNPLAYABLE: This video is not available".into(),
            )),
            _ => Ok(full("https://tv")),
        });
        for _ in 0..10 {
            r.resolve("x").unwrap();
        }
        assert_eq!(count(&calls, WebClient::VisionOs), 10);
        assert!(r.status().clients.is_empty());
    }

    #[test]
    fn the_last_client_is_never_skipped_and_the_preview_is_the_net() {
        let (r, calls) = chain(vec![WebClient::Tv], |_, _| {
            Err(Error::Parse("challenge solver: boom".into()))
        });
        for _ in 0..5 {
            assert_eq!(r.resolve("x").unwrap().url, "https://preview");
        }
        assert_eq!(count(&calls, WebClient::Tv), 5);
        let status = r.status();
        assert!(status.last_was_preview && status.last_client.is_none());
        assert_eq!(status.clients[0].consecutive_failures, 5);
    }

    #[test]
    fn the_manifest_reorders_and_disables_clients() {
        let (r, calls) = chain(WebClient::automatic(false), |_, _| Ok(full("https://ok")));
        let fixes = super::super::fixes::parse(
            br#"{"schema":1,"serial":99,"order":["TVHTML5"],"disabled":["VISIONOS"]}"#,
        )
        .unwrap();
        r.set_player_fixes(fixes);
        r.resolve("x").unwrap();
        assert_eq!(*calls.lock().unwrap(), [WebClient::Tv]);
        assert_eq!(r.status().fixes_serial, Some(99));
        // Back to the built-in data.
        r.use_player_fixes(None);
        r.resolve("x").unwrap();
        assert_eq!(calls.lock().unwrap()[1], WebClient::VisionOs);
        assert_eq!(r.status().fixes_serial, None);
    }

    #[test]
    fn the_next_tracks_url_is_resolved_ahead_and_refreshed_when_stale() {
        let n = Arc::new(AtomicUsize::new(0));
        let counter = n.clone();
        let (r, _) = chain(vec![WebClient::VisionOs], move |_, id| {
            let i = counter.fetch_add(1, Ordering::SeqCst);
            Ok(full(&format!("https://{id}/{i}")))
        });
        let track = crate::Track {
            video_id: "next".into(),
            ..Default::default()
        };
        r.prefetch_track(&track, Quality::High);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !r
            .inner
            .cache
            .has_fresh("next", Quality::High, SystemTime::now())
        {
            assert!(Instant::now() < deadline, "prefetch never finished");
            std::thread::sleep(Duration::from_millis(5));
        }
        r.prefetch_track(&track, Quality::High);
        // Served from the cache (no second resolve), once.
        let first = r.resolve_track(&track, Quality::High).unwrap();
        assert_eq!(first.url, "https://next/0");
        assert_eq!(n.load(Ordering::SeqCst), 1);
        // A stale stream is never handed out again: refresh resolves anew.
        let fresh = r.refresh_track(&track, Quality::High, &first).unwrap();
        assert_eq!(fresh.url, "https://next/1");
        // Another quality isn't served from it.
        assert_eq!(
            r.resolve_track(&track, Quality::Low).unwrap().url,
            "https://next/2"
        );
        // Local files aren't prefetched.
        let local = crate::Track {
            video_id: "local:1".into(),
            local_path: Some("/x.mp3".into()),
            ..Default::default()
        };
        r.prefetch_track(&local, Quality::High);
        assert!(r.inner.prefetching.lock().unwrap().is_empty());
    }

    #[test]
    fn a_failed_solve_drops_the_player_and_retries_once() {
        let decipher = |id: &str| Attempt::Decipher {
            player_id: id.into(),
            error: Error::Parse("challenge solver: no n function".into()),
        };
        // Fails, heals, works.
        let mut tries = 0;
        let mut healed = Vec::new();
        let got = with_decipher_retry(
            || {
                tries += 1;
                if tries == 1 {
                    Err(decipher("old"))
                } else {
                    Ok(tries)
                }
            },
            |p| healed.push(p.to_owned()),
        );
        assert_eq!(got.unwrap(), 2);
        assert_eq!(healed, ["old"]);

        // Fails twice: one retry only.
        let (mut tries, mut heals) = (0, 0);
        let got: Result<()> = with_decipher_retry(
            || {
                tries += 1;
                Err(decipher("p"))
            },
            |_| heals += 1,
        );
        assert!(got.is_err());
        assert_eq!((tries, heals), (2, 1));

        // Other failures aren't retried.
        let (mut tries, mut heals) = (0, 0);
        let got: Result<()> = with_decipher_retry(
            || {
                tries += 1;
                Err(Attempt::Other(Error::Unplayable("x".into())))
            },
            |_| heals += 1,
        );
        assert!(got.is_err());
        assert_eq!((tries, heals), (1, 0));
    }

    #[test]
    fn unusable_n_solutions() {
        assert!(n_solution_ok("abcdef", "XyZ12"));
        assert!(!n_solution_ok("abcdef", "abcdef"));
        assert!(!n_solution_ok("abcdef", "enhanced_except_AAAA_abcdef"));
        assert!(!n_solution_ok("abcdef", ""));
    }

    #[test]
    fn visitor_is_kept_on_disk() {
        let dir = std::env::temp_dir().join(format!("music-core-visitor-{}", std::process::id()));
        let file = dir.join("visionos-visitor.txt");
        let store = VisitorStore::new(Some(file.clone()));
        assert_eq!(store.get(), None);
        store.adopt("Cgt123");
        assert_eq!(
            VisitorStore::new(Some(file)).get().as_deref(),
            Some("Cgt123")
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
