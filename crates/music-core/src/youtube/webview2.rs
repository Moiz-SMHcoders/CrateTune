//! BotGuard in a hidden WebView2 (Windows; feature `webview2-potoken`).
//!
//! WebView2 is single-threaded apartment COM with callbacks delivered through
//! the window message loop, so it lives on a dedicated thread: a never-shown
//! window, an environment (user data in `%LOCALAPPDATA%\music-macos\BotGuard`),
//! a controller and its `ICoreWebView2`. Requests arrive over a channel; the
//! thread is woken with `PostThreadMessageW(WM_APP)` and otherwise blocks in
//! `GetMessageW` (no polling). The page is served for
//! `https://www.youtube.com/botguard-po-token` through `WebResourceRequested`
//! (the same origin `loadHTMLString(baseURL:)` gives on macOS). `ExecuteScript`
//! can't await, so each call is wrapped to `postMessage` its result back
//! ([`super::webview_host::wrap_async_call`]). [`BotGuardVm::release`] closes
//! the controller and ends the thread (the browser processes go with it).
//!
//! Untested: written and type-checked on macOS against webview2-com 0.39 for
//! `x86_64-pc-windows-gnu`; needs the WebView2 runtime (part of Windows 10/11).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL, CreateCoreWebView2EnvironmentWithOptions, ICoreWebView2,
    ICoreWebView2Controller, ICoreWebView2Environment, ICoreWebView2Settings2,
};
use webview2_com::{
    CoreWebView2EnvironmentOptions, CreateCoreWebView2ControllerCompletedHandler,
    CreateCoreWebView2EnvironmentCompletedHandler, ExecuteScriptCompletedHandler,
    NavigationCompletedEventHandler, WebMessageReceivedEventHandler,
    WebResourceRequestedEventHandler,
};
use windows::Win32::Foundation::{E_POINTER, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, GetMessageW, MSG, PM_NOREMOVE, PeekMessageW,
    PostThreadMessageW, TranslateMessage, WINDOW_EX_STYLE, WM_APP, WS_POPUP,
};
use windows::core::{HSTRING, Interface as _, PWSTR, w};

use super::potoken::PO_TOKEN_HTML;
use super::webview_host::{JsHost, PAGE_TIMEOUT, parse_call_result, wrap_async_call};
use crate::{Error, Result};

/// The engine is Chromium (Edge): look like Chrome on Windows to YouTube too.
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 Edg/141.0.0.0";
const PAGE_URL: &str = "https://www.youtube.com/botguard-po-token";

enum Job {
    Load(Sender<Result<()>>),
    Call(String, Sender<Result<String>>),
    Quit,
}

struct Thread {
    jobs: Sender<Job>,
    thread_id: u32,
}

impl Thread {
    fn send(&self, job: Job) -> bool {
        if self.jobs.send(job).is_err() {
            return false;
        }
        // SAFETY: plain Win32 call; the target thread has a message queue.
        unsafe { PostThreadMessageW(self.thread_id, WM_APP, WPARAM(0), LPARAM(0)).is_ok() }
    }
}

/// Runs BotGuard in WebView2 on its own thread (made on first use).
#[derive(Default)]
pub struct WebView2Host {
    thread: Mutex<Option<Thread>>,
}

fn win_err(context: &str, e: impl std::fmt::Display) -> Error {
    Error::Audio(format!("BotGuard (WebView2) {context}: {e}"))
}

impl WebView2Host {
    fn with_thread<T>(&self, f: impl FnOnce(&Thread) -> Result<T>) -> Result<T> {
        let mut guard = self.thread.lock().unwrap_or_else(PoisonError::into_inner);
        if guard.is_none() {
            *guard = Some(spawn()?);
        }
        f(guard.as_ref().expect("just made"))
    }
}

impl JsHost for WebView2Host {
    fn load(&self) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        self.with_thread(|t| {
            t.send(Job::Load(tx))
                .then_some(())
                .ok_or_else(|| win_err("thread", "gone"))
        })?;
        rx.recv_timeout(PAGE_TIMEOUT * 2)
            .map_err(|_| win_err("load", "no answer"))?
    }

    fn call(&self, body: String, timeout: Duration) -> Result<String> {
        let (tx, rx) = mpsc::channel();
        self.with_thread(|t| {
            t.send(Job::Call(body, tx))
                .then_some(())
                .ok_or_else(|| win_err("thread", "gone"))
        })?;
        rx.recv_timeout(timeout)
            .map_err(|_| win_err("script", "timed out"))?
    }

    fn release(&self) {
        if let Some(thread) = self
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            thread.send(Job::Quit);
        }
    }

    fn user_agent(&self) -> &'static str {
        USER_AGENT
    }
}

fn spawn() -> Result<Thread> {
    let (jobs, rx) = mpsc::channel();
    let (id_tx, id_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("botguard-webview2".into())
        .spawn(move || run(&rx, &id_tx))
        .map_err(Error::Io)?;
    let thread_id = id_rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| win_err("thread", "did not start"))??;
    Ok(Thread { jobs, thread_id })
}

/// The web view and what its callbacks share.
struct Page {
    controller: ICoreWebView2Controller,
    webview: ICoreWebView2,
}

type Pending = Rc<RefCell<HashMap<u64, Sender<Result<String>>>>>;

fn run(jobs: &Receiver<Job>, started: &Sender<Result<u32>>) {
    // SAFETY: COM / Win32 calls on this thread only; every handle stays on it.
    let window = unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        // Make sure the thread has a message queue before anyone posts to it.
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("BotGuard"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            None,
            None,
        )
    };
    let window = match window {
        Ok(hwnd) => hwnd,
        Err(e) => {
            let _ = started.send(Err(win_err("window", e)));
            return;
        }
    };
    // SAFETY: trivial Win32 query.
    let _ = started.send(Ok(unsafe { GetCurrentThreadId() }));
    let pending: Pending = Rc::default();
    let mut environment: Option<ICoreWebView2Environment> = None;
    let mut page: Option<Page> = None;
    let mut next_id = 0u64;
    let mut msg = MSG::default();
    'outer: loop {
        // SAFETY: the standard message loop of this thread.
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 {
            break;
        }
        // SAFETY: as above.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        while let Ok(job) = jobs.try_recv() {
            match job {
                Job::Load(reply) => {
                    if let Some(old) = page.take() {
                        // SAFETY: closing our own controller on its thread.
                        let _ = unsafe { old.controller.Close() };
                    }
                    let result = load(window, &mut environment, &pending).map(|p| page = Some(p));
                    let _ = reply.send(result);
                }
                Job::Call(body, reply) => {
                    let Some(p) = &page else {
                        let _ = reply.send(Err(win_err("script", "no page loaded")));
                        continue;
                    };
                    next_id += 1;
                    pending.borrow_mut().insert(next_id, reply);
                    let script = wrap_async_call(
                        next_id,
                        &body,
                        "m => window.chrome.webview.postMessage(m)",
                    );
                    // SAFETY: our web view, on its thread.
                    let started = unsafe {
                        p.webview.ExecuteScript(
                            &HSTRING::from(script),
                            &ExecuteScriptCompletedHandler::create(Box::new(|_, _| Ok(()))),
                        )
                    };
                    if let Err(e) = started
                        && let Some(reply) = pending.borrow_mut().remove(&next_id)
                    {
                        let _ = reply.send(Err(win_err("script", e)));
                    }
                }
                Job::Quit => break 'outer,
            }
        }
    }
    if let Some(page) = page.take() {
        // SAFETY: as above.
        let _ = unsafe { page.controller.Close() };
    }
    // SAFETY: our window, on its thread.
    let _ = unsafe { DestroyWindow(window) };
}

/// Environment (once), controller, handlers, navigation; waits for the load.
fn load(
    window: HWND,
    environment: &mut Option<ICoreWebView2Environment>,
    pending: &Pending,
) -> Result<Page> {
    if environment.is_none() {
        *environment = Some(create_environment()?);
    }
    let env = environment.clone().expect("just made");
    let (tx, rx) = mpsc::channel();
    // SAFETY: COM calls on this (the window's) thread; the closures run here too.
    unsafe {
        env.CreateCoreWebView2Controller(
            window,
            &CreateCoreWebView2ControllerCompletedHandler::create(Box::new(
                move |error, controller| {
                    let _ =
                        tx.send(error.and_then(|()| controller.ok_or_else(|| E_POINTER.into())));
                    Ok(())
                },
            )),
        )
        .map_err(|e| win_err("controller", e))?;
    }
    let controller = webview2_com::wait_with_pump(rx)
        .map_err(|e| win_err("controller", e))?
        .map_err(|e| win_err("controller", e))?;
    // SAFETY: as above.
    unsafe {
        let _ = controller.SetBounds(RECT {
            left: 0,
            top: 0,
            right: 1,
            bottom: 1,
        });
        let _ = controller.SetIsVisible(true);
        let webview = controller
            .CoreWebView2()
            .map_err(|e| win_err("web view", e))?;
        if let Ok(settings) = webview.Settings() {
            let _ = settings.SetAreDevToolsEnabled(false);
            let _ = settings.SetAreDefaultContextMenusEnabled(false);
            if let Ok(settings) = settings.cast::<ICoreWebView2Settings2>() {
                let _ = settings.SetUserAgent(&HSTRING::from(USER_AGENT));
            }
        }
        let mut token = 0i64;
        // The page itself, at YouTube's origin.
        webview
            .AddWebResourceRequestedFilter(
                &HSTRING::from(format!("{PAGE_URL}*")),
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
            )
            .map_err(|e| win_err("filter", e))?;
        let response_env = env.clone();
        webview
            .add_WebResourceRequested(
                &WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else {
                        return Ok(());
                    };
                    let stream = SHCreateMemStream(Some(PO_TOKEN_HTML.as_bytes()));
                    let response = response_env.CreateWebResourceResponse(
                        stream.as_ref(),
                        200,
                        w!("OK"),
                        w!("Content-Type: text/html; charset=utf-8"),
                    )?;
                    args.SetResponse(&response)
                })),
                &mut token,
            )
            .map_err(|e| win_err("resource handler", e))?;
        // Script results.
        let results = pending.clone();
        webview
            .add_WebMessageReceived(
                &WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else {
                        return Ok(());
                    };
                    let mut text = PWSTR::null();
                    if args.TryGetWebMessageAsString(&mut text).is_ok() {
                        let text = webview2_com::take_pwstr(text);
                        if let Some((id, result)) = parse_call_result(&text)
                            && let Some(reply) = results.borrow_mut().remove(&id)
                        {
                            let _ =
                                reply.send(result.map_err(|e| {
                                    Error::Audio(format!("BotGuard script failed: {e}"))
                                }));
                        }
                    }
                    Ok(())
                })),
                &mut token,
            )
            .map_err(|e| win_err("message handler", e))?;
        let (done_tx, done_rx) = mpsc::channel();
        webview
            .add_NavigationCompleted(
                &NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                    let mut ok = windows::core::BOOL(0);
                    if let Some(args) = args {
                        let _ = args.IsSuccess(&mut ok);
                    }
                    let _ = done_tx.send(ok.as_bool());
                    Ok(())
                })),
                &mut token,
            )
            .map_err(|e| win_err("navigation handler", e))?;
        webview
            .Navigate(&HSTRING::from(PAGE_URL))
            .map_err(|e| win_err("navigate", e))?;
        let loaded = webview2_com::wait_with_pump(done_rx).map_err(|e| win_err("navigate", e))?;
        if !loaded {
            let _ = controller.Close();
            return Err(win_err("navigate", "the page didn't load"));
        }
        Ok(Page {
            controller,
            webview,
        })
    }
}

fn create_environment() -> Result<ICoreWebView2Environment> {
    let data = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("music-macos")
        .join("BotGuard");
    let _ = std::fs::create_dir_all(&data);
    let data = HSTRING::from(data.as_os_str());
    let (tx, rx) = mpsc::channel();
    let options = CoreWebView2EnvironmentOptions::default();
    // SAFETY: COM on this thread; the completion runs through its message loop.
    unsafe {
        CreateCoreWebView2EnvironmentWithOptions(
            None,
            &data,
            &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2EnvironmentOptions::from(
                options,
            ),
            &CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(
                move |error, environment| {
                    let _ =
                        tx.send(error.and_then(|()| environment.ok_or_else(|| E_POINTER.into())));
                    Ok(())
                },
            )),
        )
        .map_err(|e| win_err("environment (is the WebView2 runtime installed?)", e))?;
    }
    webview2_com::wait_with_pump(rx)
        .map_err(|e| win_err("environment", e))?
        .map_err(|e| win_err("environment", e))
}
