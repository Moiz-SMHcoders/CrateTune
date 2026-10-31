//! About section: version, updates, license and attribution notices.

use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    progress::Progress,
    switch::Switch,
    v_flex,
};

use super::SettingsPanel;
use super::prefs::UpdatePrefs;
use super::widgets::{Anchor as _, group_at, note, row_at, status_line};
use crate::app::update_app;
use crate::update_check::{self, UpdateStatus};

const GPL_URL: &str = "https://www.gnu.org/licenses/gpl-3.0.html";
const ARCHIVETUNE_URL: &str = "https://github.com/rukamori/ArchiveTune";
const HUMMINGBIRD_URL: &str = "https://github.com/hummingbird-player/hummingbird";

pub(super) fn render(
    panel: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let welcome = h_flex().gap_2().anchor("about.welcome").child(
        Button::new("about-onboarding")
            .small()
            .outline()
            .label(t!("settings.about.show_welcome"))
            .on_click(|_, window, cx| {
                crate::app::update_app(cx, |app, cx| app.open_onboarding(0, window, cx));
            }),
    );
    let updates = crate::update_check::repo().map(|repo| updates_group(panel, &repo, cx));
    // English: the description from packaging/app.env (the one place to rename).
    let summary = if crate::i18n::locale() == crate::i18n::Locale::ENGLISH {
        crate::identity::APP_SUMMARY
    } else {
        t!("settings.about.summary")
    };
    let muted = cx.theme().muted_foreground;
    let link = |id: &'static str, label: &'static str, url: &'static str| {
        Button::new(id)
            .xsmall()
            .ghost()
            .label(label)
            .on_click(move |_, _, cx| cx.open_url(url))
    };
    let credit = |name: &'static str, text: SharedString| {
        v_flex()
            .gap_0p5()
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(name))
            .child(div().text_xs().text_color(muted).child(text))
    };
    v_flex()
        .gap_5()
        .child(
            h_flex()
                .gap_4()
                .items_center()
                .child(crate::logo::logo(64.))
                .child(
                    v_flex()
                        .gap_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_xl()
                                .font_weight(FontWeight::BOLD)
                                .child(crate::identity::APP_NAME),
                        )
                        .child(note(
                            SharedString::from(t!(
                                "settings.about.version_summary",
                                version = env!("CARGO_PKG_VERSION"),
                                summary = summary
                            )),
                            cx,
                        ))
                        .child(note(t!("settings.about.not_affiliated"), cx))
                        .child(welcome),
                ),
        )
        .children(updates)
        .child(logs_group(panel, cx))
        .child(
            group_at("about.license", t!("settings.about.license"), cx)
                .child(note(t!("settings.about.license_notice"), cx))
                .child(h_flex().child(link("about-gpl", "GNU GPL v3.0", GPL_URL))),
        )
        .child(
            group_at("about.credits", t!("settings.about.credits"), cx)
                .child(credit(
                    "ArchiveTune",
                    t!("settings.about.credit.archivetune", project = "ArchiveTune").into(),
                ))
                .child(credit(
                    "Hummingbird",
                    t!("settings.about.credit.hummingbird").into(),
                ))
                .child(credit(
                    t!("settings.about.credit.recognition_title"),
                    t!("settings.about.credit.recognition", project = "ArchiveTune").into(),
                ))
                .child(credit(
                    t!("settings.about.credit.libraries_title"),
                    t!("settings.about.credit.libraries").into(),
                ))
                .child(
                    h_flex()
                        .gap_1()
                        .child(link("about-archivetune", "ArchiveTune", ARCHIVETUNE_URL))
                        .child(link("about-hummingbird", "Hummingbird", HUMMINGBIRD_URL)),
                ),
        )
        .into_any_element()
}

/// Lines the viewer draws (the newest; Copy / Save take everything in memory).
const LOG_LINES_SHOWN: usize = 300;

/// About → Logs: the recent log lines, live, by level; copy, save, open the folder.
fn logs_group(panel: &mut SettingsPanel, cx: &mut Context<SettingsPanel>) -> impl IntoElement {
    use crate::logs::{self, Level};
    let theme = cx.theme();
    let (muted, danger, warning, border, bg) = (
        theme.muted_foreground,
        theme.danger,
        theme.warning,
        theme.border,
        theme.background,
    );
    let mono = theme.mono_font_family.clone();
    let current = panel.log_level;
    panel.log_seen = logs::generation();
    let lines = logs::lines(current, LOG_LINES_SHOWN);
    let filter = |level: Level, label: &'static str| {
        Button::new(SharedString::from(format!("about-logs-{level:?}")))
            .xsmall()
            .map(|b| {
                if level == current {
                    b.primary()
                } else {
                    b.ghost()
                }
            })
            .label(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.log_level = level;
                cx.notify();
            }))
    };
    let filters = h_flex()
        .gap_1()
        .flex_wrap()
        .child(filter(Level::Error, t!("settings.about.logs.errors")))
        .child(filter(Level::Warn, t!("settings.about.logs.warnings")))
        .child(filter(Level::Info, t!("settings.about.logs.info")))
        .child(filter(Level::Trace, t!("settings.about.logs.trace")));
    let rows = lines.iter().rev().map(|line| {
        let color = match line.level {
            Level::Error => danger,
            Level::Warn => warning,
            Level::Info | Level::Trace => muted,
        };
        h_flex()
            .gap_2()
            .items_start()
            .child(
                div()
                    .flex_none()
                    .text_color(muted)
                    .child(logs::clock(line.at_ms)),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(44.))
                    .text_color(color)
                    .child(line.level.tag()),
            )
            .child(div().flex_1().min_w_0().child(line.text.clone()))
    });
    let empty = lines.is_empty();
    let view = div()
        .id("about-logs-lines")
        .h(px(260.))
        .w_full()
        .overflow_y_scroll()
        .p_2()
        .rounded(crate::theme::radius(6.))
        .border_1()
        .border_color(border)
        .bg(bg)
        .font_family(mono)
        .text_xs()
        .when(empty, |this| {
            this.child(
                div()
                    .text_color(muted)
                    .child(t!("settings.about.logs.empty")),
            )
        })
        .children(rows);
    let folder = logs::folder();
    let file_on = panel.settings.logs.file;
    let file_switch = Switch::new("about-logs-file")
        .checked(file_on)
        .on_click(cx.listener(|this, checked: &bool, _, cx| this.set_log_file(*checked, cx)));
    let actions = h_flex()
        .gap_1()
        .flex_wrap()
        .child(
            Button::new("about-logs-copy")
                .small()
                .outline()
                .label(t!("settings.about.logs.copy"))
                .on_click(move |_, window, cx| {
                    let text = logs::dump(current);
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                    crate::app::notify(window, cx, t!("settings.about.logs.copied"));
                }),
        )
        .child(
            Button::new("about-logs-save")
                .small()
                .outline()
                .label(t!("settings.about.logs.save"))
                .on_click(cx.listener(|this, _, window, cx| this.save_logs(window, cx))),
        )
        .child(
            Button::new("about-logs-folder")
                .small()
                .ghost()
                .label(t!("settings.about.logs.open_folder"))
                .disabled(folder.is_none())
                .on_click(move |_, _, cx| {
                    if let Some(dir) = &folder {
                        let _ = std::fs::create_dir_all(dir);
                        cx.open_with_system(dir);
                    }
                }),
        );
    group_at("about.logs", t!("settings.about.logs"), cx)
        .child(note(t!("settings.about.logs.note"), cx))
        .child(filters)
        .child(view)
        .child(actions)
        .child(row_at(
            "about.logs.file",
            t!("settings.about.logs.file"),
            Some(t!("settings.about.logs.file_desc")),
            file_switch,
            cx,
        ))
}

impl SettingsPanel {
    /// About → Logs → "Write a log file".
    pub fn set_log_file(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.settings.logs.file != on {
            self.settings.logs.file = on;
            crate::logs::set_file_enabled(on);
            self.changed(false, cx);
            cx.notify();
        }
    }

    /// "Save logs…": everything in memory (redacted) to a file of the user's choice.
    pub fn save_logs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = format!("{}-log.txt", crate::identity::APP_ID);
        let dir = dirs_home();
        let target = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(path))) = target.await else {
                return;
            };
            let text = crate::logs::dump(crate::logs::Level::Trace);
            let result = std::fs::write(&path, text);
            cx.update(|window, cx| {
                let message = match result {
                    Ok(()) => t!("settings.about.logs.saved", path = path.display()),
                    Err(e) => t!("settings.about.logs.save_failed", error = e),
                };
                crate::app::notify(window, cx, message);
            })
            .ok();
        })
        .detach();
    }

    /// Changes the Updates switches (saved like every other setting).
    pub fn update_update_prefs(
        &mut self,
        f: impl FnOnce(&mut UpdatePrefs),
        cx: &mut Context<Self>,
    ) {
        let before = self.settings.updates;
        f(&mut self.settings.updates);
        if self.settings.updates != before {
            self.changed(false, cx);
        }
    }
}

/// Settings → About → Updates: status, the actions for it, the three switches.
fn updates_group(
    panel: &mut SettingsPanel,
    repo: &str,
    cx: &mut Context<SettingsPanel>,
) -> gpui::Div {
    let status = update_check::status(cx);
    let theme = cx.theme();
    let (danger, muted) = (theme.danger, theme.muted_foreground);
    let status_text: SharedString = match &status {
        UpdateStatus::Unknown => t!("update.releases_from", repo = repo).into(),
        UpdateStatus::Checking => t!("update.checking").into(),
        UpdateStatus::UpToDate => t!(
            "update.up_to_date_version",
            version = update_check::current_version()
        )
        .into(),
        UpdateStatus::Available { version, .. } => {
            t!("update.available_version", version = version).into()
        }
        UpdateStatus::Downloading {
            version,
            done,
            total,
        } => t!(
            "update.downloading",
            version = version,
            progress = update_check::progress_text(*done, *total)
        )
        .into(),
        UpdateStatus::Ready { version } => t!("update.ready", version = version).into(),
        UpdateStatus::Installing { version } => t!("update.installing", version = version).into(),
        UpdateStatus::Failed { message, .. } => message.clone().into(),
    };
    let failed = matches!(status, UpdateStatus::Failed { .. });
    let busy = matches!(
        status,
        UpdateStatus::Checking | UpdateStatus::Downloading { .. } | UpdateStatus::Installing { .. }
    );
    let manual_link = |id: &'static str, primary: bool| {
        let button = Button::new(id)
            .small()
            .label(t!("update.download_manually"))
            .on_click(|_, _, cx| {
                update_app(cx, |app, cx| app.open_update_page(cx));
            });
        if primary {
            button.primary()
        } else {
            button.outline()
        }
    };
    // The step's main action (primary), then "Check now".
    let action = match &status {
        UpdateStatus::Available {
            version,
            installable: true,
            ..
        } => Some(
            Button::new("about-update-install")
                .small()
                .primary()
                .label(t!("update.update_to", version = version))
                .on_click(|_, _, cx| {
                    update_app(cx, |app, cx| app.start_update_download(cx));
                })
                .into_any_element(),
        ),
        UpdateStatus::Available {
            installable: false, ..
        } => Some(manual_link("about-update-open", true).into_any_element()),
        UpdateStatus::Downloading { .. } => Some(
            Button::new("about-update-cancel")
                .small()
                .outline()
                .label(t!("common.cancel"))
                .on_click(|_, _, cx| {
                    update_app(cx, |app, cx| app.cancel_update_download(cx));
                })
                .into_any_element(),
        ),
        UpdateStatus::Ready { .. } => Some(
            Button::new("about-update-restart")
                .small()
                .primary()
                .label(t!("update.restart"))
                .on_click(|_, _, cx| {
                    update_app(cx, |app, cx| app.install_update_and_restart(cx));
                })
                .into_any_element(),
        ),
        UpdateStatus::Failed { .. } => Some(
            Button::new("about-update-retry")
                .small()
                .primary()
                .label(t!("update.retry"))
                .on_click(|_, _, cx| {
                    update_app(cx, |app, cx| app.retry_update(cx));
                })
                .into_any_element(),
        ),
        _ => None,
    };
    let has_action = action.is_some();
    let check = Button::new("about-update-check")
        .small()
        .label(t!("update.check_now"))
        .disabled(busy)
        .on_click(|_, _, cx| {
            update_app(cx, |app, cx| app.check_for_updates(true, cx));
        });
    let check = if has_action {
        check.outline()
    } else {
        check.primary()
    };
    let progress = match &status {
        UpdateStatus::Downloading { done, total, .. } => {
            Some(div().w_full().max_w(px(360.)).child(
                Progress::new("about-update-progress").value(match total {
                    Some(total) if *total > 0 => *done as f32 * 100. / *total as f32,
                    _ => 0.,
                }),
            ))
        }
        _ => None,
    };
    let blocker = matches!(
        status,
        UpdateStatus::Available {
            installable: false,
            ..
        }
    )
    .then(|| update_check::blocker_text(cx))
    .flatten();
    let prefs = panel.settings.updates;
    let prereleases = update_check::include_prereleases(&prefs);
    let auto_check = Switch::new("about-update-auto-check")
        .checked(prefs.auto_check)
        .on_click(cx.listener(|this, on: &bool, _, cx| {
            let on = *on;
            this.update_update_prefs(|u| u.auto_check = on, cx);
        }));
    let auto_install = Switch::new("about-update-auto-install")
        .checked(prefs.auto_install)
        .on_click(cx.listener(|this, on: &bool, _, cx| {
            let on = *on;
            this.update_update_prefs(|u| u.auto_install = on, cx);
        }));
    let include_pre = Switch::new("about-update-prereleases")
        .checked(prereleases)
        .on_click(cx.listener(|this, on: &bool, _, cx| {
            let on = *on;
            this.update_update_prefs(|u| u.include_prereleases = Some(on), cx);
        }));
    group_at("about.updates", t!("update.group"), cx)
        .child(if failed {
            status_line(status_text, danger).into_any_element()
        } else {
            div().text_sm().child(status_text).into_any_element()
        })
        .children(progress)
        .children(blocker.map(|text| div().text_xs().text_color(muted).child(text)))
        .child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .anchor("about.updates.check")
                .children(action)
                .child(check)
                .children(failed.then(|| manual_link("about-update-manual", false))),
        )
        .child(row_at(
            "about.updates.auto_check",
            t!("update.auto_check"),
            Some(t!("update.auto_check_detail")),
            auto_check,
            cx,
        ))
        .child(row_at(
            "about.updates.auto_install",
            t!("update.auto_install"),
            Some(t!("update.auto_install_detail")),
            auto_install,
            cx,
        ))
        .child(row_at(
            "about.updates.prereleases",
            t!("update.prereleases"),
            Some(t!("update.prereleases_detail")),
            include_pre,
            cx,
        ))
}

/// Where "Save logs…" starts: the home folder.
fn dirs_home() -> std::path::PathBuf {
    directories::UserDirs::new()
        .map(|d| d.home_dir().to_owned())
        .unwrap_or_else(std::env::temp_dir)
}
