//! Full-length YouTube audio: the [`WebResolver`] stream resolver.
//!
//! * [`WebClient::Tv`] needs only YouTube's JavaScript challenges solved, which
//!   [`jsc`] does with yt-dlp's public-domain solver in an embedded QuickJS.
//! * [`WebClient::MobileWeb`] additionally needs a PoToken from BotGuard
//!   ([`potoken`]): a hidden WKWebView on macOS ([`WebKitBotGuard`]), WebView2
//!   on Windows (feature `webview2-potoken`, default), WebKitGTK on Linux
//!   (feature `webkitgtk-potoken`, opt-in). The Windows and Linux engines are
//!   untested (type-checked from macOS only).
//! * Either way, googlevideo only accepts the URL once the pre-roll ad could have
//!   been skipped (a few seconds); [`crate::ResolvedStream::not_before`] carries that.
//!
//! The iOS-client [`crate::IosClientResolver`] (first ~1 MiB only) stays useful
//! as the last fallback.

mod jsc;
pub mod potoken;
mod resolver;
mod session;
#[cfg(all(target_os = "macos", feature = "webkit-potoken"))]
mod webkit;
#[cfg(all(target_os = "linux", feature = "webkitgtk-potoken"))]
mod webkitgtk;
#[cfg(all(windows, feature = "webview2-potoken"))]
mod webview2;
#[cfg_attr(
    not(any(
        all(windows, feature = "webview2-potoken"),
        all(target_os = "linux", feature = "webkitgtk-potoken")
    )),
    allow(dead_code)
)]
mod webview_host;

pub use potoken::{BotGuardTokenProvider, BotGuardVm, PoTokenProvider};
pub use resolver::{WebClient, WebResolver};
#[cfg(all(target_os = "macos", feature = "webkit-potoken"))]
pub use webkit::WebKitBotGuard;

/// The default PoToken provider for this platform, if there is one: BotGuard in a
/// hidden WKWebView on macOS (the host must run the main run loop), in WebView2
/// on Windows, in WebKitGTK on Linux (with the `webkitgtk-potoken` feature);
/// none otherwise (the TV client needs none).
pub fn platform_potoken_provider() -> Option<std::sync::Arc<dyn PoTokenProvider>> {
    #[cfg(all(target_os = "macos", feature = "webkit-potoken"))]
    {
        Some(std::sync::Arc::new(BotGuardTokenProvider::new(
            WebKitBotGuard::new(),
        )))
    }
    #[cfg(all(windows, feature = "webview2-potoken"))]
    {
        Some(std::sync::Arc::new(BotGuardTokenProvider::new(
            webview_host::HostedBotGuard(webview2::WebView2Host::default()),
        )))
    }
    #[cfg(all(target_os = "linux", feature = "webkitgtk-potoken"))]
    {
        Some(std::sync::Arc::new(BotGuardTokenProvider::new(
            webview_host::HostedBotGuard(webkitgtk::WebKitGtkHost::default()),
        )))
    }
    #[cfg(not(any(
        all(target_os = "macos", feature = "webkit-potoken"),
        all(windows, feature = "webview2-potoken"),
        all(target_os = "linux", feature = "webkitgtk-potoken")
    )))]
    {
        None
    }
}
