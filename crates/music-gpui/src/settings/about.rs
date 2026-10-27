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
            .label("Show welcome guide")
            .on_click(|_, window, cx| {
                crate::app::update_app(cx, |app, cx| app.open_onboarding(0, window, cx));
            }),
    );
    let updates = crate::update_check::repo().map(|repo| {
        use crate::update_check::UpdateStatus;
        let status = crate::update_check::status(cx);
        let (text, url): (SharedString, Option<String>) = match &status {
            UpdateStatus::Unknown => (format!("Releases from github.com/{repo}").into(), None),
            UpdateStatus::Checking => ("Checking…".into(), None),
            UpdateStatus::UpToDate => (
                format!(
                    "You have the latest version ({}).",
                    env!("CARGO_PKG_VERSION")
                )
                .into(),
                None,
            ),
            UpdateStatus::Available { version, url } => (
                format!("Version {version} is available.").into(),
                Some(url.clone()),
            ),
            UpdateStatus::Failed(e) => (format!("Couldn't check: {e}").into(), None),
        };
        group("Updates", cx).child(note(text, cx)).child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("about-update-check")
                        .small()
                        .outline()
                        .label("Check now")
                        .on_click(|_, _, cx| {
                            crate::app::update_app(cx, |app, cx| app.check_for_updates(true, cx));
                        }),
                )
                .children(url.map(|url| {
                    Button::new("about-update-open")
                        .small()
                        .primary()
                        .label("Download…")
                        .on_click(move |_, _, cx| cx.open_url(&url))
                })),
        )
    });
    let muted = cx.theme().muted_foreground;
    let link = |id: &'static str, label: &'static str, url: &'static str| {
        Button::new(id)
            .xsmall()
            .ghost()
            .label(label)
            .on_click(move |_, _, cx| cx.open_url(url))
    };
    let credit = |name: &'static str, text: &'static str| {
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
                    SharedString::from(format!(
                        "Version {} · {}",
                        env!("CARGO_PKG_VERSION"),
                        crate::identity::APP_SUMMARY
                    )),
                    cx,
                ))
                .child(note(
                    "Not affiliated with, endorsed or sponsored by YouTube or Google.",
                    cx,
                ))
                .child(welcome),
        )
        .children(updates)
        .child(
            group("License", cx)
                .child(note(
                    "This program is free software: you can redistribute it and/or modify it \
                     under the terms of the GNU General Public License, version 3. It is \
                     distributed WITHOUT ANY WARRANTY; see the license for details.",
                    cx,
                ))
                .child(h_flex().child(link("about-gpl", "GNU GPL v3.0", GPL_URL))),
        )
        .child(
            group("Credits", cx)
                .child(credit(
                    "ArchiveTune",
                    "Features and screens are ported from ArchiveTune (GPL-3.0), \
                     © Rukamori and contributors. This is an independent project.",
                ))
                .child(credit(
                    "Hummingbird",
                    "Equalizer, OS media controls, scrobbling and Discord presence are \
                     ported from Hummingbird (Apache-2.0), © Hummingbird contributors.",
                ))
                .child(credit(
                    "Song recognition",
                    "Shazam-compatible fingerprinting ported from ArchiveTune's shazamkit \
                     (GPL-3.0), which follows SongRec's format. Not affiliated with Shazam or Apple.",
                ))
                .child(credit(
                    "Libraries",
                    "GPUI and gpui-component (Apache-2.0), Symphonia and rodio, \
                     Lucide icons (ISC), LRCLIB lyrics.",
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
