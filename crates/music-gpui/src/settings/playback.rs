//! Playback section: shuffle / repeat (also in the bar and queue panel), and a
//! placeholder for the player features being added to music-core.

use gpui::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, Window,
    prelude::FluentBuilder as _,
};
use gpui_component::{
    Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    switch::Switch,
    v_flex,
};

use super::SettingsPanel;
use super::widgets::{coming_soon, group, row};
use crate::queue::Repeat;

pub(super) fn render(
    panel: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let p = panel.settings.playback.clone();
    let shuffle = Switch::new("settings-shuffle")
        .checked(p.shuffle)
        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
            let repeat = this.settings.playback.repeat;
            this.choose_playback_modes(*checked, repeat, cx)
        }));
    let repeat = h_flex()
        .gap_1()
        .children([Repeat::Off, Repeat::All, Repeat::One].map(|mode| {
            let selected = p.repeat == mode;
            Button::new(SharedString::from(format!("settings-repeat-{mode:?}")))
                .small()
                .map(|b| if selected { b.primary() } else { b.outline() })
                .selected(selected)
                .label(match mode {
                    Repeat::Off => "Off",
                    Repeat::All => "All",
                    Repeat::One => "One",
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    let shuffle = this.settings.playback.shuffle;
                    this.choose_playback_modes(shuffle, mode, cx)
                }))
        }));
    v_flex()
        .gap_5()
        .child(
            group("Queue", cx)
                .child(row(
                    "Shuffle",
                    Some("Keeps the current song first; turning it off restores the order."),
                    shuffle,
                    cx,
                ))
                .child(row(
                    "Repeat",
                    Some("Remembered between launches (⌘R cycles it)."),
                    repeat,
                    cx,
                )),
        )
        .child(coming_soon(
            "Waits for the playback engine features in music-core.",
            &[
                "Audio quality (low / normal / high) for streaming and downloads",
                "Gapless playback and crossfade",
                "Playback speed and pitch",
                "Skip silence",
                "Sleep timer (minutes or end of track)",
                "Output device",
                "Volume normalization (already on: YouTube loudness metadata)",
            ],
            cx,
        ))
        .into_any_element()
}
