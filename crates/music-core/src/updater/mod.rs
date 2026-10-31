//! Self-update from GitHub releases. Blocking; the caller picks the thread.
//!
//! 1. **Check** ([`Updater::check`]): `GET /repos/<repo>/releases` and pick the
//!    newest release above the running version ([`release::pick_release`]): drafts
//!    never, pre-releases (marked on GitHub or `-beta`-style versions) only when
//!    asked to. The default for that is [`Version::wants_prereleases_by_default`]:
//!    on while the running version is a pre-release or `0.x`.
//! 2. **Download** ([`Updater::download`]): the asset for this install
//!    ([`InstallKind::asset_suffix`]) into `<cache>/updates/<version>/`, with
//!    progress and cancel, then its SHA-256 is compared with the release's
//!    `SHA256SUMS.txt`. No sums file, no line for the asset or another hash: the
//!    file is deleted and nothing is installed. Only
//!    `https://github.com/<repo>/releases/download/…` URLs are fetched (plus
//!    GitHub's download redirect hosts), through the app's proxy / DNS settings.
//! 3. **Install** ([`Updater::install`]), per [`InstallKind`]:
//!    * macOS bundle: `ditto -x -k` next to the bundle, `codesign --verify
//!      --strict`, same bundle id, version as promised; old bundle renamed aside,
//!      new one renamed in (rolled back if that fails), old one deleted; restart
//!      with `open -n` once this process has exited.
//!    * Windows installer: the new `-setup.exe` runs silently
//!      ([`install::INNO_SILENT_FLAGS`], `/RELAUNCH=1`), detached; it replaces the
//!      files after the app has quit and starts it again.
//!    * Windows portable: the zip is unpacked into the cache and its exe started
//!      in helper mode ([`apply::run_helper`]): waits for this process, replaces
//!      the files all-or-nothing, starts the app.
//!    * AppImage: copied next to `$APPIMAGE`, re-hashed, `chmod 755`, renamed over
//!      it; tarball install: `bin/` + `share/` replaced all-or-nothing. Both restart
//!      the new binary after this process has exited.
//!
//!    After `install` returns `Ok`, the caller must exit promptly.
//!
//! Everything is logged to `<cache>/updates/update.log` ([`UpdateLog`]). Test
//! hooks: [`Source::with_api_base`] (a local fake API) and `MUSIC_UPDATE_FAIL=
//! copy|swap|verify|launch` (fail at that step, to check the rollback).

pub mod apply;
mod download;
mod install;
pub mod kind;
mod log;
pub mod release;
pub mod sums;
pub mod version;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub use apply::{HELPER_FLAG, run_helper};
pub use download::{GITHUB_API, Source};
pub use install::INNO_SILENT_FLAGS;
pub use kind::{Arch, InstallKind, MARKER_FILE, ManualReason, Os, PackageManager};
pub use log::UpdateLog;
pub use release::{Asset, Release, pick_release};
pub use sums::{SUMS_FILE, parse_sums, sha256_file};
pub use version::Version;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    /// The request failed (offline, DNS, proxy, HTTP status).
    Network(String),
    /// The API answered with something that isn't a release list.
    BadResponse(String),
    /// A URL outside the allowed GitHub release locations.
    UntrustedUrl(String),
    /// The release has no package for this install.
    NoAsset,
    /// The release has no `SHA256SUMS.txt`.
    NoChecksums,
    /// `SHA256SUMS.txt` doesn't list the package.
    NotListed(String),
    ChecksumMismatch {
        file: String,
        expected: String,
        actual: String,
    },
    Cancelled,
    /// The install's folder can't be written by this user.
    NotWritable(PathBuf),
    /// This copy can't update itself.
    Manual(ManualReason),
    /// The package isn't what it should be (signature, bundle id, layout).
    InvalidPackage(String),
    Io(String),
    /// Installing failed (the old version is still in place).
    Install(String),
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateError::Network(e) => write!(f, "network: {e}"),
            UpdateError::BadResponse(e) => write!(f, "unexpected response: {e}"),
            UpdateError::UntrustedUrl(url) => write!(f, "refused to download from {url}"),
            UpdateError::NoAsset => f.write_str("the release has no package for this install"),
            UpdateError::NoChecksums => write!(f, "the release has no {SUMS_FILE}"),
            UpdateError::NotListed(name) => write!(f, "{SUMS_FILE} doesn't list {name}"),
            UpdateError::ChecksumMismatch {
                file,
                expected,
                actual,
            } => write!(f, "SHA-256 of {file} is {actual}, expected {expected}"),
            UpdateError::Cancelled => f.write_str("cancelled"),
            UpdateError::NotWritable(dir) => write!(f, "{} isn't writable", dir.display()),
            UpdateError::Manual(reason) => write!(f, "can't update this copy ({reason:?})"),
            UpdateError::InvalidPackage(e) => write!(f, "invalid package: {e}"),
            UpdateError::Io(e) => write!(f, "{e}"),
            UpdateError::Install(e) => write!(f, "install failed: {e}"),
        }
    }
}

impl std::error::Error for UpdateError {}

impl From<std::io::Error> for UpdateError {
    fn from(e: std::io::Error) -> Self {
        UpdateError::Io(e.to_string())
    }
}

/// A newer release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub version: Version,
    pub tag: String,
    pub page_url: String,
    pub prerelease: bool,
    /// The package for this install (`None`: link only).
    pub asset: Option<Asset>,
    pub sums: Option<Asset>,
}

/// A downloaded, verified package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    pub version: Version,
    pub file: PathBuf,
    /// Lower-case hex, as verified.
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct Updater {
    pub source: Source,
    pub app_id: String,
    pub current: Version,
    /// `<cache>/updates`.
    pub dir: PathBuf,
    pub kind: InstallKind,
    pub arch: Arch,
    pub log: UpdateLog,
}

impl Updater {
    pub fn new(
        source: Source,
        app_id: &str,
        current: Version,
        dir: PathBuf,
        kind: InstallKind,
    ) -> Self {
        let log = UpdateLog::new(dir.join("update.log"));
        Updater {
            source,
            app_id: app_id.to_owned(),
            current,
            dir,
            kind,
            arch: Arch::current(),
            log,
        }
    }

    fn version_dir(&self, version: &Version) -> PathBuf {
        self.dir.join(version.to_string())
    }

    /// Why this copy can't update in place right now, if it can't.
    pub fn blocker(&self) -> Option<UpdateError> {
        if let InstallKind::Manual(reason) = &self.kind {
            return Some(UpdateError::Manual(reason.clone()));
        }
        if self.kind.asset_suffix(self.arch).is_none() {
            return Some(UpdateError::Manual(ManualReason::UnsupportedPlatform));
        }
        self.kind.unwritable_dir().map(UpdateError::NotWritable)
    }

    /// The newest release to offer, or `None` when up to date.
    pub fn check(&self, include_prereleases: bool) -> Result<Option<Offer>, UpdateError> {
        let json = download::fetch_releases(&self.source)?;
        let releases = Release::list_from_json(&json);
        if releases.is_empty() && !json.is_array() {
            return Err(UpdateError::BadResponse(
                json.get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("no releases")
                    .to_owned(),
            ));
        }
        let picked = pick_release(&releases, &self.current, include_prereleases);
        self.log.line(&format!(
            "checked {} ({} releases, running {}, pre-releases {}): {}",
            self.source.releases_url(),
            releases.len(),
            self.current,
            if include_prereleases { "on" } else { "off" },
            picked.map_or("up to date".to_owned(), |r| format!("{} available", r.tag))
        ));
        Ok(picked.map(|r| Offer {
            version: r.version.clone(),
            tag: r.tag.clone(),
            page_url: r.page_url.clone(),
            prerelease: r.is_prerelease(),
            asset: self
                .kind
                .asset_suffix(self.arch)
                .and_then(|suffix| r.package(&self.app_id, suffix))
                .cloned(),
            sums: r.sums().cloned(),
        }))
    }

    /// Downloads and verifies the offer's package (reusing an already verified
    /// download of it).
    pub fn download(
        &self,
        offer: &Offer,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<Staged, UpdateError> {
        if let Some(blocker) = self.blocker() {
            return Err(blocker);
        }
        let asset = offer.asset.as_ref().ok_or(UpdateError::NoAsset)?;
        let sums_asset = offer.sums.as_ref().ok_or(UpdateError::NoChecksums)?;
        let sums = parse_sums(&download::fetch_asset_text(&self.source, &sums_asset.url)?);
        let expected = sums
            .get(&asset.name)
            .cloned()
            .ok_or_else(|| UpdateError::NotListed(asset.name.clone()))?;
        let dir = self.version_dir(&offer.version);
        std::fs::create_dir_all(&dir)?;
        let file = dir.join(&asset.name);
        if file.exists() && sha256_file(&file).ok().as_ref() == Some(&expected) {
            self.log.line(&format!(
                "{} already downloaded and verified",
                file.display()
            ));
            progress(asset.size, Some(asset.size));
            return Ok(Staged {
                version: offer.version.clone(),
                file,
                sha256: expected,
            });
        }
        self.log.line(&format!("downloading {}", asset.url));
        let actual =
            match download::download_file(&self.source, &asset.url, &file, cancel, progress) {
                Ok(actual) => actual,
                Err(e) => {
                    self.log.line(&format!("download failed: {e}"));
                    return Err(e);
                }
            };
        if actual != expected {
            let _ = std::fs::remove_file(&file);
            let e = UpdateError::ChecksumMismatch {
                file: asset.name.clone(),
                expected,
                actual,
            };
            self.log
                .line(&format!("REFUSED: {e}; the download was deleted"));
            return Err(e);
        }
        self.log.line(&format!(
            "verified {} (SHA-256 {actual} matches {SUMS_FILE})",
            asset.name
        ));
        Ok(Staged {
            version: offer.version.clone(),
            file,
            sha256: actual,
        })
    }

    /// Installs a staged package. With `relaunch`, the new version starts once this
    /// process has exited. After `Ok` the caller must exit.
    pub fn install(&self, staged: &Staged, relaunch: bool) -> Result<(), UpdateError> {
        if let Some(blocker) = self.blocker() {
            return Err(blocker);
        }
        // Never run or unpack anything that isn't the verified file.
        let actual = sha256_file(&staged.file)?;
        if actual != staged.sha256 {
            let e = UpdateError::ChecksumMismatch {
                file: staged.file.display().to_string(),
                expected: staged.sha256.clone(),
                actual,
            };
            self.log.line(&format!("REFUSED: {e}"));
            return Err(e);
        }
        self.log.line(&format!(
            "installing {} → {} ({}, {})",
            staged.version,
            self.kind
                .path()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            self.kind.name(),
            if relaunch { "restart" } else { "no restart" }
        ));
        let result = install::install(self, staged, relaunch);
        match &result {
            Ok(()) => self.log.line(&format!("installed {}", staged.version)),
            Err(e) => self
                .log
                .line(&format!("install failed, old version kept: {e}")),
        }
        result
    }

    /// Removes downloads of versions up to the running one and what an
    /// interrupted update left next to the install. Call once at startup.
    pub fn cleanup(&self) {
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let stale =
                    Version::parse(&name.to_string_lossy()).is_some_and(|v| v <= self.current);
                if stale && entry.path().is_dir() {
                    let _ = std::fs::remove_dir_all(entry.path());
                    self.log
                        .line(&format!("removed {}", entry.path().display()));
                }
            }
        }
        match &self.kind {
            InstallKind::MacBundle { bundle } => install::remove_mac_leftovers(bundle),
            InstallKind::WindowsPortable { exe } | InstallKind::WindowsInstaller { exe } => {
                if let Some(dir) = exe.parent() {
                    apply::remove_leftovers(dir);
                }
            }
            InstallKind::LinuxTarball { prefix, .. } => {
                apply::remove_leftovers(&prefix.join("bin"))
            }
            InstallKind::AppImage { .. } | InstallKind::Manual(_) => {}
        }
    }
}

thread_local! {
    static FAIL_AT: std::cell::RefCell<Option<&'static str>> = const { std::cell::RefCell::new(None) };
}

/// Test hook: fails with an install error at `stage` when `MUSIC_UPDATE_FAIL`
/// names it (or a unit test set it with [`with_fail_point`]).
pub(crate) fn fail_point(stage: &str) -> Result<(), UpdateError> {
    let forced = FAIL_AT.with(|f| *f.borrow() == Some(stage))
        || std::env::var("MUSIC_UPDATE_FAIL").is_ok_and(|v| v.split(',').any(|s| s == stage));
    if forced {
        Err(UpdateError::Install(format!(
            "simulated failure at {stage}"
        )))
    } else {
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn with_fail_point<T>(stage: &'static str, f: impl FnOnce() -> T) -> T {
    FAIL_AT.with(|s| *s.borrow_mut() = Some(stage));
    let out = f();
    FAIL_AT.with(|s| *s.borrow_mut() = None);
    out
}

/// `<cache>/updates` for an app: under `config_override/cache` when the config
/// dir is overridden (tests), else the platform cache dir.
pub fn default_dir(
    platform_cache: Option<&Path>,
    config_override: Option<&Path>,
) -> Option<PathBuf> {
    match config_override {
        Some(config) => Some(config.join("cache").join("updates")),
        None => platform_cache.map(|c| c.join("updates")),
    }
}

#[cfg(test)]
mod tests;
