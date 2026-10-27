//! The app's name and ids, from `packaging/app.env` (via `build.rs`) — rename the app
//! there, not here. Bundling scripts and CI read the same file.

/// Display name (window title, menus, About, bundle name).
pub const APP_NAME: &str = env!("MUSIC_APP_NAME");
/// Lower-case id: settings directory, keychain service, Linux app id.
pub const APP_ID: &str = env!("MUSIC_APP_ID");
/// macOS bundle identifier.
#[allow(dead_code)]
pub const BUNDLE_ID: &str = env!("MUSIC_BUNDLE_ID");
/// One-line description.
pub const APP_SUMMARY: &str = env!("MUSIC_APP_SUMMARY");
/// `owner/repo` on GitHub for the update check; empty = no update check.
pub const UPDATE_REPO: &str = env!("MUSIC_UPDATE_REPO");
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// D-Bus / MPRIS-safe form of [`APP_ID`] (`music_macos`).
pub fn bus_name() -> String {
    APP_ID.replace(['-', '.'], "_")
}
