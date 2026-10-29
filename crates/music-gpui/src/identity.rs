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
/// Ids the app had before (newest first). Their settings / cache folders are moved to
/// [`APP_ID`]'s on the first start, and their keychain items are taken over when read.
pub const LEGACY_APP_IDS: &[&str] = &["tunedesk"];

/// The platform folders for [`APP_ID`]. On the first start after a rename, an old
/// id's config and cache folders are moved over (when the new ones don't exist yet).
pub fn project_dirs() -> Option<directories::ProjectDirs> {
    static MIGRATED: std::sync::Once = std::sync::Once::new();
    let dirs = directories::ProjectDirs::from("", "", APP_ID)?;
    MIGRATED.call_once(|| {
        for old in LEGACY_APP_IDS {
            let Some(legacy) = directories::ProjectDirs::from("", "", old) else {
                continue;
            };
            move_if_new_missing(legacy.config_dir(), dirs.config_dir());
            move_if_new_missing(legacy.cache_dir(), dirs.cache_dir());
            move_if_new_missing(legacy.data_local_dir(), dirs.data_local_dir());
        }
    });
    Some(dirs)
}

fn move_if_new_missing(old: &std::path::Path, new: &std::path::Path) {
    if old == new || !old.is_dir() || new.exists() {
        return;
    }
    if let Some(parent) = new.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::rename(old, new) {
        Ok(()) => eprintln!("music-gpui: moved {} to {}", old.display(), new.display()),
        Err(e) => eprintln!("music-gpui: could not move {}: {e}", old.display()),
    }
}

/// D-Bus / MPRIS-safe form of [`APP_ID`] (`music_macos`).
pub fn bus_name() -> String {
    APP_ID.replace(['-', '.'], "_")
}
