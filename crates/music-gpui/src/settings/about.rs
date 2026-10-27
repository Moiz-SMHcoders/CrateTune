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
            group("Music", cx)
                .child(note(
                    SharedString::from(format!(
                        "Version {} · a native desktop client for YouTube Music",
                        env!("CARGO_PKG_VERSION")
                    )),
                    cx,
                ))
                .child(note(
                    "Not affiliated with, endorsed or sponsored by YouTube or Google.",
                    cx,
                )),
        )
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
