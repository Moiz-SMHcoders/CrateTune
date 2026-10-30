//! The sign-in web view's persistent profile on disk (Windows: the WebView2
//! user-data folder) and clearing it on sign-out.
//!
//! The folder holds Google's cookies for the login, so the app can renew the
//! YouTube Music session in the background. Sign out deletes it. WebView2's browser
//! processes may hold files for a moment after a web view closes; when the folder
//! can't be deleted, a `<folder>.clear` marker is left next to it and the folder is
//! deleted before the next web view uses it (and at the next start).
//!
//! Plain `std` only, so it also builds for the Windows-only module without GPUI.
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use std::path::{Path, PathBuf};

/// The marker for a clear that couldn't finish.
fn marker(dir: &Path) -> PathBuf {
    let mut name = dir.file_name().unwrap_or_default().to_os_string();
    name.push(".clear");
    dir.with_file_name(name)
}

/// Deletes `dir` (the whole profile). When that fails, leaves the marker so the
/// next [`finish_pending_clear`] does it. Returns whether the folder is gone.
pub(crate) fn clear_dir(dir: &Path) -> bool {
    let gone = match std::fs::remove_dir_all(dir) {
        Ok(()) => true,
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    };
    let marker = marker(dir);
    if gone {
        let _ = std::fs::remove_file(&marker);
    } else {
        if let Some(parent) = marker.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(
            &marker,
            b"sign-out: delete the sign-in profile before use\n",
        );
    }
    gone
}

/// Finishes a clear that failed earlier (call before the profile is used).
/// Returns whether the profile may be used (nothing pending, or cleared now).
pub(crate) fn finish_pending_clear(dir: &Path) -> bool {
    if !marker(dir).exists() {
        return true;
    }
    clear_dir(dir)
}

/// [`clear_dir`], retried for a few seconds (browser processes exiting).
pub(crate) fn clear_dir_patiently(dir: &Path) -> bool {
    for attempt in 0..10 {
        if clear_dir(dir) {
            return true;
        }
        if attempt < 9 {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cratetune-profile-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn clearing_deletes_the_profile() {
        let root = temp("clear");
        let profile = root.join("SignIn");
        std::fs::create_dir_all(profile.join("EBWebView/Default/Network")).unwrap();
        std::fs::write(profile.join("EBWebView/Default/Network/Cookies"), b"x").unwrap();
        assert!(clear_dir(&profile));
        assert!(!profile.exists());
        assert!(!marker(&profile).exists());
        // Clearing what isn't there is fine.
        assert!(clear_dir(&profile));
        assert!(finish_pending_clear(&profile));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_failed_clear_is_finished_before_next_use() {
        let root = temp("pending");
        let profile = root.join("SignIn");
        // A file where the folder should be can't be removed as a folder: the clear
        // fails and leaves the marker.
        std::fs::write(&profile, b"not a folder").unwrap();
        assert!(!clear_dir(&profile));
        assert!(marker(&profile).exists());
        // Later (here: the obstacle is gone) the pending clear completes.
        std::fs::remove_file(&profile).unwrap();
        std::fs::create_dir_all(profile.join("Default")).unwrap();
        assert!(finish_pending_clear(&profile));
        assert!(!profile.exists(), "deleted before use");
        assert!(!marker(&profile).exists());
        // Nothing pending: the profile is left alone.
        std::fs::create_dir_all(profile.join("Default")).unwrap();
        assert!(finish_pending_clear(&profile));
        assert!(profile.exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
