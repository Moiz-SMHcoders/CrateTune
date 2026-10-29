//! Windows sign-in window: a plain Win32 window whose client area is a WebView2,
//! on a thread of its own with its own message loop (like the PoToken provider in
//! `music_core::youtube::webview2`).
//!
//! Why not a child of a GPUI window, as on macOS: GPUI draws through a topmost
//! DirectComposition target, which covers child windows; and building a web view
//! with wry waits for WebView2 by pumping the message loop, which would re-enter
//! GPUI from inside an update. On its own thread, everything can wait freely and
//! GPUI only awaits the [`Outcome`].
//!
//! The controller runs InPrivate (nothing of the session is written to disk) with a
//! user-data folder of its own (the app's local data folder, `SignIn`). Once a second
//! the page's address is checked; on music.youtube.com the cookies for it are read
//! through `ICoreWebView2CookieManager::GetCookies` (asynchronously, the answer
//! arrives through this thread's loop) and offered to [`Request::accept`]; the first
//! accepted set is sent back and the window closes.
//!
//! Plain Win32 / WebView2 only (no GPUI types), so it type-checks for a Windows
//! target without GPUI's C dependencies.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::mpsc;

use futures_channel::oneshot;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC, CreateCoreWebView2EnvironmentWithOptions,
    GetAvailableCoreWebView2BrowserVersionString, ICoreWebView2, ICoreWebView2_2,
    ICoreWebView2Controller, ICoreWebView2CookieList, ICoreWebView2Environment,
    ICoreWebView2Environment10, ICoreWebView2EnvironmentOptions,
};
use webview2_com::{
    CoreWebView2EnvironmentOptions, CreateCoreWebView2ControllerCompletedHandler,
    CreateCoreWebView2EnvironmentCompletedHandler, GetCookiesCompletedHandler,
};
use windows::Win32::Foundation::{E_POINTER, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect,
    GetMessageW, GetWindowRect, IDC_ARROW, KillTimer, LoadCursorW, LoadIconW, MSG, PostQuitMessage,
    RegisterClassW, SW_SHOW, SetForegroundWindow, SetTimer, ShowWindow, TranslateMessage,
    WINDOW_EX_STYLE, WM_DESTROY, WM_MOVE, WM_SIZE, WM_TIMER, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};
use windows::core::{HSTRING, Interface as _, PCWSTR, PWSTR, w};

/// (name, value, domain) of each cookie.
pub type Cookies = Vec<(String, String, String)>;

pub struct Request {
    /// Window title.
    pub title: String,
    /// Where the web view starts.
    pub url: &'static str,
    /// Where the flow ends; the cookies read are this URL's.
    pub cookies_url: &'static str,
    /// WebView2 user-data folder.
    pub data_dir: PathBuf,
    /// HWND of the window to center on (the main window).
    pub center_on: Option<isize>,
    /// Whether a set of cookies completes the sign-in.
    pub accept: fn(&[(String, String, String)]) -> bool,
    /// `MUSIC_TRACE`: log the pages visited (paths only, no query strings).
    pub trace: bool,
}

pub enum Failure {
    /// No WebView2 runtime installed.
    RuntimeMissing,
    Error(String),
}

pub enum Outcome {
    Cookies(Cookies),
    /// Closed by the user before signing in.
    Closed,
    Failed(Failure),
}

/// The open sign-in window (one at a time).
static OPEN: Mutex<Option<isize>> = Mutex::new(None);

/// Opens the window on a new thread; `done` gets the outcome. `false` if one is
/// already open (it is brought to the front instead).
pub fn start(request: Request, done: oneshot::Sender<Outcome>) -> bool {
    {
        let open = OPEN.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hwnd) = *open {
            if hwnd != 0 {
                // SAFETY: a stale handle just fails.
                unsafe {
                    let _ = SetForegroundWindow(HWND(hwnd as *mut _));
                }
            }
            return false;
        }
    }
    *OPEN.lock().unwrap_or_else(|e| e.into_inner()) = Some(0);
    let spawned = std::thread::Builder::new()
        .name("sign-in-webview2".into())
        .spawn(move || {
            let outcome = run(&request);
            *OPEN.lock().unwrap_or_else(|e| e.into_inner()) = None;
            let _ = done.send(outcome);
        });
    if spawned.is_err() {
        *OPEN.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    true
}

thread_local! {
    /// The controller, for resizing from the window procedure.
    static CONTROLLER: RefCell<Option<ICoreWebView2Controller>> = const { RefCell::new(None) };
    /// The window was destroyed (closed by the user, or after success).
    static CLOSED: Cell<bool> = const { Cell::new(false) };
}

fn error(context: &str, e: impl std::fmt::Display) -> Failure {
    Failure::Error(format!("{context}: {e}"))
}

/// Whether a WebView2 runtime (Evergreen or fixed) is installed.
fn runtime_installed() -> bool {
    let mut version = PWSTR::null();
    // SAFETY: the loader writes a CoTaskMem string (or nothing) to `version`.
    let ok = unsafe { GetAvailableCoreWebView2BrowserVersionString(PCWSTR::null(), &mut version) };
    let version = webview2_com::take_pwstr(version);
    ok.is_ok() && !version.is_empty()
}

fn run(request: &Request) -> Outcome {
    // SAFETY: COM for this thread only; everything below stays on it.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let outcome = if runtime_installed() {
        match create_window(request) {
            Ok(hwnd) => {
                *OPEN.lock().unwrap_or_else(|e| e.into_inner()) = Some(hwnd.0 as isize);
                let outcome = browse(hwnd, request);
                CONTROLLER.with(|c| {
                    if let Some(controller) = c.borrow_mut().take() {
                        // SAFETY: our controller, on its thread.
                        let _ = unsafe { controller.Close() };
                    }
                });
                if !CLOSED.get() {
                    // SAFETY: our window, on its thread.
                    let _ = unsafe { DestroyWindow(hwnd) };
                }
                outcome
            }
            Err(e) => Outcome::Failed(e),
        }
    } else {
        Outcome::Failed(Failure::RuntimeMissing)
    };
    // SAFETY: pairs the CoInitializeEx above.
    unsafe { CoUninitialize() };
    outcome
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_SIZE => {
            fit_to_window(hwnd);
            LRESULT(0)
        }
        WM_MOVE => {
            CONTROLLER.with(|c| {
                if let Some(controller) = c.borrow().as_ref() {
                    // SAFETY: our controller, on its thread.
                    let _ = unsafe { controller.NotifyParentWindowPositionChanged() };
                }
            });
            LRESULT(0)
        }
        WM_DESTROY => {
            CLOSED.set(true);
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Sizes the web view to the window's client area (physical pixels, as both use).
fn fit_to_window(hwnd: HWND) {
    CONTROLLER.with(|c| {
        if let Some(controller) = c.borrow().as_ref() {
            let mut rect = RECT::default();
            // SAFETY: our window and controller, on their thread.
            unsafe {
                if GetClientRect(hwnd, &mut rect).is_ok() {
                    let _ = controller.SetBounds(rect);
                }
            }
        }
    });
}

fn create_window(request: &Request) -> Result<HWND, Failure> {
    const CLASS: PCWSTR = w!("CrateTune.SignIn");
    // SAFETY: registers a class whose procedure only touches this thread's
    // thread-locals, and creates one window of it on this thread.
    unsafe {
        let module = GetModuleHandleW(None).map_err(|e| error("module", e))?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: module.into(),
            lpszClassName: CLASS,
            // Icon 1 of the .exe (packaging/windows/app.rc.in).
            hIcon: LoadIconW(Some(module.into()), PCWSTR(std::ptr::without_provenance(1)))
                .unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        // Fails harmlessly when the class exists from an earlier sign-in.
        RegisterClassW(&wc);
        // 520 × 740 at 100 %, centred on the main window.
        let (mut x, mut y, mut width, mut height) = (CW_USEDEFAULT, CW_USEDEFAULT, 520, 740);
        if let Some(main) = request.center_on.map(|h| HWND(h as *mut _)) {
            let dpi = GetDpiForWindow(main).max(96) as i32;
            width = width * dpi / 96;
            height = height * dpi / 96;
            let mut rect = RECT::default();
            if GetWindowRect(main, &mut rect).is_ok() {
                x = rect.left + (rect.right - rect.left - width) / 2;
                y = (rect.top + (rect.bottom - rect.top - height) / 2).max(rect.top);
            }
        }
        let title = HSTRING::from(request.title.as_str());
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            &title,
            WS_OVERLAPPEDWINDOW,
            x,
            y,
            width,
            height,
            None,
            None,
            Some(module.into()),
            None,
        )
        .map_err(|e| error("window", e))?;
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        Ok(hwnd)
    }
}

fn create_environment(request: &Request) -> Result<ICoreWebView2Environment, Failure> {
    let _ = std::fs::create_dir_all(&request.data_dir);
    let data = HSTRING::from(request.data_dir.as_os_str());
    let (tx, rx) = mpsc::channel();
    let options = CoreWebView2EnvironmentOptions::default();
    // SAFETY: COM on this thread; the completion runs through its message loop.
    unsafe {
        CreateCoreWebView2EnvironmentWithOptions(
            PCWSTR::null(),
            &data,
            &ICoreWebView2EnvironmentOptions::from(options),
            &CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(
                move |result, environment| {
                    let _ =
                        tx.send(result.and_then(|()| environment.ok_or_else(|| E_POINTER.into())));
                    Ok(())
                },
            )),
        )
        .map_err(|e| error("environment", e))?;
    }
    webview2_com::wait_with_pump(rx)
        .map_err(|e| error("environment", e))?
        .map_err(|e| error("environment", e))
}

fn create_controller(
    hwnd: HWND,
    env: &ICoreWebView2Environment,
) -> Result<ICoreWebView2Controller, Failure> {
    let (tx, rx) = mpsc::channel();
    let handler = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(
        move |result, controller| {
            let _ = tx.send(result.and_then(|()| controller.ok_or_else(|| E_POINTER.into())));
            Ok(())
        },
    ));
    // SAFETY: COM on this thread; the completion runs through its message loop.
    unsafe {
        // InPrivate where the runtime supports it (1.0.1185+): no cookies on disk.
        let private = env.cast::<ICoreWebView2Environment10>().and_then(|env10| {
            let options = env10.CreateCoreWebView2ControllerOptions()?;
            options.SetIsInPrivateModeEnabled(true)?;
            env10.CreateCoreWebView2ControllerWithOptions(hwnd, &options, &handler)
        });
        if private.is_err() {
            env.CreateCoreWebView2Controller(hwnd, &handler)
                .map_err(|e| error("controller", e))?;
        }
    }
    webview2_com::wait_with_pump(rx)
        .map_err(|e| error("controller", e))?
        .map_err(|e| error("controller", e))
}

/// The page's current address.
fn source(webview: &ICoreWebView2) -> Option<String> {
    let mut uri = PWSTR::null();
    // SAFETY: WebView2 writes a CoTaskMem string to `uri`.
    unsafe { webview.Source(&mut uri) }.ok()?;
    Some(webview2_com::take_pwstr(uri))
}

/// Asks for the cookies of `url`; the answer arrives on `tx` through the loop.
fn request_cookies(webview: &ICoreWebView2, url: &str, tx: mpsc::Sender<Cookies>) -> bool {
    let handler = GetCookiesCompletedHandler::create(Box::new(move |result, list| {
        let cookies = match (result, list) {
            (Ok(()), Some(list)) => read_cookies(&list),
            _ => Vec::new(),
        };
        let _ = tx.send(cookies);
        Ok(())
    }));
    let url = HSTRING::from(url);
    // SAFETY: COM on this thread.
    unsafe {
        webview
            .cast::<ICoreWebView2_2>()
            .and_then(|w2| w2.CookieManager())
            .and_then(|manager| manager.GetCookies(&url, &handler))
            .is_ok()
    }
}

fn read_cookies(list: &ICoreWebView2CookieList) -> Cookies {
    let mut out = Vec::new();
    let mut count = 0u32;
    // SAFETY: reads from the list WebView2 handed to the completion handler.
    unsafe {
        if list.Count(&mut count).is_err() {
            return out;
        }
        for i in 0..count {
            let Ok(cookie) = list.GetValueAtIndex(i) else {
                continue;
            };
            let (mut name, mut value, mut domain) = (PWSTR::null(), PWSTR::null(), PWSTR::null());
            if cookie.Name(&mut name).is_ok()
                && cookie.Value(&mut value).is_ok()
                && cookie.Domain(&mut domain).is_ok()
            {
                out.push((
                    webview2_com::take_pwstr(name),
                    webview2_com::take_pwstr(value),
                    webview2_com::take_pwstr(domain),
                ));
            }
        }
    }
    out
}

/// Environment, controller, navigation, then the loop until success or close.
fn browse(hwnd: HWND, request: &Request) -> Outcome {
    let env = match create_environment(request) {
        Ok(env) => env,
        Err(_) if CLOSED.get() => return Outcome::Closed,
        Err(e) => return Outcome::Failed(e),
    };
    let controller = match create_controller(hwnd, &env) {
        Ok(controller) => controller,
        Err(_) if CLOSED.get() => return Outcome::Closed,
        Err(e) => return Outcome::Failed(e),
    };
    if CLOSED.get() {
        // SAFETY: our controller, on its thread.
        let _ = unsafe { controller.Close() };
        return Outcome::Closed;
    }
    // SAFETY: COM on this thread.
    let webview = match unsafe { controller.CoreWebView2() } {
        Ok(webview) => webview,
        Err(e) => {
            // SAFETY: as above.
            let _ = unsafe { controller.Close() };
            return Outcome::Failed(error("web view", e));
        }
    };
    CONTROLLER.with(|c| *c.borrow_mut() = Some(controller.clone()));
    fit_to_window(hwnd);
    // SAFETY: COM / Win32 on this thread.
    unsafe {
        let _ = controller.SetIsVisible(true);
        if let Ok(settings) = webview.Settings() {
            let _ = settings.SetIsStatusBarEnabled(false);
        }
        if let Err(e) = webview.Navigate(&HSTRING::from(request.url)) {
            return Outcome::Failed(error("navigate", e));
        }
        let _ = controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
        SetTimer(Some(hwnd), 1, 1000, None);
    }
    let (cookies_tx, cookies_rx) = mpsc::channel::<Cookies>();
    let mut asking = false;
    let mut traced: Option<String> = None;
    let mut msg = MSG::default();
    loop {
        // SAFETY: the standard message loop of this thread.
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 {
            break;
        }
        if msg.message == WM_TIMER && msg.hwnd == hwnd {
            let page = source(&webview);
            if request.trace {
                let path = page
                    .as_deref()
                    .map(|p| p.split('?').next().unwrap_or_default().to_owned());
                if path != traced {
                    eprintln!(
                        "music-gpui: sign-in window at {}",
                        path.as_deref().unwrap_or_default()
                    );
                    traced = path;
                }
            }
            if !asking && page.is_some_and(|p| p.starts_with(request.cookies_url)) {
                asking = request_cookies(&webview, request.cookies_url, cookies_tx.clone());
            }
        } else {
            // SAFETY: as above.
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if let Ok(cookies) = cookies_rx.try_recv() {
            asking = false;
            if (request.accept)(&cookies) {
                // SAFETY: our timer and window, on this thread.
                unsafe {
                    let _ = KillTimer(Some(hwnd), 1);
                }
                return Outcome::Cookies(cookies);
            }
        }
    }
    Outcome::Closed
}
