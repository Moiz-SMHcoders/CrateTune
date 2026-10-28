//! Playback section: queue modes (also in the bar and queue panel), audio
//! (quality, normalization, skip silence, auto-skip), transitions (gapless /
//! crossfade), speed + pitch, output device and the sleep timer.

use std::time::Duration;

use gpui::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, Window,
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
use super::prefs_ui::{Devices, choice, slider_row};
use super::widgets::{group, note, row};
use crate::app::{read_app, update_app};
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
            Repeat::Off => "Off",
            Repeat::All => "All",
            Repeat::One => "One",
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
        "Off".to_owned()
    } else {
        format!("{:.1} s", a.crossfade_secs)
    };
    let speed_label = format!("{:.2}×", a.speed);
    let pitch_label = if a.pitch > 0. {
        format!("+{:.0} st", a.pitch)
    } else {
        format!("{:.0} st", a.pitch)
    };
    let reset = Button::new("settings-speed-reset")
        .small()
        .outline()
        .label("Reset")
        .disabled(a.speed == 1. && a.pitch == 0.)
        .on_click(
            cx.listener(|this, _, window, cx| this.set_speed_pitch(Some(1.), Some(0.), window, cx)),
        );

    // ---- output device
    let device_label: SharedString = match (&a.output_device, &panel.prefs.devices) {
        (None, _) => "System default".into(),
        (Some(id), Devices::Ready(list)) => match list.iter().find(|d| &d.id == id) {
            Some(d) => d.name.clone().into(),
            None => format!(
                "{} (not connected)",
                a.output_device_name.clone().unwrap_or_else(|| id.clone())
            )
            .into(),
        },
        (Some(id), _) => a
            .output_device_name
            .clone()
            .unwrap_or_else(|| id.clone())
            .into(),
    };
    let devices: Vec<(String, String, bool)> = match &panel.prefs.devices {
        Devices::Ready(list) => list
            .iter()
            .map(|d| (d.id.clone(), d.name.clone(), d.is_default))
            .collect(),
        _ => Vec::new(),
    };
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
                PopupMenuItem::new("System default")
                    .checked(selected_device.is_none())
                    .on_click(pick(None)),
            );
            if !devices.is_empty() {
                menu = menu.separator();
            }
            for (id, name, is_default) in &devices {
                let label = if *is_default {
                    format!("{name} (current default)")
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
        .label("Refresh")
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
                format!("{m} min"),
                format!("sleep-{m}"),
                Some(SleepTimer::After(Duration::from_secs(u64::from(m) * 60))),
            )
        }))
        .child(arm(
            "End of track".into(),
            "sleep-eot".into(),
            Some(SleepTimer::EndOfTrack),
        ))
        .child(arm(
            format!("{} min (custom)", custom.as_secs() / 60),
            "sleep-custom".into(),
            Some(SleepTimer::After(custom)),
        ))
        .when(sleep.is_some(), |this| {
            this.child(
                Button::new("sleep-off")
                    .small()
                    .primary()
                    .selected(true)
                    .label("Turn off")
                    .on_click(|_, _, cx| {
                        update_app(cx, |app, cx| app.set_sleep_timer(None, cx));
                    }),
            )
        });

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
        .child(
            group("Audio", cx)
                .child(row(
                    "YouTube streams",
                    Some(stream_source_note(a.stream_source)),
                    stream_source,
                    cx,
                ))
                .child(row(
                    "Streaming quality",
                    Some("Low saves data; High prefers Opus (~160 kbps) over AAC 128."),
                    quality,
                    cx,
                ))
                .child(row(
                    "Loudness normalization",
                    Some("Evens out volume with YouTube's loudness data and ReplayGain tags."),
                    normalize,
                    cx,
                ))
                .child(row(
                    "Skip silence",
                    Some("Drops silence at track edges and shortens long silent gaps."),
                    silence,
                    cx,
                ))
                .child(row(
                    "Skip songs that fail to load",
                    Some("Moves on instead of stopping (up to 3 in a row)."),
                    auto_skip,
                    cx,
                )),
        )
        .child(
            group("Transitions", cx)
                .child(slider_row(
                    "Crossfade",
                    crossfade_label,
                    &panel.prefs.crossfade,
                    cx,
                ))
                .child(note(
                    "Playback is gapless: the next song is buffered about 30 s ahead and starts \
                     on the very next sample. Crossfade blends the two (also before a preview \
                     ends early).",
                    cx,
                )),
        )
        .child(
            group("Speed and pitch", cx)
                .child(slider_row("Speed", speed_label, &panel.prefs.speed, cx))
                .child(slider_row("Pitch", pitch_label, &panel.prefs.pitch, cx))
                .child(
                    h_flex()
                        .justify_between()
                        .child(note("Speed keeps the pitch; pitch keeps the speed.", cx))
                        .child(reset),
                ),
        )
        .child(
            group("Output device", cx)
                .child(
                    h_flex()
                        .gap_2()
                        .child(device_menu)
                        .child(refresh)
                        .child(muted_text(
                            in_use.map(|d| format!("Playing through {d}")),
                            muted,
                        )),
                )
                .child(note(
                    "A missing device falls back to the system default and is picked up again \
                     when it comes back.",
                    cx,
                )),
        )
        .child(
            group("Sleep timer", cx)
                .child(muted_text(
                    Some(match &sleep {
                        Some(label) => format!("Pauses playback: {label}"),
                        None => "Off".to_owned(),
                    }),
                    muted,
                ))
                .child(sleep_buttons)
                .child(slider_row(
                    "Custom length",
                    format!("{} min", custom.as_secs() / 60),
                    &panel.prefs.sleep_custom,
                    cx,
                ))
                .child(note(
                    "Also in the player bar and on the Now Playing screen (moon icon).",
                    cx,
                )),
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
        StreamSourceMode::Auto => {
            "Full songs: TV client, then the web client with a PoToken, then a ~1 min preview."
        }
        StreamSourceMode::Tv => "Full songs via the TV client (no PoToken); a preview if it fails.",
        StreamSourceMode::WebPoToken => {
            "Full songs via the web client and a BotGuard PoToken (macOS); a preview if it fails."
        }
        StreamSourceMode::Preview => "Only the first ~1 minute of each song (the old behaviour).",
    }
}
