// Browser table and discovery ported from refryt crates/refryt/src/browser_cookies.rs (KNOWN, readable_on, cookies_used, detected_in) — © gridoe and refryt contributors, GPL-3.0
//! Signing in with the YouTube cookies of a browser installed on this computer.
//!
//! The user picks a browser profile where they are signed in to music.youtube.com;
//! its cookie store is read here, on their own machine and by their own action, and
//! only the `youtube.com` cookies are taken (nothing else is read from the
//! database: the query asks for those hosts only). Cookie values never appear in
//! `Debug` output, errors or logs.
//!
//! * **Chromium** browsers (Chrome, Edge, Brave, Arc, Vivaldi, Opera, Chromium) keep
//!   SQLite with values encrypted by a key from the OS ([`chromium`]):
//!   * macOS: AES-128-CBC with a key derived from the "<Browser> Safe Storage" password
//!     in the login Keychain (macOS asks the user before handing it out);
//!   * Windows: AES-256-GCM with a key kept in `Local State`, itself sealed with DPAPI.
//!     Chrome 127+ (and Edge, Brave… that adopted it) use **app-bound encryption**
//!     (`v20` values, `app_bound_encrypted_key`), which only the browser's own elevation
//!     service can open: such profiles are listed as blocked, with the reason;
//!   * Linux: AES-128-CBC with the password from libsecret / KWallet (`v11`) or the
//!     built-in "peanuts" password (`v10`).
//!
//!   The platform half (Keychain, DPAPI, libsecret) is the app's; this module takes
//!   the resulting [`chromium::Keys`].
//! * **Firefox** keeps plain SQLite (`cookies.sqlite`, [`firefox`]).
//! * **Safari** (macOS) keeps `Cookies.binarycookies` in its container, which needs
//!   Full Disk Access ([`safari`]).
//!
//! The databases are copied to a private temporary folder first (a running browser
//! holds them open), and the copy is deleted when the import ends.
//!
//! **Not verified against real browsers here**: the readers are tested against
//! fixture databases built with known test keys. Real stores (and the Keychain,
//! DPAPI and libsecret prompts) were not exercised.

pub mod chromium;
pub mod firefox;
pub mod safari;

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A browser this importer knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Browser {
    Chrome,
    Edge,
    Brave,
    Firefox,
    Safari,
    Arc,
    Vivaldi,
    Opera,
    Chromium,
}

/// How a browser stores its cookies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Chromium,
    Firefox,
    Safari,
}

/// The operating system whose folder layout to use (tests pass another one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    MacOs,
    Windows,
    Linux,
}

impl Os {
    pub const CURRENT: Os = if cfg!(target_os = "macos") {
        Os::MacOs
    } else if cfg!(windows) {
        Os::Windows
    } else {
        Os::Linux
    };
}

impl Browser {
    pub const ALL: [Browser; 9] = [
        Browser::Chrome,
        Browser::Edge,
        Browser::Brave,
        Browser::Firefox,
        Browser::Safari,
        Browser::Arc,
        Browser::Vivaldi,
        Browser::Opera,
        Browser::Chromium,
    ];

    /// The product name (not translated).
    pub fn label(self) -> &'static str {
        match self {
            Browser::Chrome => "Chrome",
            Browser::Edge => "Edge",
            Browser::Brave => "Brave",
            Browser::Firefox => "Firefox",
            Browser::Safari => "Safari",
            Browser::Arc => "Arc",
            Browser::Vivaldi => "Vivaldi",
            Browser::Opera => "Opera",
            Browser::Chromium => "Chromium",
        }
    }

    pub fn engine(self) -> Engine {
        match self {
            Browser::Firefox => Engine::Firefox,
            Browser::Safari => Engine::Safari,
            _ => Engine::Chromium,
        }
    }

    /// The macOS Keychain item (service, account) holding the cookie password.
    pub fn safe_storage(self) -> Option<(&'static str, &'static str)> {
        Some(match self {
            Browser::Chrome => ("Chrome Safe Storage", "Chrome"),
            Browser::Edge => ("Microsoft Edge Safe Storage", "Microsoft Edge"),
            Browser::Brave => ("Brave Safe Storage", "Brave"),
            Browser::Arc => ("Arc Safe Storage", "Arc"),
            Browser::Vivaldi => ("Vivaldi Safe Storage", "Vivaldi"),
            Browser::Opera => ("Opera Safe Storage", "Opera"),
            Browser::Chromium => ("Chromium Safe Storage", "Chromium"),
            Browser::Firefox | Browser::Safari => return None,
        })
    }

    /// Linux: the libsecret `application` attribute and the KWallet folder entry of
    /// the cookie password.
    pub fn linux_keyring(self) -> Option<(&'static str, &'static str)> {
        Some(match self {
            Browser::Chrome => ("chrome", "Chrome Safe Storage"),
            Browser::Edge => ("microsoft-edge", "Microsoft Edge Safe Storage"),
            Browser::Brave => ("brave", "Brave Safe Storage"),
            Browser::Vivaldi => ("vivaldi", "Vivaldi Safe Storage"),
            Browser::Opera => ("opera", "Opera Safe Storage"),
            Browser::Chromium => ("chromium", "Chromium Safe Storage"),
            Browser::Arc | Browser::Firefox | Browser::Safari => return None,
        })
    }

    /// Where its data lives under the home folder on `os` (a `*` at the end of a
    /// segment matches any suffix). Firefox and Chromium: the folder holding the
    /// profiles; Safari: the cookie file itself.
    fn roots(self, os: Os) -> &'static [&'static str] {
        use Browser::*;
        match (self, os) {
            (Chrome, Os::MacOs) => &["Library/Application Support/Google/Chrome"],
            (Chrome, Os::Windows) => &["AppData/Local/Google/Chrome/User Data"],
            (Chrome, Os::Linux) => &[
                ".config/google-chrome",
                ".var/app/com.google.Chrome/config/google-chrome",
            ],
            (Edge, Os::MacOs) => &["Library/Application Support/Microsoft Edge"],
            (Edge, Os::Windows) => &["AppData/Local/Microsoft/Edge/User Data"],
            (Edge, Os::Linux) => &[
                ".config/microsoft-edge",
                ".var/app/com.microsoft.Edge/config/microsoft-edge",
            ],
            (Brave, Os::MacOs) => &["Library/Application Support/BraveSoftware/Brave-Browser"],
            (Brave, Os::Windows) => &["AppData/Local/BraveSoftware/Brave-Browser/User Data"],
            (Brave, Os::Linux) => &[
                ".config/BraveSoftware/Brave-Browser",
                ".var/app/com.brave.Browser/config/BraveSoftware/Brave-Browser",
                "snap/brave/current/.config/BraveSoftware/Brave-Browser",
            ],
            (Firefox, Os::MacOs) => &["Library/Application Support/Firefox"],
            (Firefox, Os::Windows) => &["AppData/Roaming/Mozilla/Firefox"],
            (Firefox, Os::Linux) => &[
                ".mozilla/firefox",
                ".config/mozilla/firefox",
                "snap/firefox/common/.mozilla/firefox",
                ".var/app/org.mozilla.firefox/.mozilla/firefox",
            ],
            (Safari, Os::MacOs) => &[
                "Library/Containers/com.apple.Safari/Data/Library/Cookies/Cookies.binarycookies",
                "Library/Cookies/Cookies.binarycookies",
            ],
            (Safari, _) => &[],
            (Arc, Os::MacOs) => &["Library/Application Support/Arc/User Data"],
            (Arc, Os::Windows) => {
                &["AppData/Local/Packages/TheBrowserCompany.Arc*/LocalCache/Local/Arc/User Data"]
            }
            (Arc, Os::Linux) => &[],
            (Vivaldi, Os::MacOs) => &["Library/Application Support/Vivaldi"],
            (Vivaldi, Os::Windows) => &["AppData/Local/Vivaldi/User Data"],
            (Vivaldi, Os::Linux) => &[
                ".config/vivaldi",
                ".var/app/com.vivaldi.Vivaldi/config/vivaldi",
            ],
            (Opera, Os::MacOs) => &["Library/Application Support/com.operasoftware.Opera"],
            (Opera, Os::Windows) => &["AppData/Roaming/Opera Software/Opera Stable"],
            (Opera, Os::Linux) => &[
                ".config/opera",
                ".var/app/com.opera.Opera/config/opera",
                "snap/opera/current/.config/opera",
            ],
            (Chromium, Os::MacOs) => &["Library/Application Support/Chromium"],
            (Chromium, Os::Windows) => &["AppData/Local/Chromium/User Data"],
            (Chromium, Os::Linux) => &[
                ".config/chromium",
                ".var/app/org.chromium.Chromium/config/chromium",
                "snap/chromium/common/chromium",
            ],
        }
    }
}

/// Why a found profile can't be imported as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocked {
    /// Windows: the browser seals its cookie key to itself (app-bound encryption,
    /// Chrome 127+); only the browser can read its cookies.
    AppBound,
    /// Windows: no `Local State` with a key next to the profiles.
    NoKey,
    /// macOS: Safari's cookies need Full Disk Access for this app.
    FullDiskAccess,
}

/// One browser profile with a cookie store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundProfile {
    pub browser: Browser,
    /// The profile's name in the browser ("Default", "Work", "Person 1"…).
    pub profile: String,
    /// The cookie database / file.
    pub cookies: PathBuf,
    /// Chromium: the "User Data" folder (with `Local State`).
    pub root: PathBuf,
    /// When the cookie store last changed (most recently used first).
    pub used: Option<SystemTime>,
    pub blocked: Option<Blocked>,
}

/// Expands `rel` under `home`, a trailing `*` in a segment matching any suffix.
fn expand(home: &Path, rel: &str) -> Vec<PathBuf> {
    let mut paths = vec![home.to_path_buf()];
    for segment in rel.split('/') {
        let mut next = Vec::new();
        for base in &paths {
            match segment.strip_suffix('*') {
                Some(prefix) => {
                    let Ok(entries) = std::fs::read_dir(base) else {
                        continue;
                    };
                    for entry in entries.flatten() {
                        if entry.file_name().to_string_lossy().starts_with(prefix) {
                            next.push(entry.path());
                        }
                    }
                }
                None => next.push(base.join(segment)),
            }
        }
        paths = next;
    }
    paths
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    out.sort();
    out
}

/// Chromium profile names from `Local State` (`profile.info_cache.<dir>.name`).
fn chromium_names(root: &Path) -> serde_json::Value {
    std::fs::read_to_string(root.join("Local State"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .unwrap_or_default()
}

/// Windows: what `Local State` says about reading the cookie key.
fn windows_block(local_state: &serde_json::Value) -> Option<Blocked> {
    let crypt = local_state.get("os_crypt");
    if crypt
        .and_then(|c| c.get("encrypted_key"))
        .and_then(|k| k.as_str())
        .is_none()
    {
        return Some(Blocked::NoKey);
    }
    crypt
        .and_then(|c| c.get("app_bound_encrypted_key"))
        .is_some()
        .then_some(Blocked::AppBound)
}

fn chromium_profiles(browser: Browser, root: &Path, os: Os, out: &mut Vec<FoundProfile>) {
    let state = chromium_names(root);
    let blocked = (os == Os::Windows).then(|| windows_block(&state)).flatten();
    // Opera keeps its one profile in the root itself.
    let mut dirs = vec![root.to_path_buf()];
    dirs.extend(subdirs(root));
    for dir in dirs {
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if matches!(name.as_str(), "System Profile" | "Guest Profile") {
            continue;
        }
        let cookies = [dir.join("Network").join("Cookies"), dir.join("Cookies")]
            .into_iter()
            .find(|p| p.is_file());
        let Some(cookies) = cookies else { continue };
        let shown = state
            .pointer(&format!("/profile/info_cache/{name}/name"))
            .and_then(|n| n.as_str())
            .filter(|n| !n.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| if dir == root { "Default".into() } else { name });
        out.push(FoundProfile {
            browser,
            profile: shown,
            used: modified(&cookies),
            cookies,
            root: root.to_path_buf(),
            blocked,
        });
    }
}

/// Firefox profile names from `profiles.ini` (`Path=` → `Name=`).
fn firefox_names(root: &Path) -> Vec<(PathBuf, String)> {
    let Ok(ini) = std::fs::read_to_string(root.join("profiles.ini")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let (mut name, mut path, mut relative) = (None::<String>, None::<String>, true);
    let mut flush = |name: &mut Option<String>, path: &mut Option<String>, relative: bool| {
        if let (Some(n), Some(p)) = (name.take(), path.take()) {
            let p = if relative {
                root.join(p)
            } else {
                PathBuf::from(p)
            };
            out.push((p, n));
        }
    };
    for line in ini.lines().map(str::trim) {
        if line.starts_with('[') {
            flush(&mut name, &mut path, relative);
            relative = true;
        } else if let Some(v) = line.strip_prefix("Name=") {
            name = Some(v.to_owned());
        } else if let Some(v) = line.strip_prefix("Path=") {
            path = Some(v.to_owned());
        } else if let Some(v) = line.strip_prefix("IsRelative=") {
            relative = v != "0";
        }
    }
    flush(&mut name, &mut path, relative);
    out
}

fn firefox_profiles(root: &Path, out: &mut Vec<FoundProfile>) {
    let names = firefox_names(root);
    // Windows and macOS keep the profiles one level down, in `Profiles`.
    for dir in subdirs(root)
        .into_iter()
        .chain(subdirs(&root.join("Profiles")))
    {
        let cookies = dir.join("cookies.sqlite");
        if !cookies.is_file() {
            continue;
        }
        let shown = names
            .iter()
            .find(|(p, _)| p == &dir)
            .map(|(_, n)| n.clone())
            .unwrap_or_else(|| {
                let folder = dir.file_name().unwrap_or_default().to_string_lossy();
                // "abcd1234.default-release" → "default-release"
                folder
                    .split_once('.')
                    .map_or(folder.to_string(), |(_, n)| n.to_owned())
            });
        out.push(FoundProfile {
            browser: Browser::Firefox,
            profile: shown,
            used: modified(&cookies),
            cookies,
            root: root.to_path_buf(),
            blocked: None,
        });
    }
}

/// The browser profiles with a cookie store under `home` on `os`, the most recently
/// used first. Nothing is opened (no database, no keyring): only file names and
/// times are looked at, plus Chromium's `Local State` / Firefox's `profiles.ini` for
/// the profile names.
pub fn discover(home: &Path, os: Os) -> Vec<FoundProfile> {
    let mut found = Vec::new();
    for browser in Browser::ALL {
        for rel in browser.roots(os) {
            for root in expand(home, rel) {
                match browser.engine() {
                    Engine::Chromium if root.is_dir() => {
                        chromium_profiles(browser, &root, os, &mut found)
                    }
                    Engine::Firefox if root.is_dir() => firefox_profiles(&root, &mut found),
                    Engine::Safari => match std::fs::metadata(&root) {
                        Ok(meta) if meta.is_file() => found.push(FoundProfile {
                            browser,
                            profile: browser.label().to_owned(),
                            used: meta.modified().ok(),
                            root: root.parent().map(Path::to_path_buf).unwrap_or_default(),
                            cookies: root,
                            blocked: None,
                        }),
                        // The container is there but closed to this app.
                        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                            found.push(FoundProfile {
                                browser,
                                profile: browser.label().to_owned(),
                                used: None,
                                root: root.parent().map(Path::to_path_buf).unwrap_or_default(),
                                cookies: root,
                                blocked: Some(Blocked::FullDiskAccess),
                            })
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
    }
    // Safari's two locations: keep the first found.
    let mut seen_safari = false;
    found.retain(|p| {
        if p.browser != Browser::Safari {
            return true;
        }
        !std::mem::replace(&mut seen_safari, true)
    });
    // Newest first; stable, so equal times keep the table's order.
    found.sort_by_key(|p| std::cmp::Reverse(p.used));
    found
}

/// One cookie read from a browser. `Debug` leaves the value out.
#[derive(Clone, PartialEq, Eq)]
pub struct Cookie {
    pub host: String,
    pub name: String,
    pub value: String,
    /// Unix seconds; `None` = a session cookie.
    pub expires: Option<i64>,
}

impl std::fmt::Debug for Cookie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cookie")
            .field("host", &self.host)
            .field("name", &self.name)
            .field("value", &format_args!("<{} bytes>", self.value.len()))
            .finish()
    }
}

/// Whether `host` is youtube.com or a subdomain (the only cookies imported).
pub fn is_youtube_host(host: &str) -> bool {
    let h = host.trim_start_matches('.').to_ascii_lowercase();
    h == "youtube.com" || h.ends_with(".youtube.com")
}

/// Why an import failed. `Display` is English (logs); the app shows its own text
/// per variant. Never contains cookie values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    /// The cookie store isn't there (any more).
    NotFound,
    /// It couldn't be copied: the browser holds it locked (close it and try again).
    Locked,
    /// No permission to read it (Safari: Full Disk Access).
    PermissionDenied,
    /// The key to decrypt it couldn't be had (Keychain access denied, no DPAPI key,
    /// no libsecret / KWallet entry). The detail is English, without secrets.
    NoKey(String),
    /// Windows app-bound encryption (`v20`): only the browser can decrypt these.
    AppBound,
    /// The values didn't decrypt with the key (another key, a newer scheme).
    Decrypt,
    /// No signed-in youtube.com session in that profile.
    NotSignedIn,
    /// The store is damaged or of an unknown format.
    Format(String),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::NotFound => write!(f, "the cookie store is gone"),
            ImportError::Locked => write!(f, "the browser keeps its cookies locked"),
            ImportError::PermissionDenied => write!(f, "no permission to read the cookies"),
            ImportError::NoKey(why) => write!(f, "no key to decrypt the cookies: {why}"),
            ImportError::AppBound => write!(f, "app-bound encryption (v20)"),
            ImportError::Decrypt => write!(f, "the cookies didn't decrypt"),
            ImportError::NotSignedIn => write!(f, "not signed in to YouTube in that profile"),
            ImportError::Format(why) => write!(f, "unreadable cookie store: {why}"),
        }
    }
}

impl std::error::Error for ImportError {}

impl From<rusqlite::Error> for ImportError {
    fn from(e: rusqlite::Error) -> Self {
        match e {
            rusqlite::Error::SqliteFailure(code, _)
                if code.code == rusqlite::ErrorCode::DatabaseBusy
                    || code.code == rusqlite::ErrorCode::DatabaseLocked =>
            {
                ImportError::Locked
            }
            other => ImportError::Format(other.to_string()),
        }
    }
}

fn io_error(e: std::io::Error) -> ImportError {
    match e.kind() {
        std::io::ErrorKind::NotFound => ImportError::NotFound,
        std::io::ErrorKind::PermissionDenied => ImportError::PermissionDenied,
        // Windows: a sharing violation while the browser runs; elsewhere rare.
        _ => ImportError::Locked,
    }
}

/// A private copy of a database (and its `-wal` / `-journal`), deleted on drop.
pub(crate) struct TempCopy {
    dir: PathBuf,
    pub(crate) path: PathBuf,
}

impl TempCopy {
    pub(crate) fn of(src: &Path) -> Result<Self, ImportError> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "cratetune-cookies-{}-{nanos}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).map_err(io_error)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        let copy = TempCopy {
            path: dir.join(src.file_name().unwrap_or_default()),
            dir,
        };
        std::fs::copy(src, &copy.path).map_err(io_error)?;
        for suffix in ["-wal", "-journal"] {
            let mut side = src.as_os_str().to_owned();
            side.push(suffix);
            let side = PathBuf::from(side);
            if side.is_file() {
                let mut to = copy.path.as_os_str().to_owned();
                to.push(suffix);
                let _ = std::fs::copy(&side, PathBuf::from(to));
            }
        }
        Ok(copy)
    }
}

impl Drop for TempCopy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub(crate) fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The `Cookie` header for music.youtube.com from `cookies`: youtube.com ones that
/// haven't expired, a more specific host (music.youtube.com) winning over
/// `.youtube.com` for the same name. [`ImportError::NotSignedIn`] without a cookie
/// that can sign requests (`SAPISID` / `__Secure-3PAPISID`).
pub fn cookie_header(cookies: &[Cookie]) -> Result<String, ImportError> {
    let now = unix_now();
    let mut usable: Vec<&Cookie> = cookies
        .iter()
        .filter(|c| is_youtube_host(&c.host) && !c.name.is_empty())
        .filter(|c| c.expires.is_none_or(|t| t > now))
        .collect();
    // Longest host first: music.youtube.com before .youtube.com.
    usable.sort_by_key(|c| std::cmp::Reverse(c.host.trim_start_matches('.').len()));
    let mut seen = std::collections::HashSet::new();
    let header = usable
        .into_iter()
        .filter(|c| seen.insert(c.name.as_str()))
        .map(|c| format!("{}={}", c.name, c.value))
        .collect::<Vec<_>>()
        .join("; ");
    if crate::innertube::Auth::new(header.clone()).is_signed_in() {
        Ok(header)
    } else {
        Err(ImportError::NotSignedIn)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A fresh folder under the system temp dir (the tests' fake homes).
    pub(crate) fn temp_home(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cratetune-browser-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(home: &Path, rel: &str, age_secs: u64) {
        let path = home.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let f = std::fs::File::create(&path).unwrap();
        f.set_modified(SystemTime::now() - std::time::Duration::from_secs(age_secs))
            .unwrap();
    }

    fn c(host: &str, name: &str, value: &str) -> Cookie {
        Cookie {
            host: host.into(),
            name: name.into(),
            value: value.into(),
            expires: None,
        }
    }

    #[test]
    fn header_keeps_youtube_cookies_only() {
        let mut expired = c(".youtube.com", "OLD", "x");
        expired.expires = Some(1);
        let cookies = vec![
            c(".google.com", "SID", "google"),
            c(".youtube.com", "SAPISID", "sap"),
            c(".youtube.com", "PREF", "general"),
            c("music.youtube.com", "PREF", "music"),
            c(".notyoutube.com", "X", "1"),
            expired,
        ];
        let header = cookie_header(&cookies).unwrap();
        assert_eq!(header, "PREF=music; SAPISID=sap");
        assert_eq!(
            cookie_header(&[c(".youtube.com", "PREF", "x")]),
            Err(ImportError::NotSignedIn)
        );
        assert!(!format!("{:?}", cookies[1]).contains("sap\""));
        assert!(!format!("{:?}", cookies[1]).contains("\"sap"));
    }

    #[test]
    fn discovery_lists_profiles_newest_first() {
        let home = temp_home("discover");
        touch(
            &home,
            "Library/Application Support/Google/Chrome/Default/Network/Cookies",
            100,
        );
        touch(
            &home,
            "Library/Application Support/Google/Chrome/Profile 1/Cookies",
            10,
        );
        touch(
            &home,
            "Library/Application Support/Google/Chrome/Guest Profile/Network/Cookies",
            1,
        );
        std::fs::write(
            home.join("Library/Application Support/Google/Chrome/Local State"),
            r#"{"profile":{"info_cache":{"Profile 1":{"name":"Work"},"Default":{"name":"Me"}}}}"#,
        )
        .unwrap();
        touch(
            &home,
            "Library/Application Support/Firefox/Profiles/ab12.default-release/cookies.sqlite",
            50,
        );
        std::fs::write(
            home.join("Library/Application Support/Firefox/profiles.ini"),
            "[Profile0]\nName=default-release\nIsRelative=1\nPath=Profiles/ab12.default-release\n",
        )
        .unwrap();
        touch(
            &home,
            "Library/Application Support/com.operasoftware.Opera/Cookies",
            70,
        );
        touch(
            &home,
            "Library/Application Support/Arc/User Data/Default/Cookies",
            200,
        );
        touch(&home, "Library/Cookies/Cookies.binarycookies", 300);
        // Windows / Linux folders are ignored on macOS.
        touch(
            &home,
            "AppData/Local/Google/Chrome/User Data/Default/Network/Cookies",
            0,
        );

        let found = discover(&home, Os::MacOs);
        let names: Vec<_> = found
            .iter()
            .map(|p| format!("{}:{}", p.browser.label(), p.profile))
            .collect();
        assert_eq!(
            names,
            [
                "Chrome:Work",
                "Firefox:default-release",
                "Opera:Default",
                "Chrome:Me",
                "Arc:Default",
                "Safari:Safari"
            ]
        );
        assert!(found.iter().all(|p| p.blocked.is_none()));
        assert!(found[0].cookies.ends_with("Profile 1/Cookies"));
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Windows: app-bound encryption and a missing key are named; Arc's package folder
    /// is found by its prefix.
    #[test]
    fn windows_profiles_say_what_blocks_them() {
        let home = temp_home("windows");
        let chrome = "AppData/Local/Google/Chrome/User Data";
        touch(&home, &format!("{chrome}/Default/Network/Cookies"), 5);
        std::fs::write(
            home.join(chrome).join("Local State"),
            r#"{"os_crypt":{"app_bound_encrypted_key":"QVBQQg==","encrypted_key":"RFBBUEk="}}"#,
        )
        .unwrap();
        let brave = "AppData/Local/BraveSoftware/Brave-Browser/User Data";
        touch(&home, &format!("{brave}/Default/Network/Cookies"), 10);
        std::fs::write(
            home.join(brave).join("Local State"),
            r#"{"os_crypt":{"encrypted_key":"RFBBUEk="}}"#,
        )
        .unwrap();
        touch(
            &home,
            "AppData/Local/Vivaldi/User Data/Default/Network/Cookies",
            20,
        );
        touch(
            &home,
            "AppData/Local/Packages/TheBrowserCompany.Arc_ttt1ap7aakyb4/LocalCache/Local/Arc/User Data/Default/Network/Cookies",
            30,
        );
        touch(
            &home,
            "AppData/Roaming/Mozilla/Firefox/Profiles/x.default/cookies.sqlite",
            40,
        );
        let found = discover(&home, Os::Windows);
        let rows: Vec<_> = found.iter().map(|p| (p.browser, p.blocked)).collect();
        assert_eq!(
            rows,
            [
                (Browser::Chrome, Some(Blocked::AppBound)),
                (Browser::Brave, None),
                (Browser::Vivaldi, Some(Blocked::NoKey)),
                (Browser::Arc, Some(Blocked::NoKey)),
                (Browser::Firefox, None),
            ]
        );
        assert_eq!(found[4].profile, "default");
        // On Linux none of these folders exist.
        assert!(discover(&home, Os::Linux).is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn temp_copies_are_deleted() {
        let home = temp_home("copy");
        let src = home.join("Cookies");
        std::fs::write(&src, b"db").unwrap();
        std::fs::write(home.join("Cookies-wal"), b"wal").unwrap();
        let copy = TempCopy::of(&src).unwrap();
        let dir = copy.path.parent().unwrap().to_path_buf();
        assert_eq!(std::fs::read(&copy.path).unwrap(), b"db");
        assert!(dir.join("Cookies-wal").is_file());
        drop(copy);
        assert!(!dir.exists());
        assert_eq!(
            TempCopy::of(&home.join("missing")).err(),
            Some(ImportError::NotFound)
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}
