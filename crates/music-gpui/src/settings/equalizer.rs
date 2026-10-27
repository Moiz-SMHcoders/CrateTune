//! Equalizer section: on/off, presets, ten ±12 dB bands (applied live).

use gpui::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    slider::Slider,
    switch::Switch,
    tag::Tag,
    v_flex,
};
use music_core::EqPreset;
use music_core::equalizer::GRAPHIC_BANDS_HZ;

use super::SettingsPanel;
use super::widgets::{band_label, note, section_title};

const SLIDER_HEIGHT: f32 = 150.;

pub(super) fn render(
    panel: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let theme = cx.theme();
    let eq = &panel.settings.equalizer;
    let current = eq.preset();
    let presets = h_flex()
        .flex_wrap()
        .gap_1()
        .children(EqPreset::ALL.map(|preset| {
            let selected = eq.enabled && current == Some(preset);
            Button::new(SharedString::from(format!("preset-{}", preset.name())))
                .small()
                .map(|b| if selected { b.primary() } else { b.outline() })
                .label(preset.name())
                .selected(selected)
                .on_click(
                    cx.listener(move |this, _, window, cx| this.apply_preset(preset, window, cx)),
                )
        }));
    let muted = theme.muted_foreground;
    let bands =
        h_flex()
            .w_full()
            .justify_between()
            .children(panel.eq_sliders.iter().enumerate().map(|(i, slider)| {
                let gain = eq.gains_db[i];
                v_flex()
                    .w(px(34.))
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(if gain.abs() < 0.05 {
                                muted
                            } else {
                                theme.foreground
                            })
                            .child(if gain.abs() < 0.05 {
                                "0".to_owned()
                            } else {
                                format!("{gain:+.1}")
                            }),
                    )
                    .child(
                        div().h(px(SLIDER_HEIGHT)).child(
                            Slider::new(slider)
                                .vertical()
                                .h_full()
                                .disabled(!eq.enabled),
                        ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(band_label(GRAPHIC_BANDS_HZ[i])),
                    )
            }));
    v_flex()
        .gap_3()
        .max_w(px(520.))
        .child(
            h_flex()
                .justify_between()
                .child(section_title("Equalizer"))
                .child(
                    Switch::new("eq-enabled")
                        .checked(eq.enabled)
                        .label(if eq.enabled { "On" } else { "Off" })
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.set_eq_enabled(*checked, cx)
                        })),
                ),
        )
        .child(
            h_flex()
                .gap_2()
                .child(presets)
                .when(current.is_none(), |this| {
                    this.child(Tag::secondary().small().child("Custom"))
                }),
        )
        .child(
            h_flex()
                .items_start()
                .gap_2()
                .child(
                    v_flex()
                        .h(px(SLIDER_HEIGHT))
                        .mt(px(24.))
                        .justify_between()
                        .text_xs()
                        .text_color(muted)
                        .child("+12")
                        .child("0")
                        .child("−12"),
                )
                .child(bands),
        )
        .child(note(
            "Applied live. Boosts are compensated so the output never clips.",
            cx,
        ))
        .into_any_element()
}
