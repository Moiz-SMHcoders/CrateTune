//! About section: version, license and attribution notices.

use gpui::{
    AnyElement, Context, FontWeight, IntoElement, ParentElement as _, SharedString, Styled as _,
    Window, div,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

use super::SettingsPanel;
use super::widgets::{group, note};

const GPL_URL: &str = "https://www.gnu.org/licenses/gpl-3.0.html";
const ARCHIVETUNE_URL: &str = "https://github.com/rukamori/ArchiveTune";
const HUMMINGBIRD_URL: &str = "https://github.com/hummingbird-player/hummingbird";

pub(super) fn render(
    _: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let welcome = h_flex().gap_2().child(
        Button::new("about-onboarding")
            .small()
            .outline()
            .label(t!("settings.about.show_welcome"))
            .on_click(|_, window, cx| {
                crate::app::update_app(cx, |app, cx| app.open_onboarding(0, window, cx));
            }),
    );
    let updates = crate::update_check::repo().map(|repo| {
        use crate::update_check::UpdateStatus;
        let status = crate::update_check::status(cx);
        let (text, url): (SharedString, Option<String>) = match &status {
            UpdateStatus::Unknown => (t!("update.releases_from", repo = repo).into(), None),
            UpdateStatus::Checking => (t!("update.checking").into(), None),
            UpdateStatus::UpToDate => (
                t!(
                    "update.up_to_date_version",
                    version = env!("CARGO_PKG_VERSION")
                )
                .into(),
                None,
            ),
            UpdateStatus::Available { version, url } => (
                t!("update.available_version", version = version).into(),
                Some(url.clone()),
            ),
            UpdateStatus::Failed(e) => (t!("update.check_failed", error = e).into(), None),
        };
        group(t!("update.group"), cx).child(note(text, cx)).child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .child(
                    Button::new("about-update-check")
                        .small()
                        .outline()
                        .label(t!("update.check_now"))
                        .on_click(|_, _, cx| {
                            crate::app::update_app(cx, |app, cx| app.check_for_updates(true, cx));
                        }),
                )
                .children(url.map(|url| {
                    Button::new("about-update-open")
                        .small()
                        .primary()
                        .label(t!("update.download"))
                        .on_click(move |_, _, cx| cx.open_url(&url))
                })),
        )
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
            group(crate::identity::APP_NAME, cx)
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
        )
        .children(updates)
        .child(
            group(t!("settings.about.license"), cx)
                .child(note(t!("settings.about.license_notice"), cx))
                .child(h_flex().child(link("about-gpl", "GNU GPL v3.0", GPL_URL))),
        )
        .child(
            group(t!("settings.about.credits"), cx)
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
