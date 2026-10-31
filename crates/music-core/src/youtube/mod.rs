//! Full-length YouTube audio: the [`WebResolver`] stream resolver.
//!
//! It tries [`WebClient`]s fastest first, each behind a circuit breaker
//! ([`breaker`]: three failures in a row skip a client for ten minutes):
//!
//! * [`WebClient::VisionOs`] gets direct URLs from one request: no JavaScript,
//!   no PoToken, no ad wait.
//! * [`WebClient::Tv`] needs YouTube's JavaScript challenges solved, which
//!   [`jsc`] does with yt-dlp's public-domain solver in an embedded QuickJS.
//!   A failed solve drops the cached player, fetches it again and retries once.
//! * [`WebClient::TvSimply`] and [`WebClient::MobileWeb`] additionally need a
//!   PoToken from BotGuard
//!   ([`potoken`]): a hidden WKWebView on macOS ([`WebKitBotGuard`]), WebView2
//!   on Windows (feature `webview2-potoken`, default), WebKitGTK on Linux
//!   (feature `webkitgtk-potoken`, opt-in). The Windows and Linux engines are
//!   untested (type-checked from macOS only).
//! * Either way, googlevideo only accepts the URL once the pre-roll ad could have
//!   been skipped (a few seconds); [`crate::ResolvedStream::not_before`] carries that.
//!
//! The iOS-client [`crate::IosClientResolver`] (first ~1 MiB only) stays useful
//! as the last fallback. The signed player-fixes manifest ([`fixes`]) updates
//! client identities and player hints without an app update, and the next
//! track's URL is resolved ahead ([`prefetch`]: kept 1 h at most).

pub mod breaker;
mod client;
pub mod fixes;
mod jsc;
pub mod potoken;
mod prefetch;
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

pub use breaker::{ClientHealth, ResolverStatus};
pub use client::{ClientOverrides, WebClient};
pub use fixes::{FixesStore, PlayerFixes};
pub use potoken::{BotGuardTokenProvider, BotGuardVm, PoTokenProvider};
pub use resolver::{StatusHook, WebResolver};
#[cfg(all(target_os = "macos", feature = "webkit-potoken"))]
pub use webkit::WebKitBotGuard;

/// The default PoToken provider for this platform, if there is one: BotGuard in a
/// hidden WKWebView on macOS (the host must run the main run loop), in WebView2
/// on Windows, in WebKitGTK on Linux (with the `webkitgtk-potoken` feature);
/// none otherwise (the visionOS and TV clients need none).
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
