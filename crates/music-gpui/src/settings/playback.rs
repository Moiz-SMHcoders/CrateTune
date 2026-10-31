//! Playback section: queue modes (also in the bar and queue panel), audio
//! (quality, normalization, skip silence, auto-skip), transitions (gapless /
//! crossfade), speed + pitch, output device and the sleep timer.

use std::time::Duration;

use gpui::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div,
    prelude::FluentBuilder as _,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    switch::Switch,
    v_flex,
};
use music_core::{Quality, SleepTimer};

use super::SettingsPanel;
use super::prefs::{SLEEP_PRESETS, StreamSourceMode, quality_title};
use super::prefs_ui::{Devices, choice, slider_row_at};
use super::widgets::{group_at, note, row_at};
use crate::app::{read_app, update_app};
use crate::i18n::format::decimal;
use crate::queue::Repeat;

pub(super) fn render(
    panel: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    if matches!(panel.prefs.devices, Devices::NotLoaded) {
        panel.refresh_devices(cx);
    }
    let p = panel.settings.playback.clone();
    let a = panel.settings.audio.clone();
    let muted = cx.theme().muted_foreground;

    // ---- queue
    let shuffle = Switch::new("settings-shuffle")
        .checked(p.shuffle)
        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
            let repeat = this.settings.playback.repeat;
            this.choose_playback_modes(*checked, repeat, cx)
        }));
    let repeat = choice(
        "settings-repeat",
        &[Repeat::Off, Repeat::All, Repeat::One],
        p.repeat,
        |mode| match mode {
            Repeat::Off => t!("common.off"),
            Repeat::All => t!("settings.playback.repeat_all"),
            Repeat::One => t!("settings.playback.repeat_one"),
        },
        |this, mode, cx| {
            let shuffle = this.settings.playback.shuffle;
            this.choose_playback_modes(shuffle, mode, cx)
        },
        cx,
    );

    // ---- audio
    let quality = choice(
        "settings-quality",
        &Quality::ALL,
        a.quality,
        quality_title,
        |this, q, cx| this.set_quality(q, cx),
        cx,
    );
    let stream_source = choice(
        "settings-stream-source",
        &StreamSourceMode::ALL,
        a.stream_source,
        |m| m.title(),
        |this, m, cx| this.set_stream_source(m, cx),
        cx,
    );
    let switch = |id: &'static str,
                  on: bool,
                  f: fn(&mut SettingsPanel, bool, &mut Context<SettingsPanel>),
                  cx: &Context<SettingsPanel>| {
        Switch::new(id)
            .checked(on)
            .on_click(cx.listener(move |this, checked: &bool, _, cx| f(this, *checked, cx)))
    };
    let normalize = switch(
        "settings-normalize",
        a.normalize,
        SettingsPanel::set_normalization,
        cx,
    );
    let silence = switch(
        "settings-skip-silence",
        a.skip_silence,
        SettingsPanel::set_skip_silence,
        cx,
    );
    let auto_skip = switch(
        "settings-auto-skip",
        a.auto_skip,
        SettingsPanel::set_auto_skip,
        cx,
    );

    // ---- transitions, speed & pitch
    let crossfade_label = if a.crossfade_secs <= 0. {
        t!("common.off").to_owned()
    } else {
        t!(
            "settings.playback.crossfade_value",
            seconds = decimal(a.crossfade_secs.into(), 1)
        )
    };
    let speed_label = t!(
        "settings.playback.speed_value",
        speed = decimal(a.speed.into(), 2)
    );
    let pitch_label = t!(
        "settings.playback.pitch_value",
        value = if a.pitch > 0. {
            format!("+{:.0}", a.pitch)
        } else {
            format!("{:.0}", a.pitch)
        }
    );
    let reset = Button::new("settings-speed-reset")
        .small()
        .outline()
        .label(t!("common.reset"))
        .disabled(a.speed == 1. && a.pitch == 0.)
        .on_click(
            cx.listener(|this, _, window, cx| this.set_speed_pitch(Some(1.), Some(0.), window, cx)),
        );

    // ---- output device
    let device_label: SharedString = match (&a.output_device, &panel.prefs.devices) {
        (None, _) => t!("settings.playback.system_default").into(),
        (Some(id), Devices::Ready(list)) => match list.iter().find(|d| &d.id == id) {
            Some(d) => d.name.clone().into(),
            None => t!(
                "settings.playback.device_missing",
                device = a.output_device_name.clone().unwrap_or_else(|| id.clone())
            )
            .into(),
        },
        (Some(id), _) => a
            .output_device_name
            .clone()
            .unwrap_or_else(|| id.clone())
            .into(),
    };
    // Without ALSA's virtual / duplicate devices unless "Show all devices" is on.
    let devices: Vec<(String, String, bool)> = match &panel.prefs.devices {
        Devices::Ready(list) => music_core::tidy_output_devices(
            list.clone(),
            a.show_all_devices,
            a.output_device.as_deref(),
        )
        .into_iter()
        .map(|d| (d.id, d.name, d.is_default))
        .collect(),
        _ => Vec::new(),
    };
    let hidden = match &panel.prefs.devices {
        Devices::Ready(list) => list.len().saturating_sub(devices.len()),
        _ => 0,
    };
    let show_all = Switch::new("settings-show-all-devices")
        .checked(a.show_all_devices)
        .on_click(
            cx.listener(|this, checked: &bool, _, cx| this.set_show_all_devices(*checked, cx)),
        );
    let selected_device = a.output_device.clone();
    let panel_entity = cx.entity();
    let device_menu = Button::new("settings-device")
        .small()
        .outline()
        .label(device_label)
        .dropdown_menu(move |mut menu, _, _| {
            let pick = |device: Option<(String, String)>| {
                let panel = panel_entity.clone();
                move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                    let device = device.clone();
                    panel.update(cx, |p, cx| p.set_output_device(device, cx));
                }
            };
            menu = menu.item(
                PopupMenuItem::new(t!("settings.playback.system_default"))
                    .checked(selected_device.is_none())
                    .on_click(pick(None)),
            );
            if !devices.is_empty() {
                menu = menu.separator();
            }
            for (id, name, is_default) in &devices {
                let label = if *is_default {
                    t!("settings.playback.device_current_default", device = name)
                } else {
                    name.clone()
                };
                menu = menu.item(
                    PopupMenuItem::new(label)
                        .checked(selected_device.as_ref() == Some(id))
                        .on_click(pick(Some((id.clone(), name.clone())))),
                );
            }
            menu
        });
    let in_use = read_app(cx, |app, _| app.playback.output_device.clone()).flatten();
    let refresh = Button::new("settings-device-refresh")
        .small()
        .ghost()
        .label(t!("common.refresh"))
        .loading(matches!(panel.prefs.devices, Devices::Loading))
        .on_click(cx.listener(|this, _, _, cx| this.refresh_devices(cx)));

    // ---- sleep timer
    let sleep = read_app(cx, |app, _| app.sleep_timer_label()).flatten();
    let custom = panel.sleep_custom();
    let arm = |label: String, id: String, timer: Option<SleepTimer>| {
        Button::new(SharedString::from(id))
            .small()
            .outline()
            .label(label)
            .on_click(move |_, _, cx| {
                update_app(cx, |app, cx| app.set_sleep_timer(timer, cx));
            })
    };
    let sleep_buttons = h_flex()
        .gap_1()
        .flex_wrap()
        .children(SLEEP_PRESETS.map(|m| {
            arm(
                t!("time.minutes", minutes = m),
                format!("sleep-{m}"),
                Some(SleepTimer::After(Duration::from_secs(u64::from(m) * 60))),
            )
        }))
        .child(arm(
            t!("settings.playback.end_of_track").into(),
            "sleep-eot".into(),
            Some(SleepTimer::EndOfTrack),
        ))
        .child(arm(
            t!(
                "settings.playback.sleep_custom_button",
                minutes = custom.as_secs() / 60
            ),
            "sleep-custom".into(),
            Some(SleepTimer::After(custom)),
        ))
        .when(sleep.is_some(), |this| {
            this.child(
                Button::new("sleep-off")
                    .small()
                    .primary()
                    .selected(true)
                    .label(t!("settings.playback.turn_off"))
                    .on_click(|_, _, cx| {
                        update_app(cx, |app, cx| app.set_sleep_timer(None, cx));
                    }),
            )
        });

    v_flex()
        .gap_5()
        .child(
            group_at("playback.queue", t!("settings.playback.queue"), cx)
                .child(row_at(
                    "playback.shuffle",
                    t!("settings.playback.shuffle"),
                    Some(t!("settings.playback.shuffle_desc")),
                    shuffle,
                    cx,
                ))
                .child(row_at(
                    "playback.repeat",
                    t!("settings.playback.repeat"),
                    Some(t!("settings.playback.repeat_desc")),
                    repeat,
                    cx,
                )),
        )
        .child(
            group_at("playback.audio", t!("settings.playback.audio"), cx)
                .child(row_at(
                    "playback.streams",
                    t!("settings.playback.streams"),
                    Some(stream_source_note(a.stream_source)),
                    stream_source,
                    cx,
                ))
                .child(row_at(
                    "playback.quality",
                    t!("settings.playback.quality"),
                    Some(t!("settings.playback.quality_desc")),
                    quality,
                    cx,
                ))
                .child(row_at(
                    "playback.normalize",
                    t!("settings.playback.normalize"),
                    Some(t!("settings.playback.normalize_desc")),
                    normalize,
                    cx,
                ))
                .child(row_at(
                    "playback.skip_silence",
                    t!("settings.playback.skip_silence"),
                    Some(t!("settings.playback.skip_silence_desc")),
                    silence,
                    cx,
                ))
                .child(row_at(
                    "playback.auto_skip",
                    t!("settings.playback.auto_skip"),
                    Some(t!("settings.playback.auto_skip_desc")),
                    auto_skip,
                    cx,
                )),
        )
        .child(
            group_at(
                "playback.transitions",
                t!("settings.playback.transitions"),
                cx,
            )
            .child(slider_row_at(
                "playback.crossfade",
                t!("settings.playback.crossfade"),
                crossfade_label,
                &panel.prefs.crossfade,
                cx,
            ))
            .child(note(t!("settings.playback.transitions_note"), cx)),
        )
        .child(
            group_at(
                "playback.speed_pitch",
                t!("settings.playback.speed_pitch"),
                cx,
            )
            .child(slider_row_at(
                "playback.speed",
                t!("settings.playback.speed"),
                speed_label,
                &panel.prefs.speed,
                cx,
            ))
            .child(slider_row_at(
                "playback.pitch",
                t!("settings.playback.pitch"),
                pitch_label,
                &panel.prefs.pitch,
                cx,
            ))
            .child(
                h_flex()
                    .gap_2()
                    .justify_between()
                    .child(
                        div()
                            .min_w_0()
                            .child(note(t!("settings.playback.speed_pitch_note"), cx)),
                    )
                    .child(div().flex_none().child(reset)),
            ),
        )
        .child(
            group_at("playback.output", t!("settings.playback.output"), cx)
                .child(
                    h_flex()
                        .gap_2()
                        .child(device_menu)
                        .child(refresh)
                        .child(muted_text(
                            in_use.map(|d| t!("settings.playback.playing_through", device = d)),
                            muted,
                        )),
                )
                .child(note(t!("settings.playback.output_note"), cx))
                .child(row_at(
                    "playback.show_all_devices",
                    t!("settings.playback.show_all_devices"),
                    Some(t!("settings.playback.show_all_devices_desc")),
                    show_all,
                    cx,
                ))
                .when(hidden > 0 && !a.show_all_devices, |this| {
                    this.child(note(tp!("settings.playback.devices_hidden", hidden), cx))
                }),
        )
        .child(
            group_at("playback.sleep", t!("settings.playback.sleep"), cx)
                .child(muted_text(
                    Some(match &sleep {
                        Some(label) => t!("settings.playback.sleep_armed", when = label),
                        None => t!("common.off").to_owned(),
                    }),
                    muted,
                ))
                .child(sleep_buttons)
                .child(slider_row_at(
                    "playback.sleep.custom",
                    t!("settings.playback.custom_length"),
                    t!("time.minutes", minutes = custom.as_secs() / 60),
                    &panel.prefs.sleep_custom,
                    cx,
                ))
                .child(note(t!("settings.playback.sleep_note"), cx)),
        )
        .into_any_element()
}

fn muted_text(text: Option<String>, color: gpui::Hsla) -> impl IntoElement {
    gpui::div()
        .min_w_0()
        .text_sm()
        .text_color(color)
        .truncate()
        .children(text)
}

/// What a stream-source choice means, under the selector.
fn stream_source_note(mode: StreamSourceMode) -> &'static str {
    match mode {
        StreamSourceMode::Auto => t!("settings.playback.stream_auto"),
        StreamSourceMode::Tv => t!("settings.playback.stream_tv"),
        StreamSourceMode::WebPoToken => t!("settings.playback.stream_web"),
        StreamSourceMode::Preview => t!("settings.playback.stream_preview"),
    }
}
