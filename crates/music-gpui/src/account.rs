//! YouTube Music account: sign-in with the browser cookie (pasted, or captured by
//! the embedded login window on macOS, see `login_webview.rs`), channel / Google
//! account selection, sign-out, and syncing the account's library (liked songs,
//! saved songs, albums, subscriptions, playlists) into the local database.
//!
//! The cookie is a secret (keychain, `Secret::YouTubeCookie`); the account's
//! name, avatar, `auth_user` and selected channel live in `settings.library.account`.
//!
//! **Google accounts of one login.** A browser session (and so its cookie) can hold
//! several Google accounts, picked per request with `X-Goog-AuthUser` (0, 1, …).
//! Which ones exist is found by asking `account/account_menu` for each index in
//! turn ([`InnerTube::google_accounts`]) at sign-in, at startup and on every sync,
//! in the background; Settings → Account only offers those. A switch is applied only
//! after the new account answered: on failure the previous `auth_user` / channel stay
//! (in memory and in the settings file). A saved index that is no longer signed in
//! (left over from an older version) is reset to the first account at startup.
//!
//! **Session.** One `music_core::innertube::Session` per sign-in is attached to every
//! `Auth` handed out ([`Account::auth`]): rotated cookies (`Set-Cookie`) and renewed
//! ones (silent re-login) reach every client at once, and are written back to the
//! keychain, throttled (`session_keeper.rs`). A value written back is the same login,
//! not a new sign-in ([`Account::set_cookie`]).
//!
//! **Not verified against a real account** in this round: every request here is
//! music-core's cookie layer, whose request bodies are unit-tested but which was
//! never run signed-in. Errors are shown in the Account section.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{Context, Window};
use music_core::innertube::{AccountChannel, Auth, GoogleAccount, InnerTube, Item, Session};
use music_core::library::{
    AlbumMeta, AlbumQuery, ArtistMeta, ArtistQuery, Library, PlaylistSort, RemotePlaylist,
    SongQuery, SongScope, now_ms,
};
use music_core::{Result, Track};

use crate::app::{MusicApp, notify};
use crate::settings::library_config::AccountConfig;
use crate::util::off_thread;
use crate::youtube_sync::{SyncKind, SyncRequest, Trigger};

/// Continuation pages followed per list (≈ 100 items each).
const MAX_PAGES: usize = 60;

#[derive(Debug, Clone, Default, PartialEq)]
pub enum Busy {
    #[default]
    Idle,
    Working(String),
    Failed(String),
    Done(String),
}

pub struct Account {
    pub config: Option<AccountConfig>,
    /// The cookie this login started with (its identity; requests use `session`'s
    /// current one).
    cookie: Option<String>,
    /// The live session of `cookie` (rotation, renewal, expiry).
    session: Option<Arc<Session>>,
    /// YouTube rejected the session and a background renewal failed: "Sign in again".
    pub expired: bool,
    /// Where new sessions report to (`session_keeper.rs`); `None` in tests.
    pub(crate) hooks: Option<Arc<dyn music_core::innertube::SessionHooks>>,
    pub channels: Vec<AccountChannel>,
    /// Google accounts of the cookie's browser session; `None` until probed.
    pub google_accounts: Option<Vec<GoogleAccount>>,
    /// A probe for [`Account::google_accounts`] is running.
    probing: bool,
    /// Sign-in / account refresh.
    pub status: Busy,
    /// Library sync.
    pub sync: Busy,
    /// Automatic syncs, the sync guard and per-song likes (`youtube_sync.rs`).
    pub(crate) yt: crate::youtube_sync::YtSync,
}

/// What Settings → Account shows for the Google accounts of the login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoogleAccountsView {
    /// Not probed yet (or the probe failed): nothing.
    Hidden,
    /// The login holds just the account in use: a note on how to add one.
    Single,
    /// Named choices (several accounts, or the one in use isn't among them).
    Choices(Vec<GoogleAccount>),
}

impl Account {
    pub fn new(config: Option<AccountConfig>) -> Self {
        Self {
            config,
            cookie: None,
            session: None,
            expired: false,
            hooks: None,
            channels: Vec::new(),
            google_accounts: None,
            probing: false,
            status: Busy::Idle,
            sync: Busy::Idle,
            yt: Default::default(),
        }
    }

    /// Returns whether the cookie changed (a new login). The session's own newer
    /// value (rotated or renewed, written back to the keychain) is not a change.
    pub fn set_cookie(&mut self, cookie: Option<String>) -> bool {
        if let (Some(session), Some(new)) = (&self.session, &cookie)
            && session.is_known_value(new)
        {
            return false;
        }
        if self.cookie == cookie {
            return false;
        }
        self.cookie = cookie;
        self.expired = false;
        self.session = self.cookie.as_ref().map(|c| {
            let session = Session::new(c.clone());
            if let Some(hooks) = &self.hooks {
                session.set_hooks(hooks.clone());
            }
            session
        });
        // Another cookie is another browser session with its own accounts.
        self.google_accounts = None;
        self.yt.forget_account();
        if self.cookie.is_none() {
            self.channels.clear();
        }
        true
    }

    /// The live session of the signed-in cookie.
    pub fn session(&self) -> Option<&Arc<Session>> {
        self.session.as_ref()
    }

    /// Signed in again with `cookie` while this login was expired: the same cookie
    /// renews the session (a new one replaces it in [`Account::set_cookie`]).
    pub fn renew(&mut self, cookie: &str) {
        if let Some(session) = &self.session
            && session.is_known_value(cookie)
        {
            session.replace_cookie(cookie);
        }
        self.expired = false;
    }

    /// Credentials for acting as `auth_user` / `data_sync_id` with the current cookie,
    /// without touching the config.
    pub fn auth_as(&self, auth_user: u32, data_sync_id: Option<String>) -> Option<Auth> {
        let auth = Auth {
            auth_user,
            data_sync_id,
            session: self.session.clone(),
            ..Auth::new(self.cookie.clone()?)
        };
        auth.is_signed_in().then_some(auth)
    }

    pub fn google_accounts_view(&self) -> GoogleAccountsView {
        let current = self.config.as_ref().map_or(0, |c| c.auth_user);
        match self.google_accounts.as_deref() {
            None | Some([]) => GoogleAccountsView::Hidden,
            Some([only]) if only.auth_user == current => GoogleAccountsView::Single,
            Some(list) => GoogleAccountsView::Choices(list.to_vec()),
        }
    }

    /// Applies a finished switch to `target_auth_user`. On success the fetched config
    /// (and channels) replace the old ones and are returned for saving; on failure
    /// the `auth_user` / channel of `previous` are put back, the channels stay, and
    /// `status` says what happened. Nothing happens if the cookie changed meanwhile.
    pub fn finish_switch(
        &mut self,
        cookie: &str,
        previous: AccountConfig,
        target_auth_user: u32,
        result: Result<(AccountConfig, Vec<AccountChannel>)>,
    ) -> Option<AccountConfig> {
        if self.cookie.as_deref() != Some(cookie) || self.config.is_none() {
            return None;
        }
        match result {
            Ok((config, channels)) => {
                self.channels = channels;
                self.config = Some(config.clone());
                self.status = Busy::Idle;
                Some(config)
            }
            Err(e) => {
                let known_missing = target_auth_user != previous.auth_user
                    && self
                        .google_accounts
                        .as_ref()
                        .is_some_and(|list| list.iter().all(|a| a.auth_user != target_auth_user));
                self.status = Busy::Failed(if known_missing {
                    t!("account.switch_not_signed_in", name = previous.name)
                } else {
                    t!(
                        "account.switch_failed",
                        error = describe_error(&e),
                        name = previous.name
                    )
                });
                // Back to the previous Google account / channel (other fields, e.g. a
                // sync finishing meanwhile, are kept).
                if let Some(config) = &mut self.config {
                    config.auth_user = previous.auth_user;
                    config.data_sync_id = previous.data_sync_id;
                }
                None
            }
        }
    }

    /// Changes whenever [`Self::is_signed_in`] may (without copying the cookie;
    /// the top bar hashes it with every render, `titlebar.rs`).
    pub(crate) fn sign_in_fingerprint(&self) -> (bool, Option<usize>) {
        (self.config.is_some(), self.cookie.as_ref().map(String::len))
    }

    pub fn is_signed_in(&self) -> bool {
        self.config.is_some() && self.auth().is_some()
    }

    pub fn auth(&self) -> Option<Auth> {
        let cookie = self.cookie.as_ref()?;
        let config = self.config.as_ref();
        let auth = Auth {
            cookie: cookie.clone(),
            visitor_data: None,
            data_sync_id: config.and_then(|c| c.data_sync_id.clone()),
            auth_user: config.map_or(0, |c| c.auth_user),
            session: self.session.clone(),
        };
        auth.is_signed_in().then_some(auth)
    }

    pub fn is_syncing(&self) -> bool {
        self.yt.schedule.running().is_some()
    }

    /// `@account-index-status`: the Google account in use, the login's accounts (names
    /// only; e-mails are not printed), what Settings shows, and the status line.
    pub fn index_status_line(&self) -> String {
        let config = self.config.as_ref();
        let accounts = match &self.google_accounts {
            None if self.probing => "probing".to_owned(),
            None => "unknown".to_owned(),
            Some(list) => list
                .iter()
                .map(|a| format!("{}:{:?}", a.auth_user, a.info.name))
                .collect::<Vec<_>>()
                .join(","),
        };
        let view = match self.google_accounts_view() {
            GoogleAccountsView::Hidden => "hidden",
            GoogleAccountsView::Single => "single",
            GoogleAccountsView::Choices(_) => "choices",
        };
        let status = match &self.status {
            Busy::Idle => "idle".to_owned(),
            Busy::Working(t) => format!("working ({t})"),
            Busy::Done(t) => format!("done ({t})"),
            Busy::Failed(t) => format!("failed ({t})"),
        };
        format!(
            "account-index: signed_in={} auth_user={} channel={} name={:?} accounts=[{accounts}] view={view} status={status}",
            self.is_signed_in(),
            config.map_or(0, |c| c.auth_user),
            if config.and_then(|c| c.data_sync_id.as_ref()).is_some() {
                "set"
            } else {
                "default"
            },
            config.map(|c| c.name.as_str()).unwrap_or_default(),
        )
    }
}

/// A YouTube error as the user reads it: a rejected sign-in and an unreachable
/// server are told apart (both localized); anything else is the technical message.
pub(crate) fn describe_error(e: &music_core::Error) -> String {
    if e.is_auth() {
        t!("account.error.signed_out").to_owned()
    } else if e.is_network() {
        t!("account.error.network", error = e)
    } else {
        e.to_string()
    }
}

/// Cleans up what users paste: a `cookie:` prefix, quotes, line breaks.
pub fn normalize_cookie(raw: &str) -> String {
    let mut text = raw.trim();
    for prefix in ["cookie:", "Cookie:", "COOKIE:"] {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest.trim();
        }
    }
    let text = text.trim_matches(|c| c == '"' || c == '\'');
    text.split(['\n', '\r'])
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Account name, avatar, channels, data sync id for `auth`. Blocking.
fn fetch_account(auth: Auth) -> Result<(AccountConfig, Vec<AccountChannel>)> {
    fetch_account_on(InnerTube::default(), auth)
}

/// [`fetch_account`] through `it` (its API base; a fake server in tests).
fn fetch_account_on(it: InnerTube, auth: Auth) -> Result<(AccountConfig, Vec<AccountChannel>)> {
    let it = it.with_auth(auth.clone());
    let info = it.account_info()?;
    let channels = it.account_channels().unwrap_or_default();
    let config = AccountConfig {
        name: info.name,
        email: info.email,
        handle: info.handle,
        thumbnail_url: info.thumbnail_url,
        auth_user: auth.auth_user,
        data_sync_id: auth.data_sync_id,
        last_sync_at: None,
        last_liked_sync_at: None,
    };
    Ok((config, channels))
}

/// The startup refresh: the saved account, or — when its Google account index is
/// confirmed gone from the login (an older version saved it before checking) — the
/// first Google account instead. `Ok((.., true))` = reset to the first account.
fn refresh_account_on(
    it: InnerTube,
    auth: Auth,
) -> Result<((AccountConfig, Vec<AccountChannel>), bool)> {
    // A saved index other than 0 may have left the login (then YouTube may answer
    // 401): asked outside the session, so that alone doesn't count as an expired
    // sign-in. Index 0 below decides.
    let first_try = if auth.auth_user == 0 {
        auth.clone()
    } else {
        auth.detached()
    };
    let err = match fetch_account_on(it.clone(), first_try) {
        Ok(found) => return Ok((found, false)),
        Err(e) if auth.auth_user == 0 => return Err(e),
        Err(e) => e,
    };
    let gone = it
        .clone()
        .with_auth(auth.clone())
        .google_accounts()
        .is_ok_and(|list| !list.is_empty() && list.iter().all(|a| a.auth_user != auth.auth_user));
    if !gone {
        return Err(err);
    }
    let first = Auth {
        auth_user: 0,
        data_sync_id: None,
        ..auth
    };
    fetch_account_on(it, first).map(|found| (found, true))
}

impl MusicApp {
    /// Signs in with a `Cookie` header from a signed-in music.youtube.com tab.
    pub fn sign_in_with_cookie(
        &mut self,
        cookie: String,
        auth_user: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cookie = normalize_cookie(&cookie);
        let auth = Auth {
            auth_user,
            ..Auth::new(cookie.clone())
        };
        if !auth.is_signed_in() {
            self.store.account.status = Busy::Failed(t!("account.cookie_invalid").into());
            cx.notify();
            return;
        }
        self.store.account.status = Busy::Working(t!("account.checking").into());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = off_thread(move || fetch_account(auth)).await;
            this.update_in(cx, |this, window, cx| match result {
                Ok((config, channels)) => {
                    let name = config.name.clone();
                    this.store.account.status = Busy::Idle;
                    this.store.account.channels = channels;
                    this.store.account.config = Some(config.clone());
                    // Saves the cookie in the keychain; `SettingsEvent::Library` follows.
                    this.settings.update(cx, |panel, cx| {
                        panel.update_library(|l| l.account = Some(config), cx);
                        panel.set_youtube_cookie(Some(cookie.clone()), cx);
                    });
                    let was_expired = this.store.account.expired;
                    this.store.account.set_cookie(Some(cookie.clone()));
                    // Signed in again to the same login: its session works again.
                    this.store.account.renew(&cookie);
                    if was_expired {
                        this.hide_expired_notification(cx);
                    }
                    this.sync_account_auth(cx);
                    notify(window, cx, t!("account.toast.signed_in", name = name));
                    // Likes that waited for a working sign-in.
                    this.retry_queued_ratings(true, cx);
                    // Also finds the Google accounts of the login (in the background).
                    this.refresh_google_accounts(cx);
                    this.request_sync(Trigger::SignIn, cx);
                }
                Err(e) => {
                    this.store.account.status =
                        Busy::Failed(t!("account.sign_in_failed", error = describe_error(&e)));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Forgets the cookie and the account (the synced library stays), and the Google
    /// session of the sign-in web view's profile.
    pub fn sign_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.store.account.expired {
            self.hide_expired_notification(cx);
        }
        self.store.account.config = None;
        self.store.account.set_cookie(None);
        crate::login_webview::clear_profile(cx);
        self.store.account.status = Busy::Idle;
        self.store.account.sync = Busy::Idle;
        // Ratings still owed to YouTube were that account's.
        self.forget_queued_ratings();
        self.sync_account_auth(cx);
        self.settings.update(cx, |panel, cx| {
            panel.update_library(|l| l.account = None, cx);
            panel.set_youtube_cookie(None, cx);
        });
        notify(window, cx, t!("account.toast.signed_out"));
        cx.notify();
    }

    /// The cookie arrived from the keychain at startup: refresh name, avatar, channels
    /// and the login's Google accounts.
    pub(crate) fn account_after_cookie(&mut self, cx: &mut Context<Self>) {
        let Some(auth) = self.store.account.auth() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = off_thread(move || refresh_account_on(InnerTube::default(), auth)).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(((mut config, channels), reset)) => {
                        this.store.account.channels = channels;
                        if let Some(old) = &this.store.account.config
                            && !reset
                        {
                            config.last_sync_at = old.last_sync_at;
                            config.last_liked_sync_at = old.last_liked_sync_at;
                        }
                        if this.store.account.config.as_ref() != Some(&config) {
                            this.store.account.config = Some(config.clone());
                            this.settings.update(cx, |panel, cx| {
                                panel.update_library(|l| l.account = Some(config.clone()), cx)
                            });
                        }
                        this.store.account.status = if reset {
                            this.sync_account_auth(cx);
                            Busy::Done(t!("account.index_reset", name = config.name))
                        } else {
                            Busy::Idle
                        };
                        this.refresh_google_accounts(cx);
                        // ArchiveTune syncs on every start signed in; here unless the
                        // last sync is recent (`youtube_sync.rs`).
                        this.request_sync(Trigger::Startup, cx);
                    }
                    Err(e) => {
                        this.store.account.status =
                            Busy::Failed(t!("account.unreachable", error = describe_error(&e)));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Finds the Google accounts of the cookie's login in the background
    /// (`Account::google_accounts`); a failed probe keeps the last list.
    pub(crate) fn refresh_google_accounts(&mut self, cx: &mut Context<Self>) {
        let account = &mut self.store.account;
        if account.probing {
            return;
        }
        let Some(auth) = account.auth() else {
            return;
        };
        account.probing = true;
        let cookie = auth.cookie.clone();
        cx.spawn(async move |this, cx| {
            let result =
                off_thread(move || InnerTube::default().with_auth(auth).google_accounts()).await;
            this.update(cx, |this, cx| {
                let account = &mut this.store.account;
                account.probing = false;
                // Signed out or another cookie meanwhile: the list is for another login.
                if account.cookie.as_deref() != Some(cookie.as_str()) {
                    return;
                }
                match result {
                    Ok(list) => account.google_accounts = Some(list),
                    Err(e) => eprintln!("music-gpui: listing Google accounts: {e}"),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Acts as another channel (brand account) and/or Google account of the cookie.
    /// Nothing is saved until the new account answered; a failed switch keeps the
    /// previous account (see [`Account::finish_switch`]).
    pub fn switch_account(
        &mut self,
        data_sync_id: Option<String>,
        auth_user: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let account = &mut self.store.account;
        if matches!(account.status, Busy::Working(_)) {
            return;
        }
        let Some(previous) = account.config.clone() else {
            return;
        };
        let Some(auth) = account.auth_as(auth_user, data_sync_id) else {
            return;
        };
        account.status = Busy::Working(t!("account.switching").into());
        let cookie = auth.cookie.clone();
        // Another Google account of the login: a rejection means "not signed in to
        // that one", not an expired session.
        let auth = if auth_user == previous.auth_user {
            auth
        } else {
            auth.detached()
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = off_thread(move || fetch_account(auth)).await;
            this.update_in(cx, |this, window, cx| {
                let changed_google = auth_user != previous.auth_user;
                let account = &mut this.store.account;
                match account.finish_switch(&cookie, previous, auth_user, result) {
                    Some(config) => {
                        let name = config.name.clone();
                        this.sync_account_auth(cx);
                        this.settings.update(cx, |panel, cx| {
                            panel.update_library(|l| l.account = Some(config), cx)
                        });
                        notify(window, cx, t!("account.toast.switched", name = name));
                        // Another library: what this one owed YouTube isn't its.
                        this.store.account.yt.forget_account();
                        this.forget_queued_ratings();
                        this.refresh_google_accounts(cx);
                        this.request_sync(Trigger::AccountSwitch, cx);
                    }
                    None => {
                        // The list may be out of date (an account signed out since).
                        if changed_google {
                            this.refresh_google_accounts(cx);
                        }
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// "Sync now": copies the account's library into the local database.
    pub fn sync_youtube_library(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_youtube(Trigger::Manual, window, cx);
    }

    /// An explicit sync (`Trigger::Manual`, `Trigger::ManualLiked`).
    pub fn sync_youtube(&mut self, trigger: Trigger, window: &mut Window, cx: &mut Context<Self>) {
        if !self.store.account.is_signed_in() || self.store.library.is_none() {
            notify(window, cx, t!("account.sign_in_first"));
            return;
        }
        // Accounts may have been added to / removed from the login since.
        self.refresh_google_accounts(cx);
        self.request_sync(trigger, cx);
        cx.notify();
    }

    /// Runs `req` (already marked running by the schedule) in the background.
    pub(crate) fn run_sync(&mut self, req: SyncRequest, cx: &mut Context<Self>) {
        let (Some(auth), Some(library)) = (self.store.account.auth(), self.store.library.clone())
        else {
            self.sync_finished(cx);
            return;
        };
        let start = match req.kind {
            SyncKind::Full => t!("account.sync.starting"),
            SyncKind::Liked => t!("account.sync.step.liked"),
        };
        let progress = Arc::new(Mutex::new(String::from(start)));
        self.store.account.sync = Busy::Working(start.into());
        let shared = progress.clone();
        let executor = cx.background_executor().clone();
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(300)).await;
                let step = progress.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let running = this
                    .update(cx, |this, cx| {
                        if this.store.account.is_syncing() {
                            this.store.account.sync = Busy::Working(step);
                            cx.notify();
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
        });
        let cookie = auth.cookie.clone();
        cx.spawn(async move |this, cx| {
            let report = off_thread(move || {
                let it = InnerTube::default().with_auth(auth);
                sync_library(&library, &it, req, &|step| {
                    *shared.lock().unwrap_or_else(|e| e.into_inner()) = step.to_owned();
                })
            })
            .await;
            drop(ticker);
            let toast = this
                .update(cx, |this, cx| {
                    let summary = report.summary(req.kind);
                    // Signed out (or another login) meanwhile: the account's state isn't
                    // this result's.
                    let same_login = this
                        .store
                        .account
                        .auth()
                        .is_some_and(|a| a.cookie == cookie);
                    if same_login {
                        this.store.account.sync = if report.auth_failed {
                            // The sign-in notification says the rest.
                            Busy::Failed(t!("account.expired.status").to_owned())
                        } else if report.errors.is_empty() {
                            Busy::Done(summary.clone())
                        } else {
                            Busy::Failed(t!(
                                "account.sync.failed",
                                summary = summary,
                                errors = report.errors.join("; ")
                            ))
                        };
                        this.save_sync_times(req.kind, &report, cx);
                    }
                    // Hearts and thumbs everywhere.
                    this.reload_liked_flags();
                    this.library_changed(cx);
                    let announce = this.sync_finished(cx) && !report.auth_failed;
                    (announce && same_login).then(|| t!("account.toast.synced", summary = summary))
                })
                .ok()
                .flatten();
            if let Some(message) = toast {
                cx.update(|cx| {
                    crate::actions::with_main_window(cx, move |_, window, cx| {
                        notify(window, cx, message)
                    })
                });
            }
        })
        .detach();
        cx.notify();
    }

    /// Remembers when the library / the liked songs were last synced (a sync with a
    /// few failed parts still counts; one where the liked songs failed doesn't for them).
    fn save_sync_times(&mut self, kind: SyncKind, report: &SyncReport, cx: &mut Context<Self>) {
        let now = now_ms();
        let full = kind == SyncKind::Full && report.errors.len() < 5;
        let liked = report.liked_ok;
        if !full && !liked {
            return;
        }
        let apply = |a: &mut AccountConfig| {
            if full {
                a.last_sync_at = Some(now);
            }
            if liked {
                a.last_liked_sync_at = Some(now);
            }
        };
        if let Some(config) = &mut self.store.account.config {
            apply(config);
        }
        self.settings.update(cx, |panel, cx| {
            panel.update_library(
                |l| {
                    if let Some(a) = &mut l.account {
                        apply(a)
                    }
                },
                cx,
            )
        });
    }

    /// Drops the ratings still owed to YouTube (sign-out, another account).
    fn forget_queued_ratings(&mut self) {
        if let Some(lib) = &self.store.library
            && let Err(e) = lib.clear_pending_ratings()
        {
            eprintln!("music-gpui: clearing queued ratings: {e}");
        }
    }

    pub(crate) fn reload_liked_flags(&mut self) {
        if let Some(lib) = &self.store.library {
            if let Ok(songs) = lib.songs(&SongQuery {
                scope: SongScope::Liked,
                ..Default::default()
            }) {
                crate::store::replace_liked(songs.into_iter().map(|s| s.track.video_id).collect());
            }
            if let Ok(ids) = lib.disliked_ids() {
                crate::store::replace_disliked(ids);
            }
        }
    }

    /// Subscribes to / unsubscribes from an artist (YouTube when signed in, and the
    /// local bookmark either way).
    pub fn set_subscribed(
        &mut self,
        artist: ArtistMeta,
        subscribed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let remote = self.account_client();
        self.with_library(
            t!("account.action.subscribing"),
            move |lib| {
                lib.upsert_artist(&artist)?;
                lib.set_artist_bookmarked(&artist.id, subscribed)?;
                if remote.is_signed_in() && artist.id.starts_with("UC") {
                    let ids = [artist.id.as_str()];
                    if subscribed {
                        remote.subscribe(&ids)?;
                    } else {
                        remote.unsubscribe(&ids)?;
                    }
                }
                Ok(artist.name)
            },
            move |_, name, window, cx| {
                let message = if subscribed {
                    t!("account.toast.subscribed", artist = name)
                } else {
                    t!("account.toast.unsubscribed", artist = name)
                };
                notify(window, cx, message);
            },
            window,
            cx,
        );
    }
}

#[derive(Default, Debug)]
struct SyncReport {
    liked: usize,
    /// The liked songs were synced.
    liked_ok: bool,
    /// Local likes removed because YouTube no longer lists them.
    unliked: usize,
    songs: usize,
    albums: usize,
    artists: usize,
    playlists: usize,
    errors: Vec<String>,
    /// YouTube rejected the sign-in: the sync stopped there.
    auth_failed: bool,
}

impl SyncReport {
    fn summary(&self, kind: SyncKind) -> String {
        if kind == SyncKind::Liked {
            return tp!("account.sync.liked", self.liked);
        }
        t!(
            "account.sync.summary",
            liked = tp!("account.sync.liked", self.liked),
            songs = tp!("account.sync.songs", self.songs),
            albums = tp!("account.sync.albums", self.albums),
            artists = tp!("account.sync.artists", self.artists),
            playlists = tp!("account.sync.playlists", self.playlists),
        )
    }
}

/// All items of a library tab, following continuations.
fn all_items(
    it: &InnerTube,
    first: impl FnOnce() -> Result<music_core::innertube::LibraryPage>,
) -> Result<Vec<Item>> {
    let mut page = first()?;
    let mut items = std::mem::take(&mut page.items);
    let mut pages = 1;
    while let Some(token) = page.continuation.take()
        && pages < MAX_PAGES
    {
        page = it.library_continuation(&token)?;
        items.append(&mut page.items);
        pages += 1;
    }
    Ok(items)
}

/// A playlist's rows: track + `setVideoId`.
type Rows = Vec<(Track, Option<String>)>;

/// Playlist rows with their `setVideoId`s, following continuations.
fn playlist_rows(it: &InnerTube, playlist_id: &str) -> Result<(bool, Option<String>, Rows)> {
    let mut page = it.playlist(playlist_id)?;
    let mut rows: Rows = page
        .tracks
        .drain(..)
        .map(|e| (e.track, e.set_video_id))
        .collect();
    let mut continuation = page.continuation.take();
    let mut pages = 1;
    while let Some(token) = continuation
        && pages < MAX_PAGES
    {
        let more = it.playlist_continuation(&token)?;
        rows.extend(more.tracks.into_iter().map(|e| (e.track, e.set_video_id)));
        continuation = more.continuation;
        pages += 1;
    }
    Ok((page.editable, page.playlist.thumbnail_url, rows))
}

/// Port of ArchiveTune's `SyncUtils`: YouTube is the source of truth for liked songs,
/// saved albums, subscriptions and playlists; local-only items (local files, local
/// playlists, artists without a channel) are left alone. `SyncKind::Liked` stops
/// after the liked songs.
///
/// Ratings still queued for YouTube are sent first; those that stay queued (offline)
/// win over YouTube's list. Unless `authoritative`, an empty or truncated liked list
/// unlikes nothing (ArchiveTune: "Remote playlist is empty" → skip).
fn sync_library(
    lib: &Library,
    it: &InnerTube,
    req: SyncRequest,
    progress: &dyn Fn(&str),
) -> SyncReport {
    let mut report = SyncReport::default();
    let now = now_ms();
    let fail = |what: &str, e: music_core::Error, report: &mut SyncReport| {
        report.auth_failed |= e.is_auth();
        report.errors.push(t!(
            "account.sync.error",
            part = what,
            error = describe_error(&e)
        ));
    };

    progress(t!("account.sync.step.liked"));
    match lib.push_pending_ratings(it, 200) {
        // The session is gone: every request after this would fail the same way.
        Ok(push) if push.auth_failed => {
            report.auth_failed = true;
            return report;
        }
        Ok(_) => {}
        Err(e) => eprintln!("music-gpui: queued ratings: {e}"),
    }
    match it.liked_songs().and_then(|mut page| {
        let mut tracks: Vec<Track> = page.tracks.drain(..).map(|e| e.track).collect();
        let mut continuation = page.continuation.take();
        let mut pages = 1;
        while let Some(token) = continuation.take()
            && pages < MAX_PAGES
        {
            let more = it.playlist_continuation(&token)?;
            tracks.extend(more.tracks.into_iter().map(|e| e.track));
            continuation = more.continuation;
            pages += 1;
        }
        Ok((tracks, continuation.is_none()))
    }) {
        Ok((tracks, complete)) => {
            let result = apply_liked(lib, &tracks, complete, req.authoritative, now);
            match result {
                Ok(unliked) => {
                    report.liked = tracks.len();
                    report.unliked = unliked;
                    report.liked_ok = true;
                }
                Err(e) => fail(t!("account.sync.part.liked"), e, &mut report),
            }
        }
        Err(e) => fail(t!("account.sync.part.liked"), e, &mut report),
    }
    if req.kind == SyncKind::Liked || report.auth_failed {
        return report;
    }

    progress(t!("account.sync.step.songs"));
    match all_items(it, || it.library_songs()) {
        Ok(items) => {
            let tracks: Vec<&Track> = items
                .iter()
                .filter_map(|i| match i {
                    Item::Song(t) | Item::Video(t) => Some(t),
                    _ => None,
                })
                .collect();
            let result: Result<()> = tracks
                .iter()
                .enumerate()
                .try_for_each(|(i, t)| lib.set_in_library_at(t, Some(now - i as i64)));
            match result {
                Ok(()) => report.songs = tracks.len(),
                Err(e) => fail(t!("account.sync.part.songs"), e, &mut report),
            }
        }
        Err(e) => fail(t!("account.sync.part.songs"), e, &mut report),
    }
    if report.auth_failed {
        return report;
    }

    progress(t!("account.sync.step.albums"));
    match all_items(it, || it.library_albums()) {
        Ok(items) => {
            let result = (|| {
                let mut remote = HashSet::new();
                for item in &items {
                    let Item::Album(a) = item else { continue };
                    let meta = AlbumMeta {
                        id: a.browse_id.clone(),
                        title: a.title.clone(),
                        artists: a
                            .artists
                            .iter()
                            .map(|r| r.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", "),
                        year: a.year.as_deref().and_then(|y| y.parse().ok()),
                        thumbnail_url: a.thumbnail_url.clone(),
                        playlist_id: a.playlist_id.clone(),
                        explicit: a.explicit,
                        ..Default::default()
                    };
                    lib.upsert_album(&meta, &[])?;
                    lib.set_album_bookmarked(&meta.id, true)?;
                    remote.insert(meta.id);
                }
                for album in lib.albums(&AlbumQuery {
                    bookmarked_only: true,
                    ..Default::default()
                })? {
                    let id = &album.meta.id;
                    if !id.starts_with("LB:") && !remote.contains(id) {
                        lib.set_album_bookmarked(id, false)?;
                    }
                }
                Ok::<_, music_core::Error>(remote.len())
            })();
            match result {
                Ok(n) => report.albums = n,
                Err(e) => fail(t!("account.sync.part.albums"), e, &mut report),
            }
        }
        Err(e) => fail(t!("account.sync.part.albums"), e, &mut report),
    }
    if report.auth_failed {
        return report;
    }

    progress(t!("account.sync.step.artists"));
    match all_items(it, || it.library_subscriptions()) {
        Ok(items) => {
            let result = (|| {
                let mut remote = HashSet::new();
                for item in &items {
                    let Item::Artist(a) = item else { continue };
                    lib.upsert_artist(&ArtistMeta {
                        id: a.browse_id.clone(),
                        name: a.name.clone(),
                        thumbnail_url: a.thumbnail_url.clone(),
                    })?;
                    lib.set_artist_bookmarked(&a.browse_id, true)?;
                    remote.insert(a.browse_id.clone());
                }
                for artist in lib.artists(&ArtistQuery {
                    bookmarked_only: true,
                    ..Default::default()
                })? {
                    if artist.channel_id().is_some() && !remote.contains(&artist.id) {
                        lib.set_artist_bookmarked(&artist.id, false)?;
                    }
                }
                Ok::<_, music_core::Error>(remote.len())
            })();
            match result {
                Ok(n) => report.artists = n,
                Err(e) => fail(t!("account.sync.part.artists"), e, &mut report),
            }
        }
        Err(e) => fail(t!("account.sync.part.artists"), e, &mut report),
    }
    if report.auth_failed {
        return report;
    }

    progress(t!("account.sync.step.playlists"));
    match all_items(it, || it.library_playlists()) {
        Ok(items) => {
            let mut remote = HashSet::new();
            let playlists: Vec<_> = items
                .into_iter()
                .filter_map(|i| match i {
                    // "Liked music" and "Episodes for later" are covered elsewhere.
                    Item::Playlist(p) if p.playlist_id != "LM" && p.playlist_id != "SE" => Some(p),
                    _ => None,
                })
                .collect();
            let total = playlists.len();
            for (n, p) in playlists.into_iter().enumerate() {
                if report.auth_failed {
                    return report;
                }
                progress(&t!(
                    "account.sync.step.playlist",
                    current = n + 1,
                    total = total,
                    title = p.title
                ));
                remote.insert(p.playlist_id.clone());
                let result =
                    playlist_rows(it, &p.playlist_id).and_then(|(editable, thumb, rows)| {
                        let playlist = lib.upsert_remote_playlist(&RemotePlaylist {
                            browse_id: p.playlist_id.clone(),
                            name: p.title.clone(),
                            thumbnail_url: p.thumbnail_url.clone().or(thumb),
                            editable,
                            song_count: Some(rows.len() as u32),
                        })?;
                        lib.replace_playlist_entries(&playlist.id, &rows)?;
                        Ok(())
                    });
                match result {
                    Ok(()) => report.playlists += 1,
                    Err(e) => fail(
                        &t!("account.sync.part.playlist", title = p.title),
                        e,
                        &mut report,
                    ),
                }
            }
            // Mirrors of playlists that left the account.
            if let Ok(local) = lib.playlists(PlaylistSort::Name, false) {
                for playlist in local {
                    if let Some(browse_id) = &playlist.browse_id
                        && !remote.contains(browse_id)
                        && let Err(e) = lib.delete_playlist(&playlist.id)
                    {
                        fail(t!("account.sync.part.removing_playlist"), e, &mut report);
                    }
                }
            }
        }
        Err(e) => fail(t!("account.sync.part.playlists"), e, &mut report),
    }
    report
}

/// Makes the local likes YouTube's `remote` list (newest first). Returns how many
/// local likes were removed. A rating still queued for YouTube is kept as it is;
/// local files keep their likes; with an empty (and not `authoritative`) or
/// incomplete list nothing is unliked.
fn apply_liked(
    lib: &Library,
    remote: &[Track],
    complete: bool,
    authoritative: bool,
    now: i64,
) -> Result<usize> {
    let pending = lib.pending_rating_map()?;
    let waiting = |id: &str| pending.contains_key(id);
    for (i, track) in remote.iter().enumerate() {
        if !waiting(&track.video_id) {
            lib.set_liked_at(track, Some(now - i as i64))?;
        }
    }
    if !complete || (remote.is_empty() && !authoritative) {
        return Ok(0);
    }
    let ids: HashSet<&str> = remote.iter().map(|t| t.video_id.as_str()).collect();
    let local = lib.songs(&SongQuery {
        scope: SongScope::Liked,
        ..Default::default()
    })?;
    let mut unliked = 0;
    for song in local {
        let id = song.track.video_id.as_str();
        if !song.track.is_local() && !ids.contains(id) && !waiting(id) {
            lib.unlike(id)?;
            unliked += 1;
        }
    }
    Ok(unliked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_paste_is_normalized() {
        assert_eq!(
            normalize_cookie("cookie: SID=a; SAPISID=b\n"),
            "SID=a; SAPISID=b"
        );
        assert_eq!(normalize_cookie("'A=1;\n B=2'"), "A=1; B=2");
        assert!(Auth::new(normalize_cookie("Cookie: SAPISID=x")).is_signed_in());
    }

    #[test]
    fn account_needs_cookie_and_config() {
        let mut a = Account::new(None);
        assert!(!a.is_signed_in());
        assert!(a.set_cookie(Some("SAPISID=x; SID=y".into())));
        assert!(!a.set_cookie(Some("SAPISID=x; SID=y".into())));
        assert!(!a.is_signed_in(), "no account config yet");
        a.config = Some(AccountConfig {
            auth_user: 2,
            data_sync_id: Some("d||u".into()),
            ..Default::default()
        });
        let auth = a.auth().unwrap();
        assert_eq!(auth.auth_user, 2);
        assert_eq!(auth.delegated_session_id(), Some("d"));
        assert!(a.is_signed_in());
        a.set_cookie(Some("SID=only".into()));
        assert!(!a.is_signed_in());
    }

    // ------------------------------------------------ Google accounts of the login

    const COOKIE: &str = "SAPISID=fake; SID=fake";

    fn fixture(name: &str) -> String {
        let path = format!(
            "{}/../music-core/tests/fixtures/innertube/authed/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    /// A fake music.youtube.com whose login holds the Google accounts `menus`
    /// (`account_menu` fixture per `X-Goog-AuthUser`; other indexes get a 401 like
    /// Google's). Returns the base URL.
    fn fake_youtube(menus: &'static [&'static str]) -> String {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let (mut len, mut user) = (0usize, 0usize);
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
                        break;
                    }
                    let (k, v) = header.split_once(':').unwrap_or_default();
                    match k.trim().to_ascii_lowercase().as_str() {
                        "content-length" => len = v.trim().parse().unwrap_or(0),
                        "x-goog-authuser" => user = v.trim().parse().unwrap_or(usize::MAX),
                        _ => {}
                    }
                }
                let mut body = vec![0; len];
                let _ = reader.read_exact(&mut body);
                let (status, json) = if path.contains("/account/account_menu") {
                    match menus.get(user) {
                        Some(name) => (200, fixture(name)),
                        None => (401, r#"{"error":{"code":401,"message":"Request had invalid authentication credentials."}}"#.to_owned()),
                    }
                } else if path.contains("/account/accounts_list") {
                    (200, fixture("accounts_list"))
                } else {
                    (404, "{}".to_owned())
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                    json.len()
                );
            }
        });
        base
    }

    const TWO: &[&str] = &["account_menu", "account_menu_second"];
    const ONE: &[&str] = &["account_menu"];

    fn google(list: &[(u32, &str)]) -> Vec<GoogleAccount> {
        list.iter()
            .map(|(n, name)| GoogleAccount {
                auth_user: *n,
                info: music_core::innertube::AccountInfo {
                    name: (*name).into(),
                    ..Default::default()
                },
            })
            .collect()
    }

    /// Signed in as Google account 0 ("Test User"), last synced at 5.
    fn signed_in() -> Account {
        let mut a = Account::new(Some(AccountConfig {
            name: "Test User".into(),
            auth_user: 0,
            last_sync_at: Some(5),
            ..Default::default()
        }));
        a.set_cookie(Some(COOKIE.into()));
        a
    }

    /// What `MusicApp::switch_account` does, minus GPUI: fetch on `base`, then settle.
    fn switch(a: &mut Account, base: &str, to: u32) -> Option<AccountConfig> {
        let previous = a.config.clone().unwrap();
        let auth = a.auth_as(to, None).unwrap();
        // As `switch_account` does: another index is asked outside the session.
        let auth = if to == previous.auth_user {
            auth
        } else {
            auth.detached()
        };
        let result = fetch_account_on(InnerTube::default().with_api_base(base), auth);
        assert!(
            !a.session().unwrap().is_expired(),
            "a probe doesn't expire it"
        );
        a.finish_switch(COOKIE, previous, to, result)
    }

    #[test]
    fn failed_switch_keeps_the_previous_account() {
        let base = fake_youtube(ONE);
        let mut a = signed_in();
        a.google_accounts = Some(google(&[(0, "Test User")]));
        let before = a.config.clone();

        // Index 2 isn't in the login: nothing is kept from the attempt.
        assert_eq!(switch(&mut a, &base, 2), None, "nothing to save");
        assert_eq!(a.config, before);
        assert_eq!(
            a.auth().unwrap().auth_user,
            0,
            "still signed in to account 0"
        );
        assert!(a.is_signed_in());
        let Busy::Failed(message) = &a.status else {
            panic!("{:?}", a.status)
        };
        assert!(message.contains("isn't signed in"), "{message}");
        assert!(message.contains("Test User"), "{message}");

        // Not known to be missing (list not probed yet): the error is shown instead.
        a.google_accounts = None;
        assert_eq!(switch(&mut a, &base, 3), None);
        assert_eq!(a.config, before);
        let Busy::Failed(message) = &a.status else {
            panic!("{:?}", a.status)
        };
        assert!(message.contains("Couldn't switch"), "{message}");
        assert!(
            message.contains(t!("account.error.signed_out")) || message.contains("invalid"),
            "{message}"
        );

        // A failed channel switch puts the previous channel back as well.
        let mut a = signed_in();
        a.config.as_mut().unwrap().data_sync_id = Some("222||111".into());
        let previous = a.config.clone().unwrap();
        let err = Err(music_core::Error::Parse("offline".into()));
        assert_eq!(a.finish_switch(COOKIE, previous.clone(), 0, err), None);
        assert_eq!(a.config.as_ref(), Some(&previous));
    }

    #[test]
    fn switch_to_a_signed_in_google_account() {
        let base = fake_youtube(TWO);
        let mut a = signed_in();
        a.google_accounts = Some(google(&[(0, "Test User"), (1, "Work Account")]));
        let saved = switch(&mut a, &base, 1).expect("switched");
        assert_eq!(saved.auth_user, 1);
        assert_eq!(saved.name, "Work Account");
        assert_eq!(saved.email.as_deref(), Some("work.account@example.org"));
        assert_eq!(saved.data_sync_id, None, "the account's own channel");
        assert_eq!(saved.last_sync_at, None, "another library");
        assert_eq!(a.config.as_ref(), Some(&saved));
        assert_eq!(a.auth().unwrap().auth_user, 1);
        assert_eq!(a.status, Busy::Idle);
        assert_eq!(a.channels.len(), 2, "channels of the new account");
    }

    #[test]
    fn switch_result_after_sign_out_is_dropped() {
        let base = fake_youtube(TWO);
        let mut a = signed_in();
        let previous = a.config.clone().unwrap();
        let auth = a.auth_as(1, None).unwrap();
        let result = fetch_account_on(InnerTube::default().with_api_base(&base), auth);
        a.config = None;
        a.set_cookie(None);
        assert_eq!(a.finish_switch(COOKIE, previous, 1, result), None);
        assert_eq!(a.config, None, "not signed back in");
    }

    #[test]
    fn google_account_row_only_for_several_accounts() {
        let mut a = signed_in();
        assert_eq!(
            a.google_accounts_view(),
            GoogleAccountsView::Hidden,
            "not probed"
        );
        a.google_accounts = Some(Vec::new());
        assert_eq!(a.google_accounts_view(), GoogleAccountsView::Hidden);
        a.google_accounts = Some(google(&[(0, "Test User")]));
        assert_eq!(a.google_accounts_view(), GoogleAccountsView::Single);
        let two = google(&[(0, "Test User"), (1, "Work Account")]);
        a.google_accounts = Some(two.clone());
        assert_eq!(a.google_accounts_view(), GoogleAccountsView::Choices(two));
        // Using an index that isn't in the login: offer the way back.
        a.config.as_mut().unwrap().auth_user = 1;
        a.google_accounts = Some(google(&[(0, "Test User")]));
        assert!(matches!(
            a.google_accounts_view(),
            GoogleAccountsView::Choices(list) if list.len() == 1
        ));
        // Another cookie is another login: probe again.
        a.set_cookie(Some("SAPISID=other".into()));
        assert_eq!(a.google_accounts, None);
        assert!(a.index_status_line().contains("view=hidden"));
    }

    #[test]
    fn startup_resets_an_index_that_left_the_login() {
        let base = fake_youtube(ONE);
        let it = || InnerTube::default().with_api_base(&base);
        let saved = |auth_user| Auth {
            auth_user,
            data_sync_id: Some("999||888".into()),
            ..Auth::new(COOKIE)
        };
        // Index 2 (saved by an older version) isn't in the login: back to the first.
        let ((config, _), reset) = refresh_account_on(it(), saved(2)).unwrap();
        assert!(reset);
        assert_eq!(config.auth_user, 0);
        assert_eq!(config.data_sync_id, None);
        assert_eq!(config.name, "Test User");
        // Index 0 works as saved.
        let ((config, _), reset) = refresh_account_on(it(), saved(0)).unwrap();
        assert!(!reset);
        assert_eq!(config.data_sync_id.as_deref(), Some("999||888"));

        // Index 1 is in the login: used as saved.
        let two = fake_youtube(TWO);
        let ((config, _), reset) =
            refresh_account_on(InnerTube::default().with_api_base(&two), saved(1)).unwrap();
        assert!(!reset);
        assert_eq!(config.auth_user, 1);

        // Nothing answers (offline, cookie expired): an error, the config stays.
        let none = fake_youtube(&[]);
        assert!(refresh_account_on(InnerTube::default().with_api_base(&none), saved(2)).is_err());
        assert!(refresh_account_on(InnerTube::default().with_api_base(&none), saved(0)).is_err());
    }

    fn song(id: &str) -> Track {
        Track {
            video_id: id.into(),
            title: format!("Song {id}"),
            artists: "Artist".into(),
            ..Track::default()
        }
    }

    fn liked_ids(lib: &Library) -> Vec<String> {
        let mut ids: Vec<String> = lib
            .songs(&SongQuery {
                scope: SongScope::Liked,
                ..Default::default()
            })
            .unwrap()
            .into_iter()
            .map(|s| s.track.video_id)
            .collect();
        ids.sort();
        ids
    }

    /// YouTube's liked list wins, except over queued ratings, local files, and
    /// (unless authoritative) an empty or truncated list.
    #[test]
    fn liked_songs_follow_youtube_with_guards() {
        use music_core::innertube::Rating;
        let lib = Library::open_in_memory().unwrap();
        for id in ["stale", "kept-offline"] {
            lib.like(&song(id)).unwrap();
        }
        let mut file = song("file");
        file.local_path = Some("/music/file.mp3".into());
        lib.like(&file).unwrap();
        // Unliked here while offline; YouTube still lists it.
        lib.unlike("queued-unlike").unwrap();
        lib.queue_rating("queued-unlike", Rating::Indifferent)
            .unwrap();
        // Liked here while offline; YouTube doesn't list it yet.
        lib.queue_rating("kept-offline", Rating::Like).unwrap();
        // Disliked here earlier, liked on the phone since.
        lib.set_rating(&song("was-disliked"), Rating::Dislike)
            .unwrap();

        let remote: Vec<Track> = ["new", "queued-unlike", "was-disliked"]
            .into_iter()
            .map(song)
            .collect();
        // Truncated list: likes are added, none removed.
        assert_eq!(apply_liked(&lib, &remote, false, false, 100).unwrap(), 0);
        assert!(liked_ids(&lib).contains(&"stale".to_owned()));
        // Complete list: "stale" goes; the file and the queued ratings stay as they are.
        assert_eq!(apply_liked(&lib, &remote, true, false, 100).unwrap(), 1);
        assert_eq!(
            liked_ids(&lib),
            ["file", "kept-offline", "new", "was-disliked"]
        );
        assert_eq!(lib.rating("was-disliked").unwrap(), Rating::Like);
        assert_eq!(lib.rating("queued-unlike").unwrap(), Rating::Indifferent);
        // Newest first: the first remote song gets the latest time.
        let at = |id| lib.song(id).unwrap().unwrap().liked_at.unwrap();
        assert!(at("new") > at("was-disliked"));

        // An empty answer from an automatic sync unlikes nothing; asked for, it does.
        assert_eq!(apply_liked(&lib, &[], true, false, 200).unwrap(), 0);
        assert_eq!(liked_ids(&lib).len(), 4);
        assert_eq!(apply_liked(&lib, &[], true, true, 200).unwrap(), 2);
        assert_eq!(liked_ids(&lib), ["file", "kept-offline"]);
    }

    /// A liked-songs-only sync against a fake music.youtube.com: queued ratings go
    /// first, then `VLLM` is mirrored; nothing else is requested.
    #[test]
    fn liked_only_sync_from_a_fake_server() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::sync::{Arc, Mutex};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let mut len = 0usize;
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0; len];
                let _ = reader.read_exact(&mut body);
                let body: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
                let endpoint = path
                    .trim_start_matches("/youtubei/v1/")
                    .split('?')
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                let what = match endpoint.as_str() {
                    "browse" => format!("browse {}", body["browseId"].as_str().unwrap_or("?")),
                    other => format!(
                        "{other} {}",
                        body["target"]["videoId"].as_str().unwrap_or("?")
                    ),
                };
                log.lock().unwrap().push(what.clone());
                let (status, json) = match what.as_str() {
                    "browse VLLM" => (200, fixture("liked_songs")),
                    w if w.starts_with("like/") => (200, "{}".to_owned()),
                    _ => (404, "{}".to_owned()),
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
            .with_auth(Auth::new(COOKIE));
        let lib = Library::open_in_memory().unwrap();
        lib.like(&song("gone-from-youtube")).unwrap();
        lib.queue_rating("offline-dislike", music_core::innertube::Rating::Dislike)
            .unwrap();
        let req = SyncRequest {
            kind: SyncKind::Liked,
            authoritative: false,
            announce: false,
        };
        let report = sync_library(&lib, &it, req, &|_| {});
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(report.liked_ok);
        assert_eq!((report.liked, report.unliked), (2, 1));
        assert_eq!(liked_ids(&lib), ["5NV6Rdv1a3I", "khnokW3Mw24"]);
        assert!(lib.pending_ratings().unwrap().is_empty(), "delivered");
        assert_eq!(
            *seen.lock().unwrap(),
            ["like/dislike offline-dislike", "browse VLLM"]
        );
    }

    /// Syncing against an anonymous client fails cleanly for every step.
    #[test]
    fn sync_without_cookie_reports_errors() {
        let lib = Library::open_in_memory().unwrap();
        let full = SyncRequest {
            kind: SyncKind::Full,
            authoritative: true,
            announce: true,
        };
        let report = sync_library(&lib, &InnerTube::default(), full, &|_| {});
        assert_eq!(report.errors.len(), 5, "{:?}", report.errors);
        assert_eq!(report.liked, 0);
        assert!(!report.liked_ok);
        let liked = SyncRequest {
            kind: SyncKind::Liked,
            ..full
        };
        let report = sync_library(&lib, &InnerTube::default(), liked, &|_| {});
        assert_eq!(report.errors.len(), 1, "only the liked songs");
    }

    /// A tiny fake music.youtube.com: answers every request with `status` and `{}`,
    /// and logs the endpoints asked.
    fn status_server(status: u16) -> (String, Arc<Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let mut len = 0usize;
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0; len];
                let _ = reader.read_exact(&mut body);
                let endpoint = path.trim_start_matches("/youtubei/v1/");
                log.lock()
                    .unwrap()
                    .push(endpoint.split('?').next().unwrap_or_default().to_owned());
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
                );
            }
        });
        (base, seen)
    }

    /// The keychain copy of a rotated cookie is the same login, not a new sign-in.
    #[test]
    fn rotated_cookie_written_back_keeps_the_session() {
        let mut account = Account::new(Some(AccountConfig::default()));
        assert!(account.set_cookie(Some("SAPISID=a; TS=1".into())));
        let session = account.session().unwrap().clone();
        let auth = account.auth().unwrap();
        // Rotation (here: by hand) reaches every Auth handed out.
        session.replace_cookie("SAPISID=a; TS=2");
        assert_eq!(auth.cookie_header(), "SAPISID=a; TS=2");
        let saved = session.take_persist_now().unwrap();
        // The settings event brings the saved value back: nothing changes.
        assert!(!account.set_cookie(Some(saved)));
        assert!(Arc::ptr_eq(account.session().unwrap(), &session));
        assert_eq!(account.auth().unwrap(), auth, "same session, same Auth");
        // Another cookie is another login with a session of its own.
        account.expired = true;
        assert!(account.set_cookie(Some("SAPISID=b".into())));
        assert!(!Arc::ptr_eq(account.session().unwrap(), &session));
        assert!(!account.expired);
        // Signing in again with the same cookie renews it.
        account.expired = true;
        assert!(!account.set_cookie(Some("SAPISID=b".into())));
        account.renew("SAPISID=b");
        assert!(!account.expired);
        assert!(account.set_cookie(None));
        assert!(account.session().is_none());
    }

    /// A rejected sign-in stops the sync at the first request instead of failing
    /// every part, and queued ratings keep their attempts.
    #[test]
    fn sync_stops_at_a_rejected_sign_in() {
        let (base, seen) = status_server(401);
        let session = Session::new(COOKIE);
        let it = InnerTube::default()
            .with_api_base(&base)
            .with_auth(Auth::new(COOKIE).with_session(session.clone()));
        let lib = Library::open_in_memory().unwrap();
        let full = SyncRequest {
            kind: SyncKind::Full,
            authoritative: true,
            announce: true,
        };
        let report = sync_library(&lib, &it, full, &|_| {});
        assert!(report.auth_failed);
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert!(report.errors[0].contains(t!("account.error.signed_out")));
        assert_eq!(seen.lock().unwrap().len(), 1, "nothing after the first 401");
        assert!(session.is_expired(), "no hooks: recovery fails");

        // With ratings queued, the push already finds out; nothing is used up.
        lib.queue_rating("v", music_core::innertube::Rating::Like)
            .unwrap();
        seen.lock().unwrap().clear();
        let report = sync_library(&lib, &it, full, &|_| {});
        assert!(report.auth_failed);
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert_eq!(lib.pending_ratings().unwrap()[0].attempts, 0);
    }

    /// While the session is known to be expired, a like is saved and queued without
    /// asking YouTube (and reported as a sign-in problem, not a network one).
    #[test]
    fn likes_wait_while_the_sign_in_is_expired() {
        let (base, seen) = status_server(401);
        let session = Session::new(COOKIE);
        let it = InnerTube::default()
            .with_api_base(&base)
            .with_auth(Auth::new(COOKIE).with_session(session.clone()));
        let lib = Library::open_in_memory().unwrap();
        let errors = crate::store::rate_tracks(
            &lib,
            &it,
            true,
            &[song("a"), song("b")],
            music_core::innertube::Rating::Like,
        )
        .unwrap();
        assert_eq!(errors.len(), 2);
        assert!(errors.iter().all(music_core::Error::is_auth));
        assert_eq!(seen.lock().unwrap().len(), 1, "only the first like asked");
        assert!(session.is_expired());
        let errors = crate::store::rate_tracks(
            &lib,
            &it,
            true,
            &[song("c")],
            music_core::innertube::Rating::Like,
        )
        .unwrap();
        assert!(errors[0].is_auth());
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "expired: YouTube isn't asked"
        );
        assert_eq!(lib.pending_ratings().unwrap().len(), 3);
        assert_eq!(describe_error(&errors[0]), t!("account.error.signed_out"));
    }
}
