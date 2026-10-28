//! [`BotGuardVm`] on top of a web engine that runs on its own thread (WebView2 on
//! Windows, WebKitGTK on Linux): the same page and the same JavaScript as the
//! macOS WKWebView (`webkit.rs`), with the engine behind [`JsHost`].
//!
//! Untested on real Windows / Linux machines (written and type-checked from
//! macOS); `platform_potoken_provider` only returns it there.

use std::time::{Duration, Instant};

use super::potoken::BotGuardVm;
use crate::{Error, Result};

pub(super) const PAGE_TIMEOUT: Duration = Duration::from_secs(15);
pub(super) const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// A hidden web view on a worker thread with `assets/po_token.html` loaded at
/// `https://www.youtube.com/`.
pub(super) trait JsHost: Send + Sync {
    /// Loads a fresh page (the previous one, if any, is dropped).
    fn load(&self) -> Result<()>;
    /// Runs `body` as the body of an `async` function and returns the string it
    /// `return`s (like WebKit's `callAsyncJavaScript`).
    fn call(&self, body: String, timeout: Duration) -> Result<String>;
    /// Drops the page.
    fn release(&self);
    fn user_agent(&self) -> &'static str;
}

pub(super) struct HostedBotGuard<H>(pub(super) H);

impl<H: JsHost> HostedBotGuard<H> {
    fn load_page(&self) -> Result<()> {
        self.0.load()?;
        // Wait until the page's script is defined.
        let deadline = Instant::now() + PAGE_TIMEOUT;
        loop {
            let ready = self.0.call(
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
}

impl<H: JsHost> BotGuardVm for HostedBotGuard<H> {
    fn run_challenge(&self, config_json: &str, challenge_json: &str) -> Result<String> {
        self.0.release();
        self.load_page()?;
        // Both are serde_json output, i.e. valid JavaScript literals.
        self.0.call(
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
        self.0
            .call(
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
        let out = self.0.call(
            format!(
                "const u = await obtainPoToken(new Uint8Array({bytes}));\n\
                 return JSON.stringify(Array.from(u));"
            ),
            CALL_TIMEOUT,
        )?;
        serde_json::from_str(&out).map_err(|e| Error::Parse(format!("BotGuard token: {e}")))
    }

    fn release(&self) {
        self.0.release();
    }

    fn user_agent(&self) -> &'static str {
        self.0.user_agent()
    }
}

/// Wraps an `async` function body so a script runner that can't await (WebView2's
/// `ExecuteScript`) posts its result as a message: `{"id":…,"ok":"…"}` or
/// `{"id":…,"error":"…"}` through `post` (a JavaScript function expression).
#[cfg_attr(not(all(windows, feature = "webview2-potoken")), allow(dead_code))]
pub(super) fn wrap_async_call(id: u64, body: &str, post: &str) -> String {
    format!(
        "(async () => {{\n{body}\n}})().then(\n\
         r => ({post})(JSON.stringify({{ id: {id}, ok: String(r) }})),\n\
         e => ({post})(JSON.stringify({{ id: {id}, error: String((e && e.stack) || e) }})));"
    )
}

/// `(id, Ok(value) | Err(message))` from a message posted by [`wrap_async_call`].
#[cfg_attr(not(all(windows, feature = "webview2-potoken")), allow(dead_code))]
pub(super) fn parse_call_result(
    message: &str,
) -> Option<(u64, std::result::Result<String, String>)> {
    let v: serde_json::Value = serde_json::from_str(message).ok()?;
    let id = v.get("id")?.as_u64()?;
    if let Some(ok) = v.get("ok").and_then(|o| o.as_str()) {
        return Some((id, Ok(ok.to_owned())));
    }
    let error = v
        .get("error")
        .and_then(|e| e.as_str())
        .unwrap_or("unknown error");
    Some((id, Err(error.to_owned())))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// A host that records the scripts and answers like the page would.
    #[derive(Default)]
    struct FakeHost {
        calls: Mutex<Vec<String>>,
        loads: Mutex<u32>,
    }

    impl JsHost for FakeHost {
        fn load(&self) -> Result<()> {
            *self.loads.lock().unwrap() += 1;
            Ok(())
        }

        fn call(&self, body: String, _: Duration) -> Result<String> {
            self.calls.lock().unwrap().push(body.clone());
            Ok(if body.contains("typeof runBotGuard") {
                "true".into()
            } else if body.contains("runBotGuard(") {
                "bg-response".into()
            } else if body.contains("obtainPoToken") {
                "[1,2,250]".into()
            } else {
                "ok".into()
            })
        }

        fn release(&self) {}

        fn user_agent(&self) -> &'static str {
            "test-agent"
        }
    }

    #[test]
    fn drives_the_page_like_the_webkit_vm() {
        let vm = HostedBotGuard(FakeHost::default());
        assert_eq!(
            vm.run_challenge(r#"{"a":1}"#, r#"{"program":"p"}"#)
                .unwrap(),
            "bg-response"
        );
        vm.create_minter(&[9, 8]).unwrap();
        assert_eq!(vm.mint(b"vid").unwrap(), vec![1, 2, 250]);
        assert_eq!(vm.user_agent(), "test-agent");
        let calls = vm.0.calls.lock().unwrap();
        assert_eq!(*vm.0.loads.lock().unwrap(), 1);
        assert!(calls[1].contains(r#"globalThis.yt = { config_: {"a":1} };"#));
        assert!(calls[1].contains(r#"await runBotGuard({"program":"p"})"#));
        assert!(calls[2].contains("new Uint8Array([9,8])"));
        assert!(calls[3].contains("obtainPoToken(new Uint8Array([118,105,100]))"));
    }

    #[test]
    fn async_call_messages() {
        let js = wrap_async_call(7, "return 1;", "m => chrome.webview.postMessage(m)");
        assert!(js.starts_with("(async () => {\nreturn 1;\n})().then("));
        assert!(js.contains("JSON.stringify({ id: 7, ok: String(r) })"));
        assert_eq!(
            parse_call_result(r#"{"id":7,"ok":"true"}"#),
            Some((7, Ok("true".into())))
        );
        assert_eq!(
            parse_call_result(r#"{"id":8,"error":"TypeError: x"}"#),
            Some((8, Err("TypeError: x".into())))
        );
        assert_eq!(parse_call_result("nope"), None);
    }
}
