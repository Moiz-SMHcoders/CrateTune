//! Library, privacy, storage, local-file and account preferences (the `library`
//! object of `settings.json`). The library data itself lives in SQLite
//! (`music_core::library`); these are only the switches around it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Play-cache sizes offered in the Storage section (MiB).
pub const CACHE_LIMITS_MB: [u64; 7] = [128, 256, 512, 1024, 2048, 4096, 8192];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryConfig {
    /// Don't record plays into the history (stats stop growing too).
    pub pause_listen_history: bool,
    /// Don't remember search queries.
    pub pause_search_history: bool,
    /// Size limit of the automatic play cache, in MiB.
    pub cache_limit_mb: u64,
    /// Keep a copy of every played song in the play cache (fetches it a second time).
    pub cache_played: bool,
    /// Music folders scanned into the library.
    pub local_folders: Vec<PathBuf>,
    /// Folders (and everything below) skipped while scanning.
    pub excluded_folders: Vec<PathBuf>,
    /// Shorter local files are ignored (ringtones, samples).
    pub local_min_secs: u32,
    /// The signed-in YouTube Music account (the cookie itself is a secret).
    pub account: Option<AccountConfig>,
    /// Downloading liked songs (Storage → Downloads).
    pub downloads: DownloadPrefs,
    /// Sync with YouTube Music in the background (at start, every while, on focus,
    /// and each played song's like from YouTube). ArchiveTune's `YtmSyncKey`, on by
    /// default. Off: only "Sync now", sign-in and account switches sync.
    pub auto_sync: bool,
    /// Disliking the playing song skips to the next one (as in YouTube Music).
    pub skip_disliked: bool,
}

/// Automatic downloads of liked songs (ArchiveTune's "auto download on like").
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DownloadPrefs {
    /// Liking a YouTube song queues a download (local files are never downloaded).
    pub auto_download_liked: bool,
    /// Unliking a song deletes its download (off: downloads stay).
    pub remove_when_unliked: bool,
}

impl Default for LibraryConfig {
    fn default() -> Self {
        Self {
            pause_listen_history: false,
            pause_search_history: false,
            cache_limit_mb: 512,
            cache_played: false,
            local_folders: Vec::new(),
            excluded_folders: Vec::new(),
            local_min_secs: 30,
            account: None,
            downloads: DownloadPrefs::default(),
            auto_sync: true,
            skip_disliked: true,
        }
    }
}

impl LibraryConfig {
    pub fn normalize(&mut self) {
        self.cache_limit_mb = self.cache_limit_mb.clamp(32, 1 << 20);
        self.local_min_secs = self.local_min_secs.min(600);
        self.local_folders.dedup();
        self.excluded_folders.dedup();
    }

    pub fn cache_limit_bytes(&self) -> u64 {
        self.cache_limit_mb * 1024 * 1024
    }
}

/// What is shown for the signed-in account and which channel / Google account
/// requests act as.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountConfig {
    pub name: String,
    pub email: Option<String>,
    pub handle: Option<String>,
    pub thumbnail_url: Option<String>,
    /// `X-Goog-AuthUser`: which Google account of a multi-login cookie.
    pub auth_user: u32,
    /// Selected channel (brand account) as `DATASYNC_ID`; `None` = the default one.
    pub data_sync_id: Option<String>,
    /// Last successful library sync (Unix ms).
    pub last_sync_at: Option<i64>,
    /// Last successful sync of the liked songs, alone or as part of a full sync.
    pub last_liked_sync_at: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_clamping() {
        let mut c: LibraryConfig = serde_json::from_str(r#"{"cache_limit_mb": 1}"#).unwrap();
        c.normalize();
        assert_eq!(c.cache_limit_mb, 32);
        assert_eq!(c.local_min_secs, 30);
        assert!(c.account.is_none());
        assert_eq!(c.downloads, DownloadPrefs::default());
        assert!(!c.downloads.auto_download_liked && !c.downloads.remove_when_unliked);
        let c: LibraryConfig =
            serde_json::from_str(r#"{"downloads": {"auto_download_liked": true}}"#).unwrap();
        assert!(c.downloads.auto_download_liked && !c.downloads.remove_when_unliked);
        let c: LibraryConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(c, LibraryConfig::default());
        // Older files: syncing automatically and skipping dislikes start on.
        assert!(c.auto_sync && c.skip_disliked);
        let c: LibraryConfig = serde_json::from_str(r#"{"auto_sync": false}"#).unwrap();
        assert!(!c.auto_sync && c.skip_disliked);
        assert_eq!(c.cache_limit_bytes(), 512 * 1024 * 1024);
    }
}
