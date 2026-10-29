//! "Sign in with Google…": Google's sign-in page in a web view with
//! music.youtube.com as the destination. Once the web view is on music.youtube.com
//! with a SAPISID cookie, its `.youtube.com` cookies are handed to
//! `MusicApp::sign_in_with_cookie` and the window closes.
//!
//! * macOS ([`macos`]): a GPUI window with a WKWebView child (wry), non-persistent
//!   data store.
//! * Windows ([`webview2`]): a native window with WebView2 on its own UI thread, in
//!   InPrivate mode with its own user-data folder; GPUI only waits for the answer.
//!   Without the WebView2 runtime the app says so and opens the cookie dialog.
//!
//! Nothing of the Google session is kept besides the cookie the app stores in the
//! keychain. Google may refuse embedded browsers ("This browser or app may not be
//! secure"); the account settings say so, and the cookie-paste dialog is the
//! fallback. **Not verified with a real Google account** (only that the page loads).

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod webview2;

use gpui::{App, AppContext as _};
use music_core::innertube::Auth;

use crate::app::{app_entity, update_app};

const LOGIN_URL: &str = "https://accounts.google.com/ServiceLogin?ltmpl=music&service=youtube&continue=https%3A%2F%2Fmusic.youtube.com%2F";
/// Where the flow ends; its cookies are the ones the app needs.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
const MUSIC_URL: &str = "https://music.youtube.com";

/// (name, value, domain) of one cookie (Windows: `webview2::Cookies`).
#[cfg(target_os = "macos")]
type Cookies = Vec<(String, String, String)>;

/// Opens the sign-in window (`window`: the main window, to center on).
pub fn open(window: &gpui::Window, cx: &mut App) {
    #[cfg(target_os = "macos")]
    {
        let _ = window;
        macos::open(cx);
    }
    #[cfg(target_os = "windows")]
    open_windows(window, cx);
}

/// `name=value; …` of the youtube.com cookies, if they can sign requests.
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

/// Gives the cookie to the main window's app (notifications go there).
fn hand_over(cookie: String, cx: &mut App) {
    let Some(app) = app_entity(cx) else { return };
    let Some(main) = app.read(cx).main_window else {
        return;
    };
    let _ = cx.update_window(main, move |_, window, cx| {
        update_app(cx, |app, cx| app.sign_in_with_cookie(cookie, 0, window, cx));
    });
}

/// Starts the WebView2 window's thread and waits (without blocking) for its answer.
#[cfg(target_os = "windows")]
fn open_windows(window: &gpui::Window, cx: &mut App) {
    use self::webview2::{Failure, Outcome, Request};

    let main_hwnd = crate::services::hwnd(window).map(|h| h.get());
    let data_dir = crate::identity::project_dirs()
        .map(|d| d.data_local_dir().join("SignIn"))
        .unwrap_or_else(|| {
            std::env::temp_dir()
                .join(crate::identity::APP_ID)
                .join("SignIn")
        });
    let (tx, rx) = futures_channel::oneshot::channel();
    let request = Request {
        title: t!("login.window_title").to_owned(),
        url: LOGIN_URL,
        cookies_url: MUSIC_URL,
        data_dir,
        center_on: main_hwnd,
        accept: |cookies| youtube_cookie(cookies).is_some(),
        trace: std::env::var_os("MUSIC_TRACE").is_some(),
    };
    if !webview2::start(request, tx) {
        // Already open: it was brought to the front.
        return;
    }
    cx.spawn(async move |cx| {
        let Ok(outcome) = rx.await else { return };
        cx.update(|cx| match outcome {
            Outcome::Cookies(cookies) => {
                if let Some(cookie) = youtube_cookie(&cookies) {
                    hand_over(cookie, cx);
                }
            }
            Outcome::Closed => {}
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
}
