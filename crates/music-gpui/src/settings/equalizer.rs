//! Equalizer section: on/off, presets (built-in and the user's named curves),
//! the response graph (`eq_graph.rs`) with draggable bands and a headroom
//! warning, ten ±12 dB bands with plain-language names (applied live), then
//! the reverb (rooms, convolution rooms, fine controls), night mode and the
//! sound presets (speed, pitch, reverb and EQ together). State and methods for
//! the newer parts are in `sound.rs`.

use std::rc::Rc;

use gpui::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    slider::{Slider, SliderState},
    switch::Switch,
    tag::Tag,
    v_flex,
};
use music_core::equalizer::GRAPHIC_BANDS_HZ;
use music_core::{EqPreset, ReverbPreset};

use super::SettingsPanel;
use super::eq_graph::{EqGraph, GraphColors};
use super::prefs::{SoundPreset, reverb_title};
use super::prefs_ui::slider_row_at;
use super::sound::{HEADROOM_WARN_DB, ImpulseState, band_name};
use super::widgets::{Anchor as _, band_label, group_at, note, section_title, status_line};
use crate::icons::Lucide;

const SLIDER_HEIGHT: f32 = 150.;

pub(super) fn render(
    panel: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let theme = cx.theme();
    let eq = &panel.settings.equalizer;
    let current = eq.preset();
    let saved_current = current.is_none().then(|| eq.saved_current()).flatten();
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
        }))
        .children(eq.saved.iter().enumerate().map(|(i, saved)| {
            let selected = eq.enabled && saved_current == Some(i);
            Button::new(SharedString::from(format!("eq-saved-{i}")))
                .small()
                .map(|b| if selected { b.primary() } else { b.outline() })
                .label(saved.name.clone())
                .selected(selected)
                .on_click(
                    cx.listener(move |this, _, window, cx| this.apply_saved_eq(i, window, cx)),
                )
        }));
    let muted = theme.muted_foreground;
    let bands = h_flex().w_full().items_start().justify_between().children(
        panel.eq_sliders.iter().enumerate().map(|(i, slider)| {
            let gain = eq.gains_db[i];
            v_flex()
                .w(px(54.))
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
                    v_flex()
                        .items_center()
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(band_label(GRAPHIC_BANDS_HZ[i])),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_center()
                                .line_height(px(13.))
                                .text_color(theme.foreground)
                                .child(band_name(i)),
                        ),
                )
        }),
    );
    let curve = panel.eq_curve();
    let peak = curve.peak_db;
    let warn = eq.enabled && !eq.pre_gain && peak > HEADROOM_WARN_DB;
    let colors = GraphColors {
        line: theme.primary,
        over: theme.danger,
        grid: theme.border,
        zero: theme.muted_foreground.opacity(0.6),
        point: theme.primary,
        point_ring: theme.background,
    };
    let weak = cx.entity().downgrade();
    let graph = EqGraph::new(
        "eq-graph",
        curve.points.clone(),
        eq.gains(),
        colors,
        panel.sound.graph_drag.clone(),
    )
    .enabled(eq.enabled)
    .warn(warn)
    .on_change(Rc::new(move |band, gain, _, window, cx| {
        weak.update(cx, |p, cx| p.set_band_from_graph(band, gain, window, cx))
            .ok();
    }));
    let headroom = if !eq.enabled || peak <= 0.05 {
        status_line(t!("sound.eq.no_boost").into(), muted)
    } else if eq.pre_gain {
        status_line(
            t!("sound.eq.pre_gain_on", db = format!("{peak:.1}")).into(),
            muted,
        )
    } else if warn {
        status_line(
            t!("sound.eq.clip_warning", db = format!("{peak:.1}")).into(),
            theme.danger,
        )
    } else {
        status_line(
            t!("sound.eq.small_boost", db = format!("{peak:.1}")).into(),
            muted,
        )
    };
    let saved_actions = eq_preset_actions(panel, saved_current, cx);
    v_flex()
        .gap_3()
        .max_w(px(600.))
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
                .when(current.is_none() && saved_current.is_none(), |this| {
                    this.child(Tag::secondary().small().child(t!("common.custom")))
                }),
        )
        .child(div().anchor("eq.saved").child(saved_actions))
        .child(
            div()
                .anchor("eq.graph")
                .rounded(px(8.))
                .border_1()
                .border_color(theme.border)
                .child(graph),
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
        .child(
            h_flex()
                .anchor("eq.headroom")
                .gap_3()
                .justify_between()
                .child(div().flex_1().min_w_0().child(headroom))
                .child(
                    Switch::new("eq-pre-gain")
                        .checked(eq.pre_gain)
                        .label(t!("sound.eq.pre_gain"))
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.set_eq_pre_gain(*checked, cx)
                        })),
                ),
        )
        .child(note(t!("sound.eq.note"), cx))
        .child(effects(panel, cx))
        .into_any_element()
}

/// Save / rename / delete for the user's EQ curves.
fn eq_preset_actions(
    panel: &SettingsPanel,
    selected: Option<usize>,
    cx: &Context<SettingsPanel>,
) -> gpui::Div {
    let name = selected
        .and_then(|i| panel.settings.equalizer.saved.get(i))
        .map(|p| p.name.clone());
    let action = |id: &'static str, icon: Lucide, label: &'static str| {
        Button::new(id).ghost().small().icon(icon).label(label)
    };
    h_flex()
        .flex_wrap()
        .gap_1()
        .child(
            action("eq-save", Lucide::Plus, t!("sound.preset.save")).on_click(cx.listener(
                move |this, _, window, cx| {
                    this.name_dialog(
                        t!("sound.eq.save_title"),
                        k!("sound.preset.name_placeholder"),
                        String::new(),
                        |p, name, _, cx| {
                            p.save_eq_preset(&name, cx);
                        },
                        window,
                        cx,
                    )
                },
            )),
        )
        .child(
            action("eq-rename", Lucide::Pencil, t!("common.rename"))
                .disabled(selected.is_none())
                .on_click(cx.listener({
                    let name = name.clone().unwrap_or_default();
                    move |this, _, window, cx| {
                        let Some(i) = selected else { return };
                        this.name_dialog(
                            t!("sound.preset.rename_title"),
                            k!("sound.preset.name_placeholder"),
                            name.clone(),
                            move |p, name, _, cx| p.rename_saved_eq(i, &name, cx),
                            window,
                            cx,
                        )
                    }
                })),
        )
        .child(
            action("eq-delete", Lucide::Trash, t!("common.delete"))
                .disabled(selected.is_none())
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(i) = selected {
                        this.delete_saved_eq(i, cx);
                    }
                })),
        )
}

/// A fine-control row with a scroll anchor (an `anchor` of the search index); `off`
/// greys it out and stops input.
fn fine_row_at(
    id: &'static str,
    label: &'static str,
    value: String,
    slider: &gpui::Entity<SliderState>,
    off: bool,
    cx: &gpui::App,
) -> gpui::Div {
    h_flex()
        .anchor(id)
        .gap_3()
        .when(off, |this| this.opacity(0.5))
        .child(div().w(px(150.)).flex_none().text_sm().child(label))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(Slider::new(slider).horizontal().disabled(off)),
        )
        .child(
            div()
                .w(px(76.))
                .flex_none()
                .text_sm()
                .text_right()
                .text_color(cx.theme().muted_foreground)
                .child(value),
        )
}

/// Reverb, night mode and sound presets, under the bands.
fn effects(panel: &SettingsPanel, cx: &mut Context<SettingsPanel>) -> impl IntoElement {
    let a = &panel.settings.audio;
    let room_button = |preset: ReverbPreset, label: SharedString| {
        let selected = a.reverb == preset;
        Button::new(SharedString::from(format!("reverb-{}", preset.key())))
            .small()
            .map(|b| if selected { b.primary() } else { b.outline() })
            .label(label)
            .selected(selected)
            .on_click(cx.listener(move |this, _, window, cx| this.set_reverb(preset, window, cx)))
    };
    let rooms = h_flex()
        .flex_wrap()
        .gap_1()
        .children(
            [ReverbPreset::Off]
                .into_iter()
                .chain(ReverbPreset::ROOMS)
                .map(|p| room_button(p, reverb_title(p).into())),
        )
        .when(a.reverb == ReverbPreset::Custom, |this| {
            this.child(Tag::secondary().small().child(t!("common.custom")))
        });
    let own_label: SharedString = match panel.impulse_name() {
        Some(name) => t!("sound.room.own_named", name = name).into(),
        None => reverb_title(ReverbPreset::OwnImpulse).into(),
    };
    let loading = panel.sound.impulse_state == ImpulseState::Loading;
    let convolution = h_flex()
        .flex_wrap()
        .gap_1()
        .children(ReverbPreset::IMPULSE_ROOMS.map(|p| room_button(p, reverb_title(p).into())))
        .when(panel.impulse_name().is_some(), |this| {
            this.child(room_button(ReverbPreset::OwnImpulse, own_label))
        })
        .child(
            Button::new("reverb-load-ir")
                .ghost()
                .small()
                .icon(Lucide::Upload)
                .label(t!("sound.ir.load"))
                .loading(loading)
                .on_click(cx.listener(|this, _, window, cx| this.pick_impulse(window, cx))),
        );
    let impulse_error = match &panel.sound.impulse_state {
        ImpulseState::Failed(message) => {
            Some(status_line(message.clone().into(), cx.theme().danger))
        }
        _ => None,
    };
    let p = a.reverb_params();
    let off = a.reverb == ReverbPreset::Off;
    let conv = a.reverb.is_convolution();
    let mix = t!(
        "settings.reverb.mix_value",
        percent = (a.reverb_mix * 100.).round() as u32
    );
    let pct = |v: f32| {
        t!(
            "settings.reverb.mix_value",
            percent = (v * 100.).round() as u32
        )
    };
    let s = &panel.sound;
    let fine = v_flex()
        .gap_2()
        .child(fine_row_at(
            "eq.reverb.length",
            t!("sound.reverb.length"),
            t!(
                "sound.value.seconds",
                value = format!("{:.1}", p.decay_secs)
            ),
            &s.decay,
            conv,
            cx,
        ))
        .child(fine_row_at(
            "eq.reverb.pre_delay",
            t!("sound.reverb.pre_delay"),
            t!("sound.value.ms", value = format!("{:.0}", p.pre_delay_ms)),
            &s.pre_delay,
            false,
            cx,
        ))
        .child(fine_row_at(
            "eq.reverb.brightness",
            t!("sound.reverb.brightness"),
            pct(1. - p.damping),
            &s.brightness,
            conv,
            cx,
        ))
        .child(fine_row_at(
            "eq.reverb.early",
            t!("sound.reverb.early"),
            pct(p.early),
            &s.early,
            conv,
            cx,
        ))
        .child(fine_row_at(
            "eq.reverb.width",
            t!("sound.reverb.width"),
            pct(p.width),
            &s.width,
            false,
            cx,
        ))
        .child(fine_row_at(
            "eq.reverb.dry",
            t!("sound.reverb.dry"),
            pct(p.dry),
            &s.dry,
            false,
            cx,
        ));
    let c = a.compressor;
    let night = v_flex()
        .gap_2()
        .child(fine_row_at(
            "eq.night.threshold",
            t!("sound.night.threshold"),
            t!("sound.value.db", value = format!("{:.0}", c.threshold_db)),
            &s.threshold,
            !c.enabled,
            cx,
        ))
        .child(fine_row_at(
            "eq.night.ratio",
            t!("sound.night.ratio"),
            t!("sound.value.ratio", value = format!("{:.1}", c.ratio)),
            &s.ratio,
            !c.enabled,
            cx,
        ))
        .child(fine_row_at(
            "eq.night.attack",
            t!("sound.night.attack"),
            t!("sound.value.ms", value = format!("{:.1}", c.attack_ms)),
            &s.attack,
            !c.enabled,
            cx,
        ))
        .child(fine_row_at(
            "eq.night.release",
            t!("sound.night.release"),
            t!("sound.value.ms", value = format!("{:.0}", c.release_ms)),
            &s.release,
            !c.enabled,
            cx,
        ))
        .child(fine_row_at(
            "eq.night.makeup",
            t!("sound.night.makeup"),
            t!("sound.value.db", value = format!("{:+.1}", c.makeup_db)),
            &s.makeup,
            !c.enabled,
            cx,
        ));
    let current = SoundPreset::current(a);
    let user_current = panel.user_sound_current();
    let sounds = h_flex()
        .flex_wrap()
        .gap_1()
        .children(SoundPreset::ALL.map(|preset| {
            let selected = current == Some(preset) && user_current.is_none();
            Button::new(SharedString::from(format!("sound-{}", preset.key())))
                .small()
                .map(|b| if selected { b.primary() } else { b.outline() })
                .label(preset.title())
                .selected(selected)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.apply_sound_preset(preset, window, cx)
                }))
        }))
        .children(a.sound_presets.iter().enumerate().map(|(i, preset)| {
            let selected = user_current == Some(i);
            Button::new(SharedString::from(format!("sound-user-{i}")))
                .small()
                .map(|b| if selected { b.primary() } else { b.outline() })
                .label(preset.name.clone())
                .selected(selected)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.apply_user_sound_preset(i, window, cx)
                }))
        }));
    let user_name = user_current
        .and_then(|i| a.sound_presets.get(i))
        .map(|p| p.name.clone())
        .unwrap_or_default();
    let action = |id: &'static str, icon: Lucide, label: &'static str| {
        Button::new(id).ghost().small().icon(icon).label(label)
    };
    let sound_actions = h_flex()
        .flex_wrap()
        .gap_1()
        .child(
            action("sound-save", Lucide::Plus, t!("sound.preset.save")).on_click(cx.listener(
                |this, _, window, cx| {
                    this.name_dialog(
                        t!("sound.preset.save_title"),
                        k!("sound.preset.name_placeholder"),
                        String::new(),
                        |p, name, _, cx| {
                            p.save_sound_preset(&name, cx);
                        },
                        window,
                        cx,
                    )
                },
            )),
        )
        .child(
            action("sound-rename", Lucide::Pencil, t!("common.rename"))
                .disabled(user_current.is_none())
                .on_click(cx.listener(move |this, _, window, cx| {
                    let Some(i) = user_current else { return };
                    this.name_dialog(
                        t!("sound.preset.rename_title"),
                        k!("sound.preset.name_placeholder"),
                        user_name.clone(),
                        move |p, name, _, cx| p.rename_user_sound_preset(i, &name, cx),
                        window,
                        cx,
                    )
                })),
        )
        .child(
            action("sound-delete", Lucide::Trash, t!("common.delete"))
                .disabled(user_current.is_none())
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(i) = user_current {
                        this.delete_user_sound_preset(i, cx);
                    }
                })),
        );
    v_flex()
        .pt_3()
        .gap_5()
        .child(
            group_at("eq.reverb", t!("settings.reverb.title"), cx)
                .child(div().anchor("eq.reverb.room").child(rooms))
                .child(
                    v_flex()
                        .gap_1()
                        .anchor("eq.reverb.impulse")
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("sound.room.convolution")),
                        )
                        .child(convolution)
                        .children(impulse_error),
                )
                .child(
                    div()
                        .when(off, |this| this.opacity(0.5))
                        .child(slider_row_at(
                            "eq.reverb.mix",
                            t!("settings.reverb.mix"),
                            mix,
                            &panel.prefs.reverb_mix,
                            cx,
                        )),
                )
                .child(fine)
                .child(note(t!("sound.reverb.note"), cx)),
        )
        .child(
            group_at("eq.night", t!("sound.night.title"), cx)
                .child(
                    h_flex()
                        .justify_between()
                        .gap_3()
                        .child(div().text_sm().child(t!("sound.night.switch")))
                        .child(
                            Switch::new("night-mode")
                                .checked(c.enabled)
                                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                    this.set_night_mode(*checked, cx)
                                })),
                        ),
                )
                .child(night)
                .child(h_flex().child(
                    action("night-reset", Lucide::RotateCcw, t!("sound.night.reset")).on_click(
                        cx.listener(|this, _, window, cx| this.reset_compressor(window, cx)),
                    ),
                ))
                .child(note(t!("sound.night.note"), cx)),
        )
        .child(
            group_at("eq.sound_presets", t!("settings.sound.title"), cx)
                .child(
                    h_flex()
                        .gap_2()
                        .child(sounds)
                        .when(current.is_none() && user_current.is_none(), |this| {
                            this.child(Tag::secondary().small().child(t!("common.custom")))
                        }),
                )
                .child(sound_actions)
                .child(note(t!("sound.preset.note"), cx)),
        )
}
