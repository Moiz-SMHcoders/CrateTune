//! Script actions for updates (see `script.rs` for the runner, `update_check.rs`
//! for the flow):
//!
//! | action | does |
//! |---|---|
//! | `@update-check` | check for a new release now, as "Check now" (needs `UPDATE_REPO`) |
//! | `@update-menu` | the "Check for Updates…" menu item (opens Settings → About and checks) |
//! | `@update-status` | print the status, install kind and path, offer, staged file, log path |
//! | `@update-download` | "Update to X.Y.Z": download + verify the offered package |
//! | `@update-cancel` | cancel the download |
//! | `@update-install[=norestart]` | "Restart to finish": install the verified download and restart (`norestart`: install and quit, as on quit with "Install automatically") |
//! | `@update-retry` | "Try again" after a failure |
//! | `@update-pref=auto-check:on\|off`, `auto-install:on\|off`, `prereleases:on\|off\|auto` | the Updates switches (saved) |

use gpui::{Context, Window};

use crate::app::MusicApp;
use crate::update_check::{self, UpdateStatus};

impl MusicApp {
    /// The "Check for Updates…" menu item: Settings → About, then a manual check.
    pub fn check_for_updates_from_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(Some(crate::settings::SectionId::About), window, cx);
        self.check_for_updates(true, cx);
    }

    /// Returns whether `name` was one of these actions.
    pub fn update_script_action(
        &mut self,
        name: &str,
        arg: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match name {
            "update-check" => self.check_for_updates(true, cx),
            "update-menu" => window.dispatch_action(Box::new(crate::actions::CheckForUpdates), cx),
            "update-status" => {
                let status = match update_check::status(cx) {
                    UpdateStatus::Unknown => "unknown".to_owned(),
                    UpdateStatus::Checking => "checking".to_owned(),
                    UpdateStatus::UpToDate => {
                        format!("up-to-date {}", update_check::current_version())
                    }
                    UpdateStatus::Available {
                        version,
                        installable,
                        ..
                    } => format!("available {version} installable={installable}"),
                    UpdateStatus::Downloading {
                        version,
                        done,
                        total,
                    } => format!("downloading {version} {done}/{}", total.unwrap_or(0)),
                    UpdateStatus::Ready { version } => format!("ready {version}"),
                    UpdateStatus::Installing { version } => format!("installing {version}"),
                    UpdateStatus::Failed { message, retry, .. } => {
                        format!("failed ({retry:?}): {message}")
                    }
                };
                let prefs = self.settings.read(cx).settings().updates;
                println!(
                    "update-status: {status}; {}; auto_check={} auto_install={} prereleases={}",
                    update_check::diagnostics(cx),
                    prefs.auto_check,
                    prefs.auto_install,
                    self.include_prereleases(cx)
                );
            }
            "update-download" => self.start_update_download(cx),
            "update-cancel" => self.cancel_update_download(cx),
            "update-install" | "update-apply" => {
                if arg == Some("norestart") {
                    // As on quit with "Install updates automatically".
                    self.settings.update(cx, |p, cx| {
                        p.update_update_prefs(|u| u.auto_install = true, cx)
                    });
                    cx.quit();
                } else {
                    self.install_update_and_restart(cx);
                }
            }
            "update-retry" => self.retry_update(cx),
            "update-pref" => {
                let Some((key, value)) = arg.and_then(|a| a.split_once(':')) else {
                    eprintln!("music-gpui: @update-pref wants KEY:on|off");
                    return true;
                };
                let on = value == "on";
                self.settings.update(cx, |p, cx| {
                    p.update_update_prefs(
                        |u| match key {
                            "auto-check" => u.auto_check = on,
                            "auto-install" => u.auto_install = on,
                            "prereleases" => {
                                u.include_prereleases = (value != "auto").then_some(on)
                            }
                            other => eprintln!("music-gpui: unknown update pref {other}"),
                        },
                        cx,
                    )
                });
            }
            _ => return false,
        }
        true
    }
}
