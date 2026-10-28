//! Full-length YouTube audio: the [`WebResolver`] stream resolver.
//!
//! * [`WebClient::Tv`] needs only YouTube's JavaScript challenges solved, which
//!   [`jsc`] does with yt-dlp's public-domain solver in an embedded QuickJS.
//! * [`WebClient::MobileWeb`] additionally needs a PoToken from BotGuard
//!   ([`potoken`]); on macOS [`WebKitBotGuard`] runs BotGuard in a hidden WKWebView.
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

pub use potoken::{BotGuardTokenProvider, BotGuardVm, PoTokenProvider};
pub use resolver::{WebClient, WebResolver};
#[cfg(all(target_os = "macos", feature = "webkit-potoken"))]
pub use webkit::WebKitBotGuard;

/// The default PoToken provider for this platform, if there is one: BotGuard in a
/// hidden WKWebView on macOS (the host must run the main run loop), none elsewhere.
pub fn platform_potoken_provider() -> Option<std::sync::Arc<dyn PoTokenProvider>> {
    #[cfg(all(target_os = "macos", feature = "webkit-potoken"))]
    {
        Some(std::sync::Arc::new(BotGuardTokenProvider::new(
            WebKitBotGuard::new(),
        )))
    }
    #[cfg(not(all(target_os = "macos", feature = "webkit-potoken")))]
    {
        None
    }
}
