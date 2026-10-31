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
    /// The signed-in YouTube Music account in use (the cookie itself is a secret).
    pub account: Option<AccountConfig>,
    /// The other signed-in accounts, for switching (`account.rs`, "Several accounts");
    /// each one's cookie is a secret of its own (`secrets::account_cookie_key`).
    pub saved_accounts: Vec<AccountConfig>,
    /// "Add plays to my YouTube history": report plays to the account's YouTube Music
    /// history while signed in (`play_report.rs`). On by default.
    pub report_plays: bool,
    /// Home's "Continue from another device" card: the history entries dismissed
    /// (video ids, newest last, a few kept).
    pub continue_dismissed: Vec<String>,
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
            saved_accounts: Vec::new(),
            report_plays: true,
            continue_dismissed: Vec::new(),
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
        // Every account has an id (older files: the one account had none), no id twice,
        // and the active one isn't also among the saved ones.
        let mut seen = std::collections::HashSet::new();
        if let Some(a) = &mut self.account {
            a.ensure_id();
            seen.insert(a.id.clone());
        }
        for a in &mut self.saved_accounts {
            a.ensure_id();
        }
        self.saved_accounts.retain(|a| seen.insert(a.id.clone()));
        let keep = self.continue_dismissed.len().saturating_sub(MAX_DISMISSED);
        self.continue_dismissed.drain(..keep);
    }

    pub fn cache_limit_bytes(&self) -> u64 {
        self.cache_limit_mb * 1024 * 1024
    }
}

/// How many dismissed "Continue from another device" entries are remembered.
pub const MAX_DISMISSED: usize = 20;

/// What is shown for a signed-in account and which channel / Google account
/// requests act as. The active one is `LibraryConfig::account`, the others are in
/// `LibraryConfig::saved_accounts`.
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
    /// Local id (stable; names the account's keychain entry and nothing else).
    pub id: String,
    /// `music_core::innertube::login_key` of the cookie: which browser login it is.
    pub login_key: Option<String>,
    /// The sign-in web view profile holding this account's Google session (renewals
    /// in the background, `login_webview`); `None` = signed in another way.
    pub profile: Option<String>,
    /// When it was added (Unix ms; the list's order).
    pub added_at: i64,
    /// Likes / dislikes still owed to YouTube from while this account was in use;
    /// queued again when it is switched back to (`account_list.rs`).
    pub queued_ratings: Vec<QueuedRating>,
}

/// A rating waiting for an account that isn't in use.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct QueuedRating {
    pub video_id: String,
    /// `music_core::innertube::Rating::as_str`.
    pub rating: String,
}

impl AccountConfig {
    /// Gives the account a fresh id if it has none.
    pub fn ensure_id(&mut self) {
        if self.id.is_empty() {
            self.id = new_account_id();
        }
    }

    /// The sign-in web view profile of this account. An account saved by a version
    /// before "Several accounts" (no `login_key` yet) signed in with the one profile
    /// of that version.
    pub fn sign_in_profile(&self) -> Option<String> {
        self.profile.clone().or_else(|| {
            self.login_key
                .is_none()
                .then(|| crate::login_webview::LEGACY_PROFILE.to_owned())
        })
    }

    /// Whether `other` is the same YouTube identity: the same login and Google account,
    /// or (another login of the same person) the same e-mail and channel.
    pub fn same_identity(&self, other: &AccountConfig) -> bool {
        let same_login = self.login_key.is_some()
            && self.login_key == other.login_key
            && self.auth_user == other.auth_user;
        let same_mail = match (&self.email, &other.email) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            _ => false,
        };
        same_login || (same_mail && self.data_sync_id == other.data_sync_id)
    }
}

/// A random local account id (16 hex digits).
pub fn new_account_id() -> String {
    use std::hash::{BuildHasher as _, Hasher as _};
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default(),
    );
    format!("{:016x}", h.finish())
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
        // Older files: plays are reported, nothing saved besides the account.
        assert!(c.report_plays && c.saved_accounts.is_empty());
    }

    /// The account list survives a save and load; ids are filled in and unique.
    #[test]
    fn account_list_persists() {
        let mut c: LibraryConfig =
            serde_json::from_str(r#"{"account": {"name": "Old one", "auth_user": 1}}"#).unwrap();
        c.normalize();
        let active = c.account.clone().unwrap();
        assert_eq!(active.id.len(), 16, "an older account gets an id");
        assert_eq!(active.auth_user, 1);
        c.saved_accounts = vec![
            AccountConfig {
                name: "Work".into(),
                email: Some("work@example.org".into()),
                login_key: Some("lk-1".into()),
                profile: Some("p".repeat(32)),
                added_at: 5,
                queued_ratings: vec![QueuedRating {
                    video_id: "v".into(),
                    rating: "like".into(),
                }],
                ..Default::default()
            },
            // The active one listed again (a crash mid-switch): dropped.
            active.clone(),
        ];
        c.continue_dismissed = (0..30).map(|i| i.to_string()).collect();
        c.normalize();
        assert_eq!(c.saved_accounts.len(), 1);
        assert_eq!(c.continue_dismissed.len(), MAX_DISMISSED);
        assert_eq!(c.continue_dismissed[0], "10", "the oldest go");
        let json = serde_json::to_string(&c).unwrap();
        let mut back: LibraryConfig = serde_json::from_str(&json).unwrap();
        back.normalize();
        assert_eq!(back, c);
        let work = &back.saved_accounts[0];
        assert_eq!(work.id.len(), 16);
        assert_ne!(work.id, active.id);
        assert_eq!(work.queued_ratings[0].rating, "like");
    }

    #[test]
    fn identities_match_by_login_or_mail() {
        let a = AccountConfig {
            email: Some("Me@Example.org".into()),
            login_key: Some("lk-a".into()),
            ..Default::default()
        };
        let other_login = AccountConfig {
            email: Some("me@example.org".into()),
            login_key: Some("lk-b".into()),
            ..Default::default()
        };
        assert!(
            a.same_identity(&other_login),
            "same person, signed in again"
        );
        let brand = AccountConfig {
            data_sync_id: Some("d||u".into()),
            ..other_login.clone()
        };
        assert!(!a.same_identity(&brand), "another channel");
        let second_google = AccountConfig {
            auth_user: 1,
            email: None,
            ..a.clone()
        };
        assert!(!a.same_identity(&second_google));
        assert!(!AccountConfig::default().same_identity(&AccountConfig::default()));
    }
}
