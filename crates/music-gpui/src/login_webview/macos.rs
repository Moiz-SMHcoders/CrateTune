//! macOS sign-in window: a GPUI window whose content below a small status bar
//! is a WKWebView (wry, `build_as_child` on the window's NSView) showing Google's
//! sign-in page. The cookie store is read with `getAllCookies` + a channel polled
//! from a GPUI timer instead of wry's `cookies()`, which spins the main run loop and
//! would re-enter GPUI from inside an update.
//!
//! The web view's data store is persistent ([`sign_in_store`]): on macOS 14+ one
//! `WKWebsiteDataStore` per account's sign-in profile (identified by the profile
//! id; the first account's is [`STORE_ID`]), the app's default store before.
//! [`silent_refresh`] loads music.youtube.com in a WKWebView on that store that is
//! never put in a window; [`clear_profile`] empties the store (sign-out).

use std::cell::RefCell;
use std::ptr::NonNull;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use block2::RcBlock;
use gpui::{
    App, AppContext as _, Bounds, Context, IntoElement, ParentElement as _, Render, Styled as _,
    Task, TitlebarOptions, Window, WindowBounds, WindowOptions, div, px, size,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, MainThreadOnly as _};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{
    NSArray, NSDate, NSError, NSHTTPCookie, NSProcessInfo, NSString, NSURL, NSURLRequest, NSUUID,
};
use objc2_web_kit::{WKWebView, WKWebViewConfiguration, WKWebsiteDataStore};
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder, WebViewBuilderExtDarwin as _, WebViewExtMacOS as _};

use super::{
    Cookies, LOGIN_URL, MUSIC_URL, Probe, RefreshResult, SIGNED_IN_PROBE, SILENT_TIMEOUT,
    hand_over, profile_bytes, read_probe, youtube_cookie,
};

/// The first sign-in data store's identifier (macOS 14+; any fixed UUID of this app;
/// `LEGACY_PROFILE` spells it in hex).
#[cfg_attr(not(test), allow(dead_code))]
pub(super) const STORE_ID: [u8; 16] = [
    0x28, 0xEE, 0x6E, 0xD9, 0x6C, 0xE3, 0x4D, 0x48, 0xBA, 0x81, 0x15, 0xF5, 0x55, 0x84, 0xB7, 0x63,
];

/// Whether `WKWebsiteDataStore(forIdentifier:)` exists (macOS 14+), as wry decides.
pub(super) fn identified_stores() -> bool {
    NSProcessInfo::processInfo()
        .operatingSystemVersion()
        .majorVersion
        >= 14
}

/// The persistent store of sign-in `profile` (the same one wry picks for
/// `with_data_store_identifier(profile_bytes(profile))`).
fn sign_in_store(profile: &str, mtm: MainThreadMarker) -> Retained<WKWebsiteDataStore> {
    // SAFETY: main thread; the identifier is non-zero (`profile_bytes`).
    unsafe {
        if identified_stores() {
            WKWebsiteDataStore::dataStoreForIdentifier(
                &NSUUID::from_bytes(profile_bytes(profile)),
                mtm,
            )
        } else {
            WKWebsiteDataStore::defaultDataStore(mtm)
        }
    }
}

/// Reads every cookie of `store`; the answer arrives on the returned channel.
fn all_cookies(store: &WKWebsiteDataStore) -> mpsc::Receiver<Cookies> {
    let (tx, rx) = mpsc::channel::<Cookies>();
    let block = RcBlock::new(move |cookies: NonNull<NSArray<NSHTTPCookie>>| {
        // SAFETY: WebKit passes a valid array for the duration of the callback.
        let cookies = unsafe { cookies.as_ref() };
        let list = cookies
            .to_vec()
            .into_iter()
            .map(|c| {
                (
                    c.name().to_string(),
                    c.value().to_string(),
                    c.domain().to_string(),
                )
            })
            .collect();
        let _ = tx.send(list);
    });
    // SAFETY: main thread; the store outlives the call (WebKit retains the block).
    unsafe { store.httpCookieStore().getAllCookies(&block) };
    rx
}

/// Empties `profile`'s data store: cookies, storage, caches (sign-out). The
/// receiver hears when WebKit is done.
pub(super) fn clear_profile(profile: &str) -> mpsc::Receiver<()> {
    let (tx, rx) = mpsc::channel();
    let Some(mtm) = MainThreadMarker::new() else {
        let _ = tx.send(());
        return rx;
    };
    let store = sign_in_store(profile, mtm);
    let done = RcBlock::new(move || {
        let _ = tx.send(());
    });
    // SAFETY: main thread; WebKit retains the block until it runs.
    unsafe {
        store.removeDataOfTypes_modifiedSince_completionHandler(
            &WKWebsiteDataStore::allWebsiteDataTypes(mtm),
            &NSDate::distantPast(),
            &done,
        );
    }
    rx
}

thread_local! {
    /// A background renewal is running (one at a time).
    static SILENT_BUSY: RefCell<bool> = const { RefCell::new(false) };
}

/// Loads music.youtube.com in a hidden WKWebView on `profile`'s store and reads its
/// cookies once the page says it is signed in. Main thread (GPUI's).
pub(super) fn silent_refresh(
    profile: &str,
    cx: &mut App,
) -> futures_channel::oneshot::Receiver<RefreshResult> {
    let (tx, rx) = futures_channel::oneshot::channel();
    let Some(mtm) = MainThreadMarker::new() else {
        let _ = tx.send(Err("not on the main thread".into()));
        return rx;
    };
    if SILENT_BUSY.with(|b| b.replace(true)) {
        let _ = tx.send(Err("already renewing".into()));
        return rx;
    }
    // SAFETY: plain WebKit object creation on the main thread.
    let view = unsafe {
        let config = WKWebViewConfiguration::new(mtm);
        config.setWebsiteDataStore(&sign_in_store(profile, mtm));
        let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1024.0, 768.0));
        let view = WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &config);
        view.setCustomUserAgent(Some(&NSString::from_str(USER_AGENT)));
        if let Some(url) = NSURL::URLWithString(&NSString::from_str(MUSIC_URL)) {
            view.loadRequest(&NSURLRequest::requestWithURL(&url));
        }
        view
    };
    let executor = cx.background_executor().clone();
    cx.spawn(async move |_| {
        let started = Instant::now();
        let mut probe: Option<mpsc::Receiver<String>> = None;
        let mut cookies: Option<mpsc::Receiver<Cookies>> = None;
        let mut unknown_seen = 0;
        let result = loop {
            executor.timer(Duration::from_millis(1000)).await;
            if started.elapsed() > SILENT_TIMEOUT {
                break Err("timed out".to_owned());
            }
            if let Some(rx) = &cookies {
                match rx.try_recv() {
                    Ok(list) => {
                        break youtube_cookie(&list)
                            .ok_or_else(|| "no sign-in cookie in the profile".to_owned());
                    }
                    Err(mpsc::TryRecvError::Empty) => continue,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        break Err("the cookie store didn't answer".to_owned());
                    }
                }
            }
            // SAFETY: property reads on the live web view, on the main thread.
            let url = unsafe { view.URL() }
                .and_then(|u| u.absoluteString())
                .map(|s| s.to_string())
                .unwrap_or_default();
            if url.starts_with("https://accounts.google.com") {
                break Err("the profile is signed out".to_owned());
            }
            // SAFETY: as above.
            if !url.starts_with(MUSIC_URL) || unsafe { view.isLoading() } {
                continue;
            }
            match probe.as_ref().map(|rx| rx.try_recv()) {
                None => probe = Some(evaluate(&view, SIGNED_IN_PROBE)),
                Some(Err(mpsc::TryRecvError::Empty)) => {}
                Some(answer) => {
                    probe = None;
                    match read_probe(&answer.unwrap_or_default(), &mut unknown_seen) {
                        Probe::Wait => {}
                        Probe::SignedOut => break Err("the profile is signed out".to_owned()),
                        // SAFETY: main thread (this task runs on GPUI's).
                        Probe::SignedIn => {
                            let store = unsafe { view.configuration().websiteDataStore() };
                            cookies = Some(all_cookies(&store));
                        }
                    }
                }
            }
        };
        // SAFETY: main thread.
        unsafe { view.stopLoading() };
        drop(view);
        SILENT_BUSY.with(|b| *b.borrow_mut() = false);
        let _ = tx.send(result);
    })
    .detach();
    rx
}

/// Runs `script` in the page; its string result arrives on the channel ("" on errors).
fn evaluate(view: &WKWebView, script: &str) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    let tx = RefCell::new(Some(tx));
    let handler = RcBlock::new(move |value: *mut AnyObject, _error: *mut NSError| {
        let Some(tx) = tx.borrow_mut().take() else {
            return;
        };
        // SAFETY: WebKit passes a valid (or null) object for the duration of the call.
        let text = unsafe { value.as_ref() }
            .and_then(|v| v.downcast_ref::<NSString>())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let _ = tx.send(text);
    });
    // SAFETY: main thread; WebKit retains the handler until it runs.
    unsafe {
        view.evaluateJavaScript_completionHandler(&NSString::from_str(script), Some(&handler))
    };
    rx
}

/// Safari's user agent: Google serves its normal sign-in flow to WebKit with it.
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
     (KHTML, like Gecko) Version/18.5 Safari/605.1.15";
const BAR_HEIGHT: f32 = 44.;

pub(super) struct LoginWindow {
    /// The sign-in profile (its data store) this window signs in with.
    profile: String,
    webview: Option<WebView>,
    status: String,
    cookies: Option<mpsc::Receiver<Cookies>>,
    size: (f32, f32),
    done: bool,
    /// `MUSIC_TRACE`: the page last logged.
    traced: Option<String>,
    _poll: Task<()>,
}

/// Opens the sign-in window on `profile` (one at a time is enough; a second call
/// opens another).
pub(super) fn open(profile: String, cx: &mut App) {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(520.), px(740.)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some(t!("login.window_title").into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    if let Err(e) = cx.open_window(options, |window, cx| {
        cx.new(|cx| LoginWindow::new(profile, window, cx))
    }) {
        eprintln!("music-gpui: login window: {e}");
    }
}

impl LoginWindow {
    fn new(profile: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let viewport = window.viewport_size();
        let (w, h) = (f32::from(viewport.width), f32::from(viewport.height));
        // Persistent store (see the module docs): the Google session stays for
        // background renewals until sign-out.
        let built = WebViewBuilder::new()
            .with_url(LOGIN_URL)
            .with_user_agent(USER_AGENT)
            .with_incognito(false)
            .with_data_store_identifier(profile_bytes(&profile))
            .with_bounds(bounds(w, h))
            .build_as_child(&*window);
        let (webview, status) = match built {
            Ok(webview) => (Some(webview), t!("login.instructions").to_owned()),
            Err(e) => (None, t!("login.webview_failed", error = e)),
        };
        let executor = cx.background_executor().clone();
        let poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(1000)).await;
                let keep = this
                    .update_in(cx, |this, window, cx| this.tick(window, cx))
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        });
        Self {
            profile,
            webview,
            status,
            cookies: None,
            size: (w, h),
            done: false,
            traced: None,
            _poll: poll,
        }
    }

    fn current_url(&self) -> Option<String> {
        let webview = self.webview.as_ref()?.webview();
        // SAFETY: plain property reads on the live WKWebView, on the main thread.
        let url = unsafe { webview.URL() }?;
        let text = url.absoluteString()?;
        Some(text.to_string())
    }

    /// Asks WebKit for every cookie; the answer arrives on `self.cookies`.
    fn request_cookies(&mut self) {
        let Some(webview) = &self.webview else { return };
        let wk = webview.webview();
        // SAFETY: main thread; the data store is owned by the live web view.
        let store = unsafe { wk.configuration().websiteDataStore() };
        self.cookies = Some(all_cookies(&store));
    }

    /// Returns whether to keep polling.
    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.done {
            return false;
        }
        // Keep the web view sized to the window.
        let viewport = window.viewport_size();
        let size = (f32::from(viewport.width), f32::from(viewport.height));
        if size != self.size {
            self.size = size;
            if let Some(webview) = &self.webview {
                let _ = webview.set_bounds(bounds(size.0, size.1));
            }
        }
        if let Some(rx) = &self.cookies
            && let Ok(cookies) = rx.try_recv()
        {
            self.cookies = None;
            if let Some(cookie) = youtube_cookie(&cookies) {
                self.done = true;
                self.status = t!("login.checking").into();
                cx.notify();
                hand_over(cookie, self.profile.clone(), cx);
                window.remove_window();
                return false;
            }
        }
        let on_music = self
            .current_url()
            .is_some_and(|u| u.starts_with("https://music.youtube.com"));
        if on_music && self.cookies.is_none() {
            self.request_cookies();
        }
        let page = self
            .current_url()
            .map(|u| u.split('?').next().unwrap_or_default().to_owned());
        if std::env::var_os("MUSIC_TRACE").is_some() && page != self.traced {
            self.traced = page.clone();
            eprintln!("music-gpui: login window at {}", page.unwrap_or_default());
            // What the page says (e.g. Google refusing embedded browsers).
            if let Some(webview) = &self.webview {
                let _ = webview.evaluate_script_with_callback(
                    "document.title + ' | ' + (document.body ? document.body.innerText.slice(0, 160) : '')",
                    |text| eprintln!("music-gpui: login page: {}", text.replace('\n', " ")),
                );
            }
        }
        true
    }
}

fn bounds(width: f32, height: f32) -> Rect {
    Rect {
        position: LogicalPosition::new(0., BAR_HEIGHT as f64).into(),
        size: LogicalSize::new(width as f64, (height - BAR_HEIGHT).max(0.) as f64).into(),
    }
}

impl Render for LoginWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                h_flex()
                    .h(px(BAR_HEIGHT))
                    .px_3()
                    .gap_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .line_clamp(2)
                            .child(self.status.clone()),
                    )
                    .child(
                        Button::new("login-cancel")
                            .ghost()
                            .small()
                            .label(t!("common.cancel"))
                            .on_click(|_, window, _| window.remove_window()),
                    ),
            )
    }
}
