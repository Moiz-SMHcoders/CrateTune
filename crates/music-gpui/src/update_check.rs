//! Updates: the check against the GitHub releases of [`identity::UPDATE_REPO`] and
//! the self-update flow on top of [`music_core::updater`].
//!
//! * **Check:** at launch and every 24 hours while "Check for updates
//!   automatically" is on, and on "Check now" / the "Check for Updates…" menu item
//!   (always). An empty repo = no requests and no UI.
//! * **Update:** "Update to X.Y.Z" downloads the package for this install (progress,
//!   cancel), verifies it against the release's `SHA256SUMS.txt`, then offers
//!   "Restart to finish", which installs it and starts the new version. Copies that
//!   can't replace themselves (built from source, a read-only folder, a package
//!   manager) get "Download manually" (the release page) instead.
//! * **Install automatically** (off by default): a found update is downloaded in
//!   the background, the app asks to restart, and it is installed on quit if the
//!   user didn't restart before.
//!
//! Test hooks: `MUSIC_UPDATE_REPO=owner/name`, `MUSIC_UPDATE_API=http://127.0.0.1:PORT`
//! (a fake GitHub API), `MUSIC_UPDATE_FAKE_VERSION=0.0.1` (pretend to run that
//! version), `MUSIC_UPDATE_FAIL=copy|swap|verify|launch` (fail an install step).
//! Script actions are in `update_script.rs`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use gpui::{App, Context, Global};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::notification::Notification;
use gpui_component::{Sizable as _, WindowExt as _};
use music_core::updater::{
    InstallKind, ManualReason, Offer, Source, Staged, UpdateError, Updater, Version,
};

use crate::app::MusicApp;
use crate::identity;
use crate::util::off_thread;

/// How often a running app checks again.
const RECHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// What the last step left for a retry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retry {
    Check,
    Download,
    Install,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum UpdateStatus {
    #[default]
    Unknown,
    Checking,
    UpToDate,
    /// A newer release; `installable`: this copy can update itself.
    Available {
        version: String,
        url: String,
        installable: bool,
    },
    Downloading {
        version: String,
        done: u64,
        total: Option<u64>,
    },
    /// Downloaded and verified: "Restart to finish".
    Ready {
        version: String,
    },
    Installing {
        version: String,
    },
    /// `message` is translated; `url` the manual download page.
    Failed {
        message: String,
        url: Option<String>,
        retry: Retry,
    },
}

impl UpdateStatus {
    fn busy(&self) -> bool {
        matches!(
            self,
            UpdateStatus::Checking
                | UpdateStatus::Downloading { .. }
                | UpdateStatus::Installing { .. }
        )
    }
}

#[derive(Default)]
struct Progress {
    done: AtomicU64,
    /// `u64::MAX` = unknown.
    total: AtomicU64,
}

#[derive(Default)]
struct UpdateState {
    status: UpdateStatus,
    /// `None` without a repo or a cache directory.
    updater: Option<Updater>,
    offer: Option<Offer>,
    /// Why this copy can't update itself (link only).
    blocker: Option<UpdateError>,
    staged: Option<Staged>,
    cancel: Option<Arc<AtomicBool>>,
    progress: Arc<Progress>,
}
impl Global for UpdateState {}

/// The repository checked (`owner/name`), if the feature is enabled.
pub fn repo() -> Option<String> {
    std::env::var("MUSIC_UPDATE_REPO")
        .ok()
        .filter(|r| !r.trim().is_empty())
        .or_else(|| Some(identity::UPDATE_REPO.to_owned()))
        .filter(|r| r.contains('/'))
}

/// The running version (`MUSIC_UPDATE_FAKE_VERSION` pretends another one).
pub fn current_version() -> String {
    std::env::var("MUSIC_UPDATE_FAKE_VERSION")
        .ok()
        .filter(|v| Version::parse(v).is_some())
        .unwrap_or_else(|| identity::VERSION.to_owned())
}

fn releases_page(repo: &str) -> String {
    format!("https://github.com/{repo}/releases")
}

fn build_updater() -> Option<Updater> {
    let repo = repo()?;
    let source = match std::env::var("MUSIC_UPDATE_API")
        .ok()
        .filter(|v| !v.is_empty())
    {
        Some(api) => Source::with_api_base(&repo, &api),
        None => Source::github(&repo),
    };
    let config_override = std::env::var_os("MUSIC_CONFIG_DIR")
        .filter(|d| !d.is_empty())
        .map(std::path::PathBuf::from);
    let platform_cache =
        directories::ProjectDirs::from("", "", identity::APP_ID).map(|d| d.cache_dir().to_owned());
    let dir =
        music_core::updater::default_dir(platform_cache.as_deref(), config_override.as_deref())?;
    let current = Version::parse(&current_version())?;
    Some(Updater::new(
        source,
        identity::APP_ID,
        current,
        dir,
        InstallKind::detect_current(identity::APP_ID),
    ))
}

fn state(cx: &mut App) -> &mut UpdateState {
    if !cx.has_global::<UpdateState>() {
        cx.set_global(UpdateState {
            updater: build_updater(),
            ..Default::default()
        });
    }
    cx.global_mut::<UpdateState>()
}

pub fn status(cx: &App) -> UpdateStatus {
    cx.try_global::<UpdateState>()
        .map(|s| s.status.clone())
        .unwrap_or_default()
}

/// The release page of the offered version (or all releases).
pub fn download_page(cx: &App) -> Option<String> {
    cx.try_global::<UpdateState>()
        .and_then(|s| s.offer.as_ref())
        .map(|o| o.page_url.clone())
        .filter(|u| !u.is_empty())
        .or_else(|| repo().map(|r| releases_page(&r)))
}

/// Why this copy can only link to the download (translated), once a check ran.
pub fn blocker_text(cx: &App) -> Option<String> {
    cx.try_global::<UpdateState>()
        .and_then(|s| s.blocker.as_ref())
        .map(describe)
}

/// For `@update-status`: install kind, path, staged file, log.
pub fn diagnostics(cx: &App) -> String {
    let Some(s) = cx.try_global::<UpdateState>() else {
        return "not initialized".into(); // i18n-ignore
    };
    let Some(u) = &s.updater else {
        return "no updater (no repo or cache dir)".into(); // i18n-ignore
    };
    format!(
        "kind={} path={} api={} current={} offer={} staged={} blocker={} log={}", // i18n-ignore
        u.kind.name(),
        u.kind
            .path()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        u.source.api_base,
        u.current,
        s.offer.as_ref().map(|o| o.tag.as_str()).unwrap_or("-"),
        s.staged
            .as_ref()
            .map(|st| st.file.display().to_string())
            .unwrap_or_else(|| "-".into()),
        s.blocker
            .as_ref()
            .map(|b| b.to_string())
            .unwrap_or_else(|| "-".into()),
        u.log.path().display()
    )
}

/// An updater error in plain words.
pub fn describe(error: &UpdateError) -> String {
    match error {
        UpdateError::Network(e) => t!("update.error.network", error = e),
        UpdateError::BadResponse(e) => t!("update.error.bad_response", error = e),
        UpdateError::UntrustedUrl(_) => t!("update.error.untrusted").to_owned(),
        UpdateError::NoAsset => t!("update.error.no_asset").to_owned(),
        UpdateError::NoChecksums => t!("update.error.no_checksums").to_owned(),
        UpdateError::NotListed(_) => t!("update.error.not_listed").to_owned(),
        UpdateError::ChecksumMismatch { .. } => t!("update.error.mismatch").to_owned(),
        UpdateError::Cancelled => t!("update.error.cancelled").to_owned(),
        UpdateError::NotWritable(dir) => t!("update.error.not_writable", path = dir.display()),
        UpdateError::Manual(ManualReason::DevBuild) => t!("update.error.dev_build").to_owned(),
        UpdateError::Manual(ManualReason::Translocated) => {
            t!("update.error.translocated").to_owned()
        }
        UpdateError::Manual(ManualReason::SystemPackage) => {
            t!("update.error.system_package").to_owned()
        }
        UpdateError::Manual(ManualReason::UnsupportedPlatform) => {
            t!("update.error.no_asset").to_owned()
        }
        UpdateError::InvalidPackage(e) => t!("update.error.invalid", error = e),
        UpdateError::Io(e) => t!("update.error.io", error = e),
        UpdateError::Install(e) => t!("update.error.install", error = e),
    }
}

/// "Include pre-releases" in effect: the saved choice, else on while the running
/// version is a pre-release or `0.x`.
pub fn include_prereleases(prefs: &crate::settings::prefs::UpdatePrefs) -> bool {
    let current = Version::parse(&current_version()).unwrap_or(Version::new(0, 0, 0));
    prefs
        .include_prereleases
        .unwrap_or_else(|| current.wants_prereleases_by_default())
}

/// `12.3 MB of 31.0 MB` / `12.3 MB`.
pub fn progress_text(done: u64, total: Option<u64>) -> String {
    use crate::pages::widgets::format_bytes;
    match total {
        Some(total) => t!(
            "update.progress",
            done = format_bytes(done),
            total = format_bytes(total)
        ),
        None => format_bytes(done),
    }
}

type AppAction = fn(&mut MusicApp, &mut Context<MusicApp>);

/// Id of the update notifications (a new one replaces the previous one).
struct UpdateNotification;

impl MusicApp {
    fn update_prefs(&self, cx: &App) -> crate::settings::prefs::UpdatePrefs {
        self.settings.read(cx).settings().updates
    }

    pub fn include_prereleases(&self, cx: &App) -> bool {
        include_prereleases(&self.update_prefs(cx))
    }

    fn set_update_status(&mut self, status: UpdateStatus, cx: &mut Context<Self>) {
        state(cx).status = status;
        cx.notify();
        self.settings.update(cx, |_, cx| cx.notify());
    }

    /// A notification in the main window, optionally with one button.
    fn update_notification(
        &self,
        message: String,
        action: Option<(&'static str, AppAction)>,
        cx: &mut Context<Self>,
    ) {
        let Some(window) = self.main_window else {
            return;
        };
        cx.defer(move |cx| {
            window
                .update(cx, |_, window, cx| {
                    let mut note = Notification::new()
                        .id::<UpdateNotification>()
                        .message(message);
                    if let Some((label, run)) = action {
                        note = note.action(move |_, _, _| {
                            Button::new("update-notification-action")
                                .small()
                                .primary()
                                .label(label)
                                .on_click(move |_, _, cx| {
                                    crate::app::update_app(cx, run);
                                })
                        });
                    }
                    window.push_notification(note, cx);
                })
                .ok();
        });
    }

    /// Startup: tidy old downloads, check (if enabled) and keep checking every 24 h.
    pub fn start_update_checks(&mut self, cx: &mut Context<Self>) {
        let Some(updater) = state(cx).updater.clone() else {
            return;
        };
        eprintln!(
            "music-gpui: updates from {} ({}, {})",
            updater.source.releases_url(),
            updater.kind.name(),
            updater.current
        );
        cx.background_executor()
            .spawn(async move { updater.cleanup() })
            .detach();
        if self.update_prefs(cx).auto_check {
            self.check_for_updates(false, cx);
        }
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(RECHECK_EVERY).await;
                let alive = this
                    .update(cx, |this, cx| {
                        if this.update_prefs(cx).auto_check {
                            this.check_for_updates(false, cx);
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    /// Fetches the releases (no-op without a repo). `manual`: "Check now" / the
    /// menu item — also reports "up to date".
    pub fn check_for_updates(&mut self, manual: bool, cx: &mut Context<Self>) {
        let Some(updater) = state(cx).updater.clone() else {
            return;
        };
        let status = status(cx);
        // A download in progress or a verified one waiting for the restart stays.
        if status.busy() || (!manual && matches!(status, UpdateStatus::Ready { .. })) {
            return;
        }
        let include_pre = self.include_prereleases(cx);
        let ready = matches!(status, UpdateStatus::Ready { .. });
        self.set_update_status(UpdateStatus::Checking, cx);
        cx.spawn(async move |this, cx| {
            let result = off_thread(move || {
                let offer = updater.check(include_pre);
                (offer, updater.blocker())
            })
            .await;
            this.update(cx, |this, cx| this.on_checked(result, manual, ready, cx))
                .ok();
        })
        .detach();
    }

    fn on_checked(
        &mut self,
        (result, blocker): (Result<Option<Offer>, UpdateError>, Option<UpdateError>),
        manual: bool,
        was_ready: bool,
        cx: &mut Context<Self>,
    ) {
        let repo = repo().unwrap_or_default();
        match result {
            Ok(Some(offer)) => {
                let version = offer.version.to_string();
                let blocker =
                    blocker.or_else(|| offer.asset.is_none().then_some(UpdateError::NoAsset));
                let installable = blocker.is_none();
                let same_as_staged = state(cx)
                    .staged
                    .as_ref()
                    .is_some_and(|s| s.version == offer.version);
                let url = if offer.page_url.is_empty() {
                    releases_page(&repo)
                } else {
                    offer.page_url.clone()
                };
                {
                    let s = state(cx);
                    s.offer = Some(offer);
                    s.blocker = blocker;
                }
                if was_ready && same_as_staged {
                    self.set_update_status(UpdateStatus::Ready { version }, cx);
                    return;
                }
                self.set_update_status(
                    UpdateStatus::Available {
                        version: version.clone(),
                        url,
                        installable,
                    },
                    cx,
                );
                if installable && self.update_prefs(cx).auto_install {
                    self.start_update_download(cx);
                } else if !manual {
                    let action: (&'static str, AppAction) = if installable {
                        (t!("update.update_now"), |app, cx| {
                            app.start_update_download(cx)
                        })
                    } else {
                        (t!("update.download_manually"), |app, cx| {
                            app.open_update_page(cx)
                        })
                    };
                    self.update_notification(
                        t!("update.available_toast", version = version),
                        Some(action),
                        cx,
                    );
                }
            }
            Ok(None) => {
                state(cx).blocker = blocker;
                self.set_update_status(UpdateStatus::UpToDate, cx);
                if manual {
                    self.update_notification(t!("update.up_to_date_toast").to_owned(), None, cx);
                }
            }
            Err(e) => {
                eprintln!("music-gpui: update check failed: {e}");
                self.set_update_status(
                    UpdateStatus::Failed {
                        message: t!("update.check_failed", error = describe(&e)),
                        url: Some(releases_page(&repo)),
                        retry: Retry::Check,
                    },
                    cx,
                );
            }
        }
    }

    pub fn open_update_page(&mut self, cx: &mut Context<Self>) {
        if let Some(url) = download_page(cx) {
            cx.open_url(&url);
        }
    }

    /// "Update to X.Y.Z": downloads and verifies the offered package.
    pub fn start_update_download(&mut self, cx: &mut Context<Self>) {
        let (Some(updater), Some(offer)) = (state(cx).updater.clone(), state(cx).offer.clone())
        else {
            return;
        };
        if status(cx).busy() {
            return;
        }
        if let Some(blocker) = state(cx).blocker.clone() {
            eprintln!("music-gpui: can't update in place ({blocker}); opening the release page");
            self.open_update_page(cx);
            return;
        }
        let version = offer.version.to_string();
        let size = offer.asset.as_ref().map(|a| a.size).filter(|s| *s > 0);
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(Progress::default());
        progress
            .total
            .store(size.unwrap_or(u64::MAX), Ordering::Relaxed);
        {
            let s = state(cx);
            s.cancel = Some(cancel.clone());
            s.progress = progress.clone();
            s.staged = None;
        }
        eprintln!("music-gpui: downloading update {version}");
        // The "available" notification's button has done its job.
        if let Some(window) = self.main_window {
            cx.defer(move |cx| {
                window
                    .update(cx, |_, window, cx| {
                        window.remove_notification::<UpdateNotification>(cx)
                    })
                    .ok();
            });
        }
        self.set_update_status(
            UpdateStatus::Downloading {
                version: version.clone(),
                done: 0,
                total: size,
            },
            cx,
        );
        // Progress for the UI, five times a second while downloading.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
                let more = this
                    .update(cx, |this, cx| {
                        let s = state(cx);
                        let UpdateStatus::Downloading { version, .. } = &s.status else {
                            return false;
                        };
                        let total = s.progress.total.load(Ordering::Relaxed);
                        let next = UpdateStatus::Downloading {
                            version: version.clone(),
                            done: s.progress.done.load(Ordering::Relaxed),
                            total: (total != u64::MAX).then_some(total),
                        };
                        if s.status != next {
                            this.set_update_status(next, cx);
                        }
                        true
                    })
                    .unwrap_or(false);
                if !more {
                    break;
                }
            }
        })
        .detach();
        let worker_cancel = cancel.clone();
        cx.spawn(async move |this, cx| {
            let result = off_thread(move || {
                updater.download(&offer, &worker_cancel, &mut |done, total| {
                    progress.done.store(done, Ordering::Relaxed);
                    if let Some(total) = total {
                        progress.total.store(total, Ordering::Relaxed);
                    }
                })
            })
            .await;
            this.update(cx, |this, cx| {
                // A cancelled download that finished anyway is ignored.
                if !cancel.load(Ordering::Relaxed) {
                    this.on_downloaded(result, version, cx)
                }
            })
            .ok();
        })
        .detach();
    }

    fn on_downloaded(
        &mut self,
        result: Result<Staged, UpdateError>,
        version: String,
        cx: &mut Context<Self>,
    ) {
        state(cx).cancel = None;
        match result {
            Ok(staged) => {
                eprintln!(
                    "music-gpui: update {version} downloaded and verified: {}",
                    staged.file.display()
                );
                state(cx).staged = Some(staged);
                self.set_update_status(
                    UpdateStatus::Ready {
                        version: version.clone(),
                    },
                    cx,
                );
                self.update_notification(
                    t!("update.ready_toast", version = version),
                    Some((t!("update.restart"), |app, cx| {
                        app.install_update_and_restart(cx)
                    })),
                    cx,
                );
            }
            Err(e) => {
                eprintln!("music-gpui: update download failed: {e}");
                let message = describe(&e);
                self.set_update_status(
                    UpdateStatus::Failed {
                        message: message.clone(),
                        url: download_page(cx),
                        retry: Retry::Download,
                    },
                    cx,
                );
                self.update_notification(
                    t!("update.failed_toast", error = message),
                    Some((t!("update.download_manually"), |app, cx| {
                        app.open_update_page(cx)
                    })),
                    cx,
                );
            }
        }
    }

    pub fn cancel_update_download(&mut self, cx: &mut Context<Self>) {
        let Some(cancel) = state(cx).cancel.take() else {
            return;
        };
        cancel.store(true, Ordering::Relaxed);
        eprintln!("music-gpui: update download cancelled");
        let status = match state(cx).offer.clone() {
            Some(offer) => UpdateStatus::Available {
                version: offer.version.to_string(),
                url: download_page(cx).unwrap_or_default(),
                installable: true,
            },
            None => UpdateStatus::Unknown,
        };
        self.set_update_status(status, cx);
    }

    /// "Restart to finish": installs the verified download, then quits (the new
    /// version starts once this process has exited).
    pub fn install_update_and_restart(&mut self, cx: &mut Context<Self>) {
        let Some(updater) = state(cx).updater.clone() else {
            return;
        };
        let Some(staged) = state(cx).staged.take() else {
            return;
        };
        let version = staged.version.to_string();
        self.set_update_status(
            UpdateStatus::Installing {
                version: version.clone(),
            },
            cx,
        );
        cx.spawn(async move |this, cx| {
            let keep = staged.clone();
            let result = off_thread(move || updater.install(&staged, true)).await;
            this.update(cx, |this, cx| match result {
                Ok(()) => {
                    eprintln!("music-gpui: update {version} installed; restarting");
                    cx.quit();
                }
                Err(e) => {
                    eprintln!("music-gpui: update install failed: {e}");
                    state(cx).staged = Some(keep);
                    this.set_update_status(
                        UpdateStatus::Failed {
                            message: describe(&e),
                            url: download_page(cx),
                            retry: Retry::Install,
                        },
                        cx,
                    );
                }
            })
            .ok();
        })
        .detach();
    }

    /// "Try again" after a failure.
    pub fn retry_update(&mut self, cx: &mut Context<Self>) {
        let UpdateStatus::Failed { retry, .. } = status(cx) else {
            return;
        };
        match retry {
            Retry::Check => self.check_for_updates(true, cx),
            Retry::Download => {
                state(cx).status = UpdateStatus::Unknown;
                self.start_update_download(cx)
            }
            Retry::Install => self.install_update_and_restart(cx),
        }
    }

    /// On quit with "Install updates automatically": installs a verified download
    /// that is waiting for a restart (without starting the app again). Blocking: a
    /// rename on macOS / Linux, starting the installer or helper on Windows.
    pub fn install_update_on_quit(&mut self, cx: &mut Context<Self>) {
        if !self.update_prefs(cx).auto_install {
            return;
        }
        let s = state(cx);
        if !matches!(s.status, UpdateStatus::Ready { .. }) {
            return;
        }
        let (Some(updater), Some(staged)) = (s.updater.clone(), s.staged.take()) else {
            return;
        };
        match updater.install(&staged, false) {
            Ok(()) => eprintln!("music-gpui: update {} installed on quit", staged.version),
            Err(e) => eprintln!("music-gpui: installing the update on quit failed: {e}"),
        }
    }
}
