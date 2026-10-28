//! [`BotGuardVm`] on a hidden WKWebView (macOS).
//!
//! WebKit objects belong to the main thread: every operation is dispatched to the
//! main queue and the calling (worker) thread blocks on the answer. The host
//! process must therefore be running the main run loop (any AppKit app does —
//! GPUI's included); calling from the main thread itself is refused instead of
//! deadlocking. The view is never put in a window.

use std::cell::RefCell;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSError, NSString, NSURL};
use objc2_web_kit::{WKContentWorld, WKWebView, WKWebViewConfiguration};

use super::potoken::{ATTESTATION_UA, BotGuardVm, PO_TOKEN_HTML};
use crate::{Error, Result};

const PAGE_TIMEOUT: Duration = Duration::from_secs(15);
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

thread_local! {
    /// The BotGuard page; lives on (and is only touched from) the main thread.
    static VIEW: RefCell<Option<Retained<WKWebView>>> = const { RefCell::new(None) };
}

/// Runs BotGuard in a WKWebView. Cheap to create; the view is made on first use
/// and released by [`BotGuardVm::release`].
#[derive(Debug, Default)]
pub struct WebKitBotGuard;

impl WebKitBotGuard {
    pub fn new() -> Self {
        Self
    }
}

/// Runs `f` on the main thread and waits for what it sends back.
fn on_main<T: Send + 'static>(
    timeout: Duration,
    f: impl FnOnce(MainThreadMarker, mpsc::Sender<Result<T>>) + Send + 'static,
) -> Result<T> {
    if MainThreadMarker::new().is_some() {
        return Err(Error::Audio(
            "BotGuard must not be driven from the main thread".into(),
        ));
    }
    let (tx, rx) = mpsc::channel();
    DispatchQueue::main().exec_async(move || {
        let mtm = MainThreadMarker::new().expect("main queue runs on the main thread");
        f(mtm, tx);
    });
    rx.recv_timeout(timeout).map_err(|_| {
        Error::Audio("BotGuard: the web view did not answer (is the main run loop running?)".into())
    })?
}

/// `callAsyncJavaScript` on the BotGuard page; the body must `return` a string.
fn call_js(body: String, timeout: Duration) -> Result<String> {
    on_main(timeout, move |mtm, tx| {
        let Some(view) = VIEW.with(|v| v.borrow().clone()) else {
            let _ = tx.send(Err(Error::Audio("BotGuard: no page loaded".into())));
            return;
        };
        let tx = std::cell::RefCell::new(Some(tx));
        let handler = RcBlock::new(move |value: *mut AnyObject, error: *mut NSError| {
            let Some(tx) = tx.borrow_mut().take() else {
                return;
            };
            // SAFETY: WebKit passes valid (or null) pointers for the duration of the call.
            let result = unsafe {
                if let Some(error) = error.as_ref() {
                    Err(Error::Audio(format!(
                        "BotGuard script failed: {}",
                        error.localizedDescription()
                    )))
                } else if let Some(text) = value.as_ref().and_then(|v| v.downcast_ref::<NSString>())
                {
                    Ok(text.to_string())
                } else {
                    Err(Error::Audio("BotGuard script returned no string".into()))
                }
            };
            let _ = tx.send(result);
        });
        // SAFETY: arguments is None; the page world and handler are valid.
        unsafe {
            view.callAsyncJavaScript_arguments_inFrame_inContentWorld_completionHandler(
                &NSString::from_str(&body),
                None,
                None,
                &WKContentWorld::pageWorld(mtm),
                Some(&handler),
            );
        }
    })
}

fn load_page() -> Result<()> {
    on_main(PAGE_TIMEOUT, |mtm, tx| {
        // SAFETY: plain WebKit object creation on the main thread.
        unsafe {
            let config = WKWebViewConfiguration::new(mtm);
            let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1.0, 1.0));
            let view =
                WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &config);
            view.setCustomUserAgent(Some(&NSString::from_str(ATTESTATION_UA)));
            let base = NSURL::URLWithString(&NSString::from_str("https://www.youtube.com"));
            view.loadHTMLString_baseURL(&NSString::from_str(PO_TOKEN_HTML), base.as_deref());
            VIEW.with(|v| *v.borrow_mut() = Some(view));
        }
        let _ = tx.send(Ok(()));
    })?;
    // Wait until the page's script is defined.
    let deadline = Instant::now() + PAGE_TIMEOUT;
    loop {
        let ready = call_js(
            "return String(typeof runBotGuard === 'function' && document.readyState === 'complete');"
                .into(),
            Duration::from_secs(5),
        );
        if matches!(ready.as_deref(), Ok("true")) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::Audio("BotGuard page did not load".into()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

impl BotGuardVm for WebKitBotGuard {
    fn run_challenge(&self, config_json: &str, challenge_json: &str) -> Result<String> {
        self.release();
        load_page()?;
        // Both are serde_json output, i.e. valid JavaScript literals.
        call_js(
            format!(
                "globalThis.yt = {{ config_: {config_json} }};\n\
                 const r = await runBotGuard({challenge_json});\n\
                 globalThis.webPoSignalOutput = r.webPoSignalOutput;\n\
                 return String(r.botguardResponse);"
            ),
            CALL_TIMEOUT,
        )
    }

    fn create_minter(&self, integrity_token: &[u8]) -> Result<()> {
        let bytes = serde_json::to_string(integrity_token).unwrap_or_default();
        call_js(
            format!(
                "await createPoTokenMinter(globalThis.webPoSignalOutput, new Uint8Array({bytes}));\n\
                 return 'ok';"
            ),
            CALL_TIMEOUT,
        )
        .map(drop)
    }

    fn mint(&self, identifier: &[u8]) -> Result<Vec<u8>> {
        let bytes = serde_json::to_string(identifier).unwrap_or_default();
        let out = call_js(
            format!(
                "const u = await obtainPoToken(new Uint8Array({bytes}));\n\
                 return JSON.stringify(Array.from(u));"
            ),
            CALL_TIMEOUT,
        )?;
        serde_json::from_str(&out).map_err(|e| Error::Parse(format!("BotGuard token: {e}")))
    }

    fn release(&self) {
        if MainThreadMarker::new().is_some() {
            VIEW.with(|v| v.borrow_mut().take());
            return;
        }
        DispatchQueue::main().exec_async(|| {
            VIEW.with(|v| v.borrow_mut().take());
        });
    }
}
