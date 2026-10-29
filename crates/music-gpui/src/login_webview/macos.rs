//! macOS sign-in window: a GPUI window whose content below a small status bar
//! is a WKWebView (wry, `build_as_child` on the window's NSView) showing Google's
//! sign-in page. The web view uses a non-persistent data store. The cookie store is
//! read with `getAllCookies` + a channel polled from a GPUI timer instead of wry's
//! `cookies()`, which spins the main run loop and would re-enter GPUI from inside an
//! update.

use std::ptr::NonNull;
use std::sync::mpsc;
use std::time::Duration;

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
use objc2_foundation::{NSArray, NSHTTPCookie};
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder, WebViewExtMacOS as _};

use super::{Cookies, LOGIN_URL, hand_over, youtube_cookie};

/// Safari's user agent: Google serves its normal sign-in flow to WebKit with it.
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
     (KHTML, like Gecko) Version/18.5 Safari/605.1.15";
const BAR_HEIGHT: f32 = 44.;

pub(super) struct LoginWindow {
    webview: Option<WebView>,
    status: String,
    cookies: Option<mpsc::Receiver<Cookies>>,
    size: (f32, f32),
    done: bool,
    /// `MUSIC_TRACE`: the page last logged.
    traced: Option<String>,
    _poll: Task<()>,
}

/// Opens the sign-in window (one at a time is enough; a second call opens another).
pub(super) fn open(cx: &mut App) {
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
        cx.new(|cx| LoginWindow::new(window, cx))
    }) {
        eprintln!("music-gpui: login window: {e}");
    }
}

impl LoginWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let viewport = window.viewport_size();
        let (w, h) = (f32::from(viewport.width), f32::from(viewport.height));
        let built = WebViewBuilder::new()
            .with_url(LOGIN_URL)
            .with_user_agent(USER_AGENT)
            .with_incognito(true)
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
        let wk = webview.webview();
        // SAFETY: main thread; the configuration's data store and its cookie store are
        // owned by the web view, which outlives the call (the block is retained by WebKit).
        unsafe {
            let store = wk.configuration().websiteDataStore().httpCookieStore();
            store.getAllCookies(&block);
        }
        self.cookies = Some(rx);
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
                hand_over(cookie, cx);
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
