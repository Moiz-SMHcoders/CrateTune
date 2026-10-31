//! "Sign in with Google…": Google's sign-in page in a web view with
//! music.youtube.com as the destination. Once the web view is on music.youtube.com
//! with a SAPISID cookie, its `.youtube.com` cookies are handed to
//! `MusicApp::sign_in_with_cookie` and the window closes.
//!
//! * macOS ([`macos`]): a GPUI window with a WKWebView child (wry).
//! * Windows ([`webview2`]): a native window with WebView2 on its own UI thread;
//!   GPUI only waits for the answer. Without the WebView2 runtime the app says so and
//!   opens the cookie dialog.
//!
//! **Persistent profile.** The web view keeps its Google session on disk, like a
//! browser, so the app can renew an expired YouTube Music session without the user
//! ([`silent_refresh`]: the same web view, never shown, loads music.youtube.com and
//! the fresh cookies are read the same way as at sign-in):
//!
//! * Windows: the WebView2 user-data folder `%LOCALAPPDATA%\<app id>\data\SignIn`
//!   ([`profile_dir`]).
//! * macOS 14+: a `WKWebsiteDataStore` of its own (`macos::STORE_ID`, under
//!   `~/Library/WebKit/<bundle id>/WebsiteDataStore/`); macOS 11–13: the app's
//!   default data store (`~/Library/WebKit/<bundle id>/`, `~/Library/HTTPStorages/`).
//!
//! **One profile per account** ("Several accounts", `account_list.rs`): each
//! account signed in through the window keeps its own profile (an id in its
//! `AccountConfig::profile`): its own `WKWebsiteDataStore` identifier on macOS 14+,
//! its own folder (`SignIn-<id>`) on Windows. Renewals load the profile of the
//! account in use, so one account's Google session never renews another's. The
//! first account keeps the profile of older versions ([`LEGACY_PROFILE`]: the fixed
//! store id / the `SignIn` folder). macOS 11–13 have one store for the whole app:
//! there, adding an account empties it first, and only the account that signed in
//! last can be renewed in the background ([`separate_profiles`]).
//!
//! **Sign out** clears it ([`clear_profile`]), so signing out really signs out.
//! Google may refuse embedded browsers ("This browser or app may not be secure"); the
//! account settings say so, and the cookie-paste dialog is the fallback. **Not
//! verified with a real Google account** (only that the page loads); neither is
//! whether a background page load renews the session.

#[cfg(target_os = "macos")]
mod macos;
pub(crate) mod profile;
#[cfg(target_os = "windows")]
mod webview2;

use gpui::App;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use gpui::AppContext as _;
use music_core::innertube::Auth;

#[cfg(any(target_os = "macos", target_os = "windows"))]
use crate::app::{app_entity, update_app};

#[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
const LOGIN_URL: &str = "https://accounts.google.com/ServiceLogin?ltmpl=music&service=youtube&continue=https%3A%2F%2Fmusic.youtube.com%2F";
/// Where the flow ends; its cookies are the ones the app needs.
#[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
const MUSIC_URL: &str = "https://music.youtube.com";

/// Whether the session can be renewed in the background (a sign-in web view exists).
pub const SILENT_REFRESH: bool = cfg!(any(target_os = "macos", target_os = "windows"));

/// How long a background renewal may take before it counts as failed.
#[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
const SILENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);
/// "unknown" probe answers (page loaded, no `LOGGED_IN` flag) before the cookies
/// are read anyway.
#[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
const PROBE_PATIENCE: u32 = 6;

/// Run in the music.youtube.com page: `loading`, `true` / `false` (YouTube's own
/// `LOGGED_IN` flag of the page), or `unknown` (loaded, no flag).
#[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
const SIGNED_IN_PROBE: &str = "(function(){try{if(document.readyState!=='complete')return 'loading';var c=window.ytcfg;if(!c||typeof c.get!=='function')return 'unknown';var v=c.get('LOGGED_IN');return v===true?'true':v===false?'false':'unknown'}catch(e){return 'loading'}})()";

/// (name, value, domain) of one cookie (Windows: `webview2::Cookies`).
#[cfg(target_os = "macos")]
type Cookies = Vec<(String, String, String)>;

/// The answer of a background renewal: the new cookie header, or why not (English,
/// for the log; never contains cookie values).
pub type RefreshResult = Result<String, String>;

/// The profile of older versions (one for the whole app): the account that was
/// signed in when "Several accounts" arrived keeps it. The bytes are the fixed
/// macOS store id (`macos::STORE_ID`); on Windows it is the `SignIn` folder.
pub const LEGACY_PROFILE: &str = "28ee6ed96ce34d48ba8115f55584b763";

/// A fresh profile id (32 hex digits).
#[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
pub fn new_profile_id() -> String {
    let id = format!(
        "{}{}",
        crate::settings::library_config::new_account_id(),
        crate::settings::library_config::new_account_id()
    );
    if id == LEGACY_PROFILE {
        new_profile_id()
    } else {
        id
    }
}

/// The 16 bytes of a profile id (a store identifier on macOS 14+).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn profile_bytes(profile: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    let hex = profile.as_bytes();
    for (i, byte) in out.iter_mut().enumerate() {
        let pair = hex
            .get(2 * i..2 * i + 2)
            .and_then(|p| std::str::from_utf8(p).ok());
        *byte = pair
            .and_then(|p| u8::from_str_radix(p, 16).ok())
            .unwrap_or(0);
    }
    // A UUID of zeros is invalid for WebKit: keep one bit set.
    out[15] |= u8::from(out.iter().all(|b| *b == 0));
    out
}

/// Whether each account can have a sign-in profile of its own (macOS 14+, Windows).
#[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
pub fn separate_profiles() -> bool {
    #[cfg(target_os = "macos")]
    return macos::identified_stores();
    #[cfg(target_os = "windows")]
    return true;
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    false
}

/// Which profile a sign-in uses, and whether it must be emptied first.
///
/// * Signing in (again) to the account in use: its profile.
/// * Signing in with nobody using the old profile: that one (an upgrade keeps
///   working the way it did).
/// * Otherwise (`add`ing an account, or the active one has no profile yet): a new
///   one.
/// * With one store for the app (`!separate`): always that store; emptied first when
///   it holds another account's session.
#[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
pub(crate) fn plan_profile(
    add: bool,
    active: Option<&str>,
    in_use: &[String],
    separate: bool,
) -> (String, bool) {
    let legacy_used = in_use.iter().any(|p| p == LEGACY_PROFILE);
    if !separate {
        let owner_is_active = active == Some(LEGACY_PROFILE);
        let clear = legacy_used && (add || !owner_is_active);
        return (LEGACY_PROFILE.to_owned(), clear);
    }
    match active {
        Some(p) if !add => (p.to_owned(), false),
        _ if !legacy_used => (LEGACY_PROFILE.to_owned(), false),
        _ => (new_profile_id(), false),
    }
}

/// The profile for a sign-in from the app's accounts (see [`plan_profile`]).
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn profile_for(add: bool, cx: &App) -> (String, bool) {
    let (active, in_use) =
        crate::app::read_app(cx, |app, cx| app.sign_in_profiles(cx)).unwrap_or_default();
    plan_profile(add, active.as_deref(), &in_use, separate_profiles())
}

/// Opens the sign-in window for the account in use (or the first one; `window`:
/// the main window, to center on).
pub fn open(window: &gpui::Window, cx: &mut App) {
    open_with(window, cx, false);
}

/// Opens the sign-in window for another account ("Add account").
#[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
pub fn open_new(window: &gpui::Window, cx: &mut App) {
    open_with(window, cx, true);
}

fn open_with(window: &gpui::Window, cx: &mut App, add: bool) {
    #[cfg(target_os = "macos")]
    {
        let _ = window;
        let (profile, clear) = profile_for(add, cx);
        if !clear {
            macos::open(profile, cx);
            return;
        }
        // One store for the app: empty it, then open (WebKit answers on the main thread).
        let done = macos::clear_profile(&profile);
        let executor = cx.background_executor().clone();
        cx.spawn(async move |cx| {
            for _ in 0..50 {
                if done.try_recv().is_ok() {
                    break;
                }
                executor.timer(std::time::Duration::from_millis(100)).await;
            }
            cx.update(|cx| macos::open(profile, cx));
        })
        .detach();
    }
    #[cfg(target_os = "windows")]
    {
        let (profile, _) = profile_for(add, cx);
        open_windows(window, profile, cx);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = (window, cx, add);
}

/// Renews the YouTube Music cookie from `profile` without showing anything: loads
/// music.youtube.com hidden, waits until it's loaded and signed in, reads the
/// cookies like the sign-in does. `None` where there is no web view.
pub fn silent_refresh(
    profile: &str,
    cx: &mut App,
) -> Option<futures_channel::oneshot::Receiver<RefreshResult>> {
    #[cfg(target_os = "macos")]
    return Some(macos::silent_refresh(profile, cx));
    #[cfg(target_os = "windows")]
    return Some(silent_refresh_windows(profile, cx));
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (profile, cx);
        None
    }
}

/// Forgets the Google session of `profile` (sign-out, removing an account).
pub fn clear_profile(profile: &str, cx: &mut App) {
    #[cfg(target_os = "macos")]
    {
        let _ = cx;
        let _ = macos::clear_profile(profile);
    }
    #[cfg(target_os = "windows")]
    {
        let _ = cx;
        let dir = profile_dir(profile);
        let cancelled = webview2::cancel_silent();
        // Its own thread: retrying may take a few seconds.
        let _ = std::thread::Builder::new()
            .name("sign-in-profile-clear".into())
            .spawn(move || {
                if cancelled {
                    // Let the hidden web view close first.
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                if !profile::clear_dir_patiently(&dir) {
                    eprintln!(
                        "music-gpui: the sign-in profile is in use; it is deleted before its next use"
                    );
                }
            });
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = (profile, cx);
}

/// The WebView2 user-data folder of sign-in `profile` (Windows): `SignIn` for
/// [`LEGACY_PROFILE`], `SignIn-<id>` for the others.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn profile_dir(profile: &str) -> std::path::PathBuf {
    let name = if profile == LEGACY_PROFILE {
        "SignIn".to_owned()
    } else {
        // Only hex digits reach the file system.
        let id: String = profile.chars().filter(char::is_ascii_hexdigit).collect();
        format!("SignIn-{id}")
    };
    crate::identity::project_dirs()
        .map(|d| d.data_local_dir().join(&name))
        .unwrap_or_else(|| {
            std::env::temp_dir()
                .join(crate::identity::APP_ID)
                .join(&name)
        })
}

/// `name=value; …` of the youtube.com cookies, if they can sign requests.
#[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
fn youtube_cookie(cookies: &[(String, String, String)]) -> Option<String> {
    let mut seen = std::collections::HashSet::new();
    let header = cookies
        .iter()
        .filter(|(_, _, domain)| {
            let d = domain.trim_start_matches('.');
            d == "youtube.com" || d.ends_with(".youtube.com")
        })
        // A more specific domain (music.youtube.com) comes first if both exist.
        .filter(|(name, _, _)| seen.insert(name.clone()))
        .map(|(name, value, _)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ");
    Auth::new(header.clone()).is_signed_in().then_some(header)
}

/// What a page probe said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
enum Probe {
    /// Still loading: ask again.
    Wait,
    /// YouTube says signed in: read the cookies.
    SignedIn,
    /// YouTube says signed out: the profile's session is gone too.
    SignedOut,
}

/// Reads the [`SIGNED_IN_PROBE`] answer (WebView2 hands it over JSON-encoded).
/// `unknown` counts as signed in after [`PROBE_PATIENCE`] answers (the cookie check
/// and the retried request decide then).
#[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
fn read_probe(answer: &str, unknown_seen: &mut u32) -> Probe {
    match answer.trim().trim_matches('"') {
        "true" => Probe::SignedIn,
        "false" => Probe::SignedOut,
        "unknown" => {
            *unknown_seen += 1;
            if *unknown_seen >= PROBE_PATIENCE {
                Probe::SignedIn
            } else {
                Probe::Wait
            }
        }
        _ => Probe::Wait,
    }
}

/// Gives the cookie (from `profile`) to the main window's app (notifications go
/// there).
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn hand_over(cookie: String, profile: String, cx: &mut App) {
    let Some(app) = app_entity(cx) else { return };
    let Some(main) = app.read(cx).main_window else {
        return;
    };
    let _ = cx.update_window(main, move |_, window, cx| {
        update_app(cx, |app, cx| {
            app.sign_in_from(cookie, 0, Some(profile), window, cx)
        });
    });
}

/// Starts the WebView2 window's thread and waits (without blocking) for its answer.
#[cfg(target_os = "windows")]
fn open_windows(window: &gpui::Window, profile: String, cx: &mut App) {
    use self::webview2::{Failure, Outcome, Request, Start};

    let main_hwnd = crate::services::hwnd(window).map(|h| h.get());
    let (tx, rx) = futures_channel::oneshot::channel();
    let request = Request {
        title: t!("login.window_title").to_owned(),
        url: LOGIN_URL,
        cookies_url: MUSIC_URL,
        data_dir: profile_dir(&profile),
        center_on: main_hwnd,
        accept: |cookies| youtube_cookie(cookies).is_some(),
        silent: false,
        trace: std::env::var_os("MUSIC_TRACE").is_some(),
    };
    let executor = cx.background_executor().clone();
    cx.spawn(async move |cx| {
        // A background renewal running is asked to stop; the window opens once it has.
        let mut pending = Some((request, tx));
        for _ in 0..100 {
            let Some((request, tx)) = pending.take() else {
                break;
            };
            match webview2::start(request, tx) {
                Start::Started => break,
                // Already open: it was brought to the front.
                Start::BroughtToFront => return,
                Start::Busy(request, tx) => {
                    pending = Some((request, tx));
                    executor.timer(std::time::Duration::from_millis(200)).await;
                }
            }
        }
        if pending.is_some() {
            return;
        }
        let Ok(outcome) = rx.await else { return };
        cx.update(|cx| match outcome {
            Outcome::Cookies(cookies) => {
                if let Some(cookie) = youtube_cookie(&cookies) {
                    hand_over(cookie, profile, cx);
                }
            }
            Outcome::Closed | Outcome::SignedOut => {}
            Outcome::Failed(failure) => {
                let message = match failure {
                    Failure::RuntimeMissing => t!("login.webview2_missing").to_owned(),
                    Failure::Error(error) => t!("login.webview_failed", error = error),
                };
                eprintln!("music-gpui: sign-in window: {message}");
                let Some(main) = app_entity(cx).and_then(|app| app.read(cx).main_window) else {
                    return;
                };
                let _ = cx.update_window(main, move |_, window, cx| {
                    crate::app::notify(window, cx, message);
                    crate::settings::open_cookie_dialog(window, cx);
                });
            }
        });
    })
    .detach();
}

/// The hidden WebView2 renewal (Windows).
#[cfg(target_os = "windows")]
fn silent_refresh_windows(
    profile: &str,
    cx: &mut App,
) -> futures_channel::oneshot::Receiver<RefreshResult> {
    use self::webview2::{Failure, Outcome, Request, Start};

    let (answer_tx, answer_rx) = futures_channel::oneshot::channel();
    let (tx, rx) = futures_channel::oneshot::channel();
    let request = Request {
        title: String::new(),
        url: MUSIC_URL,
        cookies_url: MUSIC_URL,
        data_dir: profile_dir(profile),
        center_on: None,
        accept: |cookies| youtube_cookie(cookies).is_some(),
        silent: true,
        trace: std::env::var_os("MUSIC_TRACE").is_some(),
    };
    if !matches!(webview2::start(request, tx), Start::Started) {
        let _ = answer_tx.send(Err("the sign-in window is open".to_owned()));
        return answer_rx;
    }
    // On the main thread: background threads may all be busy with the requests
    // waiting for this renewal.
    cx.spawn(async move |_| {
        let result = match rx.await {
            Ok(Outcome::Cookies(cookies)) => youtube_cookie(&cookies)
                .ok_or_else(|| "no sign-in cookie in the profile".to_owned()),
            Ok(Outcome::SignedOut) => Err("the profile is signed out".to_owned()),
            Ok(Outcome::Closed) => Err("cancelled".to_owned()),
            Ok(Outcome::Failed(Failure::RuntimeMissing)) => Err("no WebView2 runtime".to_owned()),
            Ok(Outcome::Failed(Failure::Error(e))) => Err(e),
            Err(_) => Err("the web view thread ended".to_owned()),
        };
        let _ = answer_tx.send(result);
    })
    .detach();
    answer_rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_youtube_cookies() {
        let c = |n: &str, v: &str, d: &str| (n.to_owned(), v.to_owned(), d.to_owned());
        let cookies = vec![
            c("SID", "1", ".google.com"),
            c("SAPISID", "abc", ".youtube.com"),
            c("SID", "2", ".youtube.com"),
            c("PREF", "x", "music.youtube.com"),
        ];
        assert_eq!(
            youtube_cookie(&cookies).as_deref(),
            Some("SAPISID=abc; SID=2; PREF=x")
        );
        assert_eq!(youtube_cookie(&cookies[..1]), None);
    }

    #[test]
    fn each_account_signs_in_with_its_own_profile() {
        let none: Vec<String> = Vec::new();
        // The first sign-in keeps the profile of older versions.
        assert_eq!(
            plan_profile(false, None, &none, true),
            (LEGACY_PROFILE.to_owned(), false)
        );
        let legacy = vec![LEGACY_PROFILE.to_owned()];
        // Signing in again: the account's own profile.
        assert_eq!(
            plan_profile(false, Some(LEGACY_PROFILE), &legacy, true),
            (LEGACY_PROFILE.to_owned(), false)
        );
        // Adding another account: a new profile, nothing emptied.
        let (p, clear) = plan_profile(true, Some(LEGACY_PROFILE), &legacy, true);
        assert!(p != LEGACY_PROFILE && p.len() == 32 && !clear);
        assert_ne!(plan_profile(true, Some(LEGACY_PROFILE), &legacy, true).0, p);
        // Signing in again to an account without a profile (it pasted a cookie).
        let (p, _) = plan_profile(false, None, &legacy, true);
        assert_ne!(p, LEGACY_PROFILE);
        // One store for the app (macOS 11–13): emptied before another account uses it.
        assert_eq!(
            plan_profile(true, Some(LEGACY_PROFILE), &legacy, false),
            (LEGACY_PROFILE.to_owned(), true)
        );
        assert_eq!(
            plan_profile(false, Some(LEGACY_PROFILE), &legacy, false),
            (LEGACY_PROFILE.to_owned(), false)
        );
        assert_eq!(
            plan_profile(false, None, &legacy, false),
            (LEGACY_PROFILE.to_owned(), true),
            "the store holds another account's session"
        );
        assert_eq!(
            plan_profile(true, None, &none, false),
            (LEGACY_PROFILE.to_owned(), false)
        );
    }

    #[test]
    fn profile_ids_and_folders() {
        let id = new_profile_id();
        assert!(id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(
            profile_bytes(LEGACY_PROFILE),
            [
                0x28, 0xEE, 0x6E, 0xD9, 0x6C, 0xE3, 0x4D, 0x48, 0xBA, 0x81, 0x15, 0xF5, 0x55, 0x84,
                0xB7, 0x63
            ]
        );
        #[cfg(target_os = "macos")]
        assert_eq!(profile_bytes(LEGACY_PROFILE), macos::STORE_ID);
        assert_ne!(profile_bytes("zz"), [0u8; 16]);
        assert!(profile_dir(LEGACY_PROFILE).ends_with("SignIn"));
        assert!(profile_dir("ab12/../x").ends_with("SignIn-ab12"));
    }

    #[test]
    fn probe_answers() {
        let mut unknown = 0;
        assert_eq!(read_probe("\"true\"", &mut unknown), Probe::SignedIn);
        assert_eq!(read_probe("false", &mut unknown), Probe::SignedOut);
        assert_eq!(read_probe("\"loading\"", &mut unknown), Probe::Wait);
        assert_eq!(read_probe("null", &mut unknown), Probe::Wait);
        for _ in 1..PROBE_PATIENCE {
            assert_eq!(read_probe("\"unknown\"", &mut unknown), Probe::Wait);
        }
        assert_eq!(read_probe("unknown", &mut unknown), Probe::SignedIn);
    }
}
