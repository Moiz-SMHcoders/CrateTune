//! About section: version, updates, license and attribution notices.

use gpui::{
    AnyElement, Context, FontWeight, IntoElement, ParentElement as _, SharedString, Styled as _,
    Window, div, px,
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
    let welcome = h_flex()
        .gap_2()
        .anchor("about.welcome")
        .child(
            Button::new("about-onboarding")
                .small()
                .outline()
                .label(t!("settings.about.show_welcome"))
                .on_click(|_, window, cx| {
                    crate::app::update_app(cx, |app, cx| app.open_onboarding(0, window, cx));
                }),
        )
        .children(privacy_url().map(|url| {
            Button::new("about-privacy")
                .small()
                .ghost()
                .label(t!("settings.about.privacy"))
                .on_click(move |_, _, cx| cx.open_url(&url))
        }));
    let updates =
        crate::update_check::repo().map(|repo| match crate::update_check::package_manager() {
            Some(manager) => managed_group(manager, cx),
            None => updates_group(panel, &repo, cx),
        });
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

/// PRIVACY.md in the release repository (what the app contacts and stores).
fn privacy_url() -> Option<String> {
    let repo = crate::identity::UPDATE_REPO;
    repo.contains('/')
        .then(|| format!("https://github.com/{repo}/blob/main/PRIVACY.md"))
}

/// Settings → About → Updates for a copy a package manager updates: no checks,
/// no switches, just where updates come from.
fn managed_group(
    manager: &music_core::updater::PackageManager,
    cx: &mut Context<SettingsPanel>,
) -> gpui::Div {
    let name = manager.display_name();
    group_at("about.updates", t!("update.group"), cx)
        .child(
            div()
                .text_sm()
                .child(SharedString::from(t!("update.managed", manager = name))),
        )
        .child(note(t!("update.managed_detail", manager = name), cx))
}

impl SettingsPanel {
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
