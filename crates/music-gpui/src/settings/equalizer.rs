//! Equalizer section: on/off, presets, ten ±12 dB bands (applied live), then
//! the reverb (rooms + wet mix) and one-click sound presets (speed, pitch and
//! reverb together).

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
use music_core::equalizer::GRAPHIC_BANDS_HZ;
use music_core::{EqPreset, ReverbPreset};

use super::SettingsPanel;
use super::prefs::{SoundPreset, reverb_title};
use super::prefs_ui::slider_row_at;
use super::widgets::{Anchor as _, band_label, group_at, note, section_title};

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
                .label(crate::components::sound_menu::eq_title(preset))
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
                .anchor("eq.enabled")
                .child(section_title(t!("settings.section.equalizer")))
                .child(
                    Switch::new("eq-enabled")
                        .checked(eq.enabled)
                        .label(if eq.enabled {
                            t!("common.on")
                        } else {
                            t!("common.off")
                        })
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.set_eq_enabled(*checked, cx)
                        })),
                ),
        )
        .child(
            h_flex()
                .gap_2()
                .anchor("eq.presets")
                .child(presets)
                .when(current.is_none(), |this| {
                    this.child(Tag::secondary().small().child(t!("common.custom")))
                }),
        )
        .child(
            h_flex()
                .items_start()
                .gap_2()
                .anchor("eq.bands")
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
        .child(note(t!("settings.eq.note"), cx))
        .child(effects(panel, cx))
        .into_any_element()
}

/// Reverb and sound presets, under the bands.
fn effects(panel: &SettingsPanel, cx: &mut Context<SettingsPanel>) -> impl IntoElement {
    let a = &panel.settings.audio;
    let rooms = h_flex()
        .flex_wrap()
        .gap_1()
        .children(ReverbPreset::ALL.map(|preset| {
            let selected = a.reverb == preset;
            Button::new(SharedString::from(format!("reverb-{}", preset.key())))
                .small()
                .map(|b| if selected { b.primary() } else { b.outline() })
                .label(reverb_title(preset))
                .selected(selected)
                .on_click(cx.listener(move |this, _, _, cx| this.set_reverb(preset, cx)))
        }));
    let current = SoundPreset::current(a);
    let sounds = h_flex()
        .flex_wrap()
        .gap_1()
        .children(SoundPreset::ALL.map(|preset| {
            let selected = current == Some(preset);
            Button::new(SharedString::from(format!("sound-{}", preset.key())))
                .small()
                .map(|b| if selected { b.primary() } else { b.outline() })
                .label(preset.title())
                .selected(selected)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.apply_sound_preset(preset, window, cx)
                }))
        }));
    let mix = t!(
        "settings.reverb.mix_value",
        percent = (a.reverb_mix * 100.).round() as u32
    );
    v_flex()
        .pt_3()
        .gap_5()
        .child(
            group_at("eq.reverb", t!("settings.reverb.title"), cx)
                .child(div().anchor("eq.reverb.room").child(rooms))
                .child(
                    div()
                        .when(a.reverb == ReverbPreset::Off, |this| this.opacity(0.5))
                        .child(slider_row_at(
                            "eq.reverb.mix",
                            t!("settings.reverb.mix"),
                            mix,
                            &panel.prefs.reverb_mix,
                            cx,
                        )),
                )
                .child(note(t!("settings.reverb.note"), cx)),
        )
        .child(
            group_at("eq.sound_presets", t!("settings.sound.title"), cx)
                .child(
                    h_flex()
                        .gap_2()
                        .child(sounds)
                        .when(current.is_none(), |this| {
                            this.child(Tag::secondary().small().child(t!("common.custom")))
                        }),
                )
                .child(note(t!("settings.sound.note"), cx)),
        )
}
