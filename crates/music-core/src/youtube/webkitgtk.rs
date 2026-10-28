//! BotGuard in a hidden WebKitGTK web view (Linux; opt-in feature
//! `webkitgtk-potoken`, needs `libwebkit2gtk-4.1` ≥ 2.40 at build and run time).
//!
//! GTK may only be used from the thread that initialized it, so one dedicated
//! thread calls `gtk::init`, owns the default GLib main context and runs a main
//! loop; requests arrive through a GLib channel (no polling). The view lives in
//! an `OffscreenWindow` and loads `assets/po_token.html` with
//! `load_html(…, "https://www.youtube.com/")`; calls go through
//! `call_async_javascript_function`, which awaits promises like WebKit's
//! `callAsyncJavaScript` on macOS. [`BotGuardVm::release`] drops the view; the
//! thread stays (GTK can't be initialized twice).
//!
//! Untested: written and type-checked on macOS (webkit2gtk 2.0 with v2_40 for
//! `x86_64-unknown-linux-gnu`). If the process already initialized GTK on
//! another thread, `gtk::init` fails here and there is no PoToken provider.

use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use gtk::prelude::*;
use javascriptcore::ValueExt as _;
use webkit2gtk::{SettingsExt as _, WebView, WebViewExt, gio, glib};

use super::potoken::PO_TOKEN_HTML;
use super::webview_host::{JsHost, PAGE_TIMEOUT};
use crate::{Error, Result};

/// The engine is WebKit: present it as Safari-like WebKit on Linux.
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15";

enum Job {
    Load(Sender<Result<()>>),
    Call(String, Sender<Result<String>>),
    Release,
}

/// Runs BotGuard in WebKitGTK on its own thread (made on first use).
#[derive(Default)]
pub struct WebKitGtkHost {
    jobs: Mutex<Option<glib::Sender<Job>>>,
}

fn gtk_err(context: &str, e: impl std::fmt::Display) -> Error {
    Error::Audio(format!("BotGuard (WebKitGTK) {context}: {e}"))
}

impl WebKitGtkHost {
    fn send(&self, job: Job) -> Result<()> {
        let mut guard = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        if guard.is_none() {
            *guard = Some(spawn()?);
        }
        guard
            .as_ref()
            .expect("just made")
            .send(job)
            .map_err(|_| gtk_err("thread", "gone"))
    }
}

impl JsHost for WebKitGtkHost {
    fn load(&self) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        self.send(Job::Load(tx))?;
        rx.recv_timeout(PAGE_TIMEOUT)
            .map_err(|_| gtk_err("load", "no answer"))?
    }

    fn call(&self, body: String, timeout: Duration) -> Result<String> {
        let (tx, rx) = mpsc::channel();
        self.send(Job::Call(body, tx))?;
        rx.recv_timeout(timeout)
            .map_err(|_| gtk_err("script", "timed out"))?
    }

    fn release(&self) {
        let guard = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(jobs) = guard.as_ref() {
            let _ = jobs.send(Job::Release);
        }
    }

    fn user_agent(&self) -> &'static str {
        USER_AGENT
    }
}

fn spawn() -> Result<glib::Sender<Job>> {
    let (started_tx, started_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("botguard-webkitgtk".into())
        .spawn(move || run(&started_tx))
        .map_err(Error::Io)?;
    started_rx
        .recv_timeout(Duration::from_secs(10))
        .map_err(|_| gtk_err("thread", "did not start"))?
}

fn run(started: &Sender<Result<glib::Sender<Job>>>) {
    if let Err(e) = gtk::init() {
        let _ = started.send(Err(gtk_err("init (no display?)", e)));
        return;
    }
    let context = glib::MainContext::default();
    let Ok(_owner) = context.acquire() else {
        let _ = started.send(Err(gtk_err("main context", "owned by another thread")));
        return;
    };
    let main_loop = glib::MainLoop::new(Some(&context), false);
    #[allow(deprecated)] // glib 0.18's channel; fine for this one-thread loop
    let (tx, rx) = glib::MainContext::channel::<Job>(glib::Priority::DEFAULT);
    let mut page: Option<(gtk::OffscreenWindow, WebView)> = None;
    #[allow(deprecated)]
    rx.attach(Some(&context), move |job| {
        match job {
            Job::Load(reply) => {
                if let Some((window, _)) = page.take() {
                    // SAFETY: our own top-level, destroyed on its GTK thread.
                    unsafe { window.destroy() };
                }
                let view = WebView::new();
                if let Some(settings) = WebViewExt::settings(&view) {
                    settings.set_user_agent(Some(USER_AGENT));
                    settings.set_enable_developer_extras(false);
                }
                let window = gtk::OffscreenWindow::new();
                window.add(&view);
                window.show_all();
                view.load_html(PO_TOKEN_HTML, Some("https://www.youtube.com/"));
                page = Some((window, view));
                // Readiness is polled with `typeof runBotGuard` by the caller.
                let _ = reply.send(Ok(()));
            }
            Job::Call(body, reply) => match &page {
                Some((_, view)) => view.call_async_javascript_function(
                    &body,
                    None,
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    move |result| {
                        let _ = reply.send(match result {
                            Ok(value) => Ok(value.to_str().to_string()),
                            Err(e) => Err(Error::Audio(format!("BotGuard script failed: {e}"))),
                        });
                    },
                ),
                None => {
                    let _ = reply.send(Err(gtk_err("script", "no page loaded")));
                }
            },
            Job::Release => {
                if let Some((window, _)) = page.take() {
                    // SAFETY: as above.
                    unsafe { window.destroy() };
                }
            }
        }
        glib::ControlFlow::Continue
    });
    let _ = started.send(Ok(tx));
    main_loop.run();
}
