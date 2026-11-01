//! Settings → Devices (device hand-off, `integrations/devices.rs`): this device's
//! name, pairing (show a code with its link and QR code, or enter one), the paired
//! devices, and the relay for other networks.

use gpui::{
    AnyElement, AppContext as _, ClipboardItem, Context, Entity, FontWeight, IntoElement,
    ParentElement as _, SharedString, Styled as _, Subscription, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    switch::Switch,
    v_flex,
};

use super::SettingsPanel;
use super::file::Settings;
use super::panel::SettingsEvent;
use super::widgets::{Anchor as _, field_at, group_at, note, row, row_at, status_line};
use crate::app::{read_app, update_app};
use crate::integrations::config::{DevicesPrefs, relay_url_valid};
use music_core::sync::Category as SyncCategory;
use crate::integrations::devices::{DeviceStatus, PairStatus, status_text};
use crate::secrets::Secret;

pub struct DevicesUi {
    name: Entity<InputState>,
    relay: Entity<InputState>,
    code: Entity<InputState>,
    relay_invalid: bool,
}

impl DevicesUi {
    pub fn new(
        settings: &Settings,
        window: &mut Window,
        cx: &mut Context<SettingsPanel>,
    ) -> (Self, Vec<Subscription>) {
        let builtin = crate::identity::IROH_RELAY_URL.to_owned();
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(crate::integrations::config::computer_name())
                .default_value(settings.devices.name.clone())
        });
        let relay = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(builtin)
                .default_value(settings.devices.relay_url.clone())
        });
        let code = cx.new(|cx| InputState::new(window, cx).placeholder("ABCD-EFGH"));
        let mut subscriptions = Vec::new();
        for input in [&name, &relay] {
            subscriptions.push(cx.subscribe(
                input,
                |panel: &mut SettingsPanel, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        panel.sync_devices(cx);
                    }
                },
            ));
        }
        subscriptions.push(cx.subscribe(
            &code,
            |_: &mut SettingsPanel, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let value = input.read(cx).value().to_string();
                    update_app(cx, |app, cx| app.devices_pair(&value, cx));
                }
            },
        ));
        (
            Self {
                name,
                relay,
                code,
                relay_invalid: false,
            },
            subscriptions,
        )
    }
}

impl SettingsPanel {
    pub fn set_devices(&mut self, f: impl FnOnce(&mut DevicesPrefs), cx: &mut Context<Self>) {
        let before = self.settings.devices.clone();
        f(&mut self.settings.devices);
        if self.settings.devices != before {
            self.changed(false, cx);
            cx.emit(SettingsEvent::Devices);
            cx.notify();
        }
    }

    /// The device key, made (and stored in the keychain) on first use. `None` until
    /// the keychain has been read, or when a key was made before but can't be read
    /// now (a new one would make this a stranger to the paired devices).
    pub fn device_key_or_create(&mut self, cx: &mut Context<Self>) -> Option<String> {
        if !self.secrets_loaded {
            return None;
        }
        if let Some(key) = self
            .secret(Secret::DeviceKey)
            .filter(|k| music_core::handoff::parse_device_key(k).is_some())
        {
            return Some(key);
        }
        if self.settings.devices.key_set && !self.settings.devices.paired.is_empty() {
            eprintln!("music-gpui: the device key can't be read; devices stay off");
            return None;
        }
        let key = music_core::handoff::new_device_key();
        self.put_secret(Secret::DeviceKey, Some(key.clone()), cx);
        self.settings.devices.key_set = true;
        self.changed(false, cx);
        Some(key)
    }

    /// The name / relay fields, when left or confirmed.
    fn sync_devices(&mut self, cx: &mut Context<Self>) {
        let name = self.devices_ui.name.read(cx).value().trim().to_owned();
        let relay = self.devices_ui.relay.read(cx).value().trim().to_owned();
        self.devices_ui.relay_invalid = !relay.is_empty() && !relay_url_valid(&relay);
        let relay_ok = !self.devices_ui.relay_invalid;
        self.set_devices(
            |d| {
                d.name = name;
                if relay_ok {
                    d.relay_url = relay;
                }
            },
            cx,
        );
        cx.notify();
    }

    /// Settings another device synced (`integrations/device_sync.rs`): written in,
    /// saved, and the app told about the sections that changed.
    pub fn apply_synced(&mut self, changes: &[(String, serde_json::Value)], cx: &mut Context<Self>) {
        let next = super::sync_settings::apply(&self.settings, changes);
        if next == self.settings {
            return;
        }
        let old = std::mem::replace(&mut self.settings, next);
        let new = &self.settings;
        let look = old.appearance != new.appearance
            || old.custom_theme != new.custom_theme
            || old.app_look != new.app_look
            || old.effects != new.effects
            || old.player_bar != new.player_bar
            || old.now_playing != new.now_playing
            || old.video != new.video
            || old.home != new.home;
        if look {
            cx.emit(SettingsEvent::Appearance);
            cx.emit(SettingsEvent::NowPlaying);
        }
        if old.appearance.language != new.appearance.language {
            cx.emit(SettingsEvent::Language);
        }
        if old.equalizer != new.equalizer {
            cx.emit(SettingsEvent::Equalizer(self.equalizer()));
        }
        if old.playback != new.playback {
            cx.emit(SettingsEvent::PlaybackModes);
        }
        if old.audio != new.audio {
            cx.emit(SettingsEvent::Audio);
        }
        if old.lyrics != new.lyrics {
            cx.emit(SettingsEvent::Lyrics);
        }
        if old.content != new.content {
            cx.emit(SettingsEvent::Content);
        }
        if old.library != new.library {
            cx.emit(SettingsEvent::Library);
        }
        let integrations = old.sponsorblock != new.sponsorblock
            || old.together != new.together
            || old.ai_filter != new.ai_filter
            || old.discord != new.discord
            || old.scrobble_rules != new.scrobble_rules;
        self.changed(integrations, cx);
    }

    /// A sync switch (`None`: sync itself).
    pub fn set_sync(&mut self, cat: Option<SyncCategory>, on: bool, cx: &mut Context<Self>) {
        self.set_devices(
            |d| match cat {
                None => d.sync.enabled = on,
                Some(cat) => *d.sync.get_mut(cat) = on,
            },
            cx,
        );
    }

    /// Script hook (`@set=devices-name:…`, `devices-relay`, `devices-code`).
    pub fn set_devices_field(
        &mut self,
        field: &str,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let input = match field {
            "devices-name" => &self.devices_ui.name,
            "devices-relay" => &self.devices_ui.relay,
            "devices-code" => &self.devices_ui.code,
            _ => return false,
        };
        let value = value.to_owned();
        input.update(cx, |input, cx| input.set_value(value, window, cx));
        self.sync_devices(cx);
        true
    }
}

/// The pairing code as big, spaced characters.
pub(super) fn code_box(code: &str, cx: &gpui::App) -> impl IntoElement {
    div()
        .px_4()
        .py_2()
        .rounded(crate::theme::radius(8.))
        .bg(cx.theme().secondary)
        .text_2xl()
        .font_weight(FontWeight::SEMIBOLD)
        .child(SharedString::from(music_core::handoff::display_code(code)))
}

pub(super) fn render(
    panel: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let prefs = panel.settings.devices.clone();
    // The app's state only: the settings panel is being updated while it renders.
    let (list, offer, pair, id, error) = read_app(cx, |app, _| {
        let d = &app.integrations.devices;
        (
            prefs
                .paired
                .iter()
                .map(|p| (p.clone(), d.status_of(&p.id)))
                .collect::<Vec<_>>(),
            d.pairing.clone(),
            d.pair.clone(),
            d.id.clone(),
            d.error.clone(),
        )
    })
    .unwrap_or_default();
    let theme = cx.theme();
    let (muted, danger, success) = (theme.muted_foreground, theme.danger, theme.success);

    let this_device = group_at("devices.this", t!("settings.devices.this_device"), cx)
        .child(field_at(
            "devices.name",
            t!("settings.devices.name"),
            Input::new(&panel.devices_ui.name).small(),
            cx,
        ))
        .child(note(t!("settings.devices.name_note"), cx))
        .children(id.map(|id| {
            note(
                t!(
                    "settings.devices.id",
                    id = music_core::handoff::short_id(&id)
                ),
                cx,
            )
        }))
        .children(error.map(|e| status_line(e.into(), danger)));

    // Show a code, or enter the other device's.
    let mut pairing = group_at("devices.pair", t!("settings.devices.pair"), cx)
        .child(note(t!("settings.devices.pair_note"), cx));
    match &offer {
        Some(offer) => {
            let link = offer.link.to_link();
            let copy = link.clone();
            pairing = pairing.child(
                h_flex()
                    .gap_4()
                    .items_start()
                    .child(crate::qr::qr_code(&link, px(132.), cx))
                    .child(
                        v_flex()
                            .gap_2()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .child(SharedString::from(t!("settings.devices.code_shown"))),
                            )
                            .child(code_box(&offer.code, cx))
                            .child(note(t!("settings.devices.code_expires"), cx))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        Button::new("devices-copy-link")
                                            .small()
                                            .outline()
                                            .label(t!("settings.devices.copy_link"))
                                            .on_click(move |_, _, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    copy.clone(),
                                                ));
                                            }),
                                    )
                                    .child(
                                        Button::new("devices-hide-code")
                                            .small()
                                            .ghost()
                                            .label(t!("common.cancel"))
                                            .on_click(|_, _, cx| {
                                                update_app(cx, |app, cx| app.devices_hide_code(cx));
                                            }),
                                    ),
                            ),
                    ),
            );
        }
        None => {
            pairing = pairing.child(row_at(
                "devices.show_code",
                t!("settings.devices.show_code"),
                Some(t!("settings.devices.show_code_desc")),
                Button::new("devices-show-code")
                    .small()
                    .primary()
                    .label(t!("settings.devices.show_code_button"))
                    .on_click(|_, _, cx| {
                        update_app(cx, |app, cx| app.devices_show_code(cx));
                    }),
                cx,
            ));
        }
    }
    let looking = pair == PairStatus::Looking;
    let code_input = panel.devices_ui.code.clone();
    pairing = pairing
        .child(
            div()
                .anchor("devices.enter_code")
                .text_sm()
                .child(SharedString::from(t!("settings.devices.enter_code"))),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&panel.devices_ui.code).small()),
                )
                .child(
                    Button::new("devices-pair")
                        .small()
                        .outline()
                        .label(t!("settings.devices.pair_button"))
                        .loading(looking)
                        .on_click(move |_, _, cx| {
                            let value = code_input.read(cx).value().to_string();
                            update_app(cx, |app, cx| app.devices_pair(&value, cx));
                        }),
                ),
        )
        .child(note(t!("settings.devices.enter_note"), cx))
        .children(match &pair {
            PairStatus::Idle => None,
            PairStatus::Looking => Some(status_line(t!("settings.devices.looking").into(), muted)),
            PairStatus::Failed(e) => Some(status_line(e.clone().into(), danger)),
            PairStatus::Paired(name) => Some(status_line(
                t!("settings.devices.paired_with", device = name).into(),
                success,
            )),
        });

    let mut paired = group_at("devices.paired", t!("settings.devices.paired"), cx);
    if list.is_empty() {
        paired = paired.child(note(t!("settings.devices.none_paired"), cx));
    }
    for (ix, (device, status)) in list.into_iter().enumerate() {
        let id = device.id.clone();
        let status = match status {
            DeviceStatus::Checking => t!("devices.status.checking").to_owned(),
            other => status_text(&other),
        };
        paired = paired.child(
            h_flex()
                .gap_3()
                .justify_between()
                .child(
                    v_flex()
                        .min_w_0()
                        .child(
                            div()
                                .text_sm()
                                .truncate()
                                .child(SharedString::from(device.name.clone())),
                        )
                        .child(div().text_xs().text_color(muted).truncate().child(
                            SharedString::from(format!(
                                "{} · {status}",
                                music_core::handoff::short_id(&device.id)
                            )),
                        )),
                )
                .child(
                    Button::new(("devices-forget", ix))
                        .small()
                        .ghost()
                        .label(t!("settings.devices.forget"))
                        .on_click(move |_, _, cx| {
                            let id = id.clone();
                            update_app(cx, |app, cx| app.devices_forget(&id, cx));
                        }),
                ),
        );
    }

    let relay_error = panel
        .devices_ui
        .relay_invalid
        .then(|| t!("settings.devices.relay_error"));
    let connection = group_at("devices.networks", t!("settings.devices.networks"), cx)
        .child(row_at(
            "devices.use_relay",
            t!("settings.devices.use_relay"),
            Some(t!("settings.devices.use_relay_desc")),
            Switch::new("devices-use-relay")
                .checked(prefs.use_relay)
                .on_click(cx.listener(|p, on: &bool, _, cx| {
                    let on = *on;
                    p.set_devices(|d| d.use_relay = on, cx);
                })),
            cx,
        ))
        .when(prefs.use_relay, |this| {
            this.child(field_at(
                "devices.relay",
                t!("settings.devices.relay"),
                Input::new(&panel.devices_ui.relay).small(),
                cx,
            ))
            .children(relay_error.map(|e| status_line(e.into(), danger)))
            .child(note(t!("settings.devices.relay_note"), cx))
        })
        .child(
            div()
                .anchor("devices.privacy")
                .child(note(t!("settings.devices.privacy"), cx)),
        );

    v_flex()
        .gap_5()
        .child(this_device)
        .child(pairing)
        .child(paired)
        .child(sync_group(&prefs, cx))
        .child(connection)
        .into_any_element()
}

/// A sync category's switch label and description.
fn category_text(cat: SyncCategory) -> (&'static str, &'static str) {
    match cat {
        SyncCategory::Settings => (
            t!("settings.devices.sync.settings"),
            t!("settings.devices.sync.settings_desc"),
        ),
        SyncCategory::Sound => (
            t!("settings.devices.sync.sound"),
            t!("settings.devices.sync.sound_desc"),
        ),
        SyncCategory::Likes => (
            t!("settings.devices.sync.likes"),
            t!("settings.devices.sync.likes_desc"),
        ),
        SyncCategory::Playlists => (
            t!("settings.devices.sync.playlists"),
            t!("settings.devices.sync.playlists_desc"),
        ),
        SyncCategory::Pins => (
            t!("settings.devices.sync.pins"),
            t!("settings.devices.sync.pins_desc"),
        ),
        SyncCategory::QueueTabs => (
            t!("settings.devices.sync.queue_tabs"),
            t!("settings.devices.sync.queue_tabs_desc"),
        ),
        SyncCategory::History => (
            t!("settings.devices.sync.history"),
            t!("settings.devices.sync.history_desc"),
        ),
    }
}

/// Settings → Devices → Sync: the switches, each paired device's last sync, "Sync now".
fn sync_group(prefs: &DevicesPrefs, cx: &mut Context<SettingsPanel>) -> AnyElement {
    let sync = prefs.sync.clone();
    // The app's state only (the settings panel is being updated while it renders).
    let (running, last, busy, errors, received) = read_app(cx, |app, _| {
        let s = &app.integrations.sync;
        (
            s.running(),
            s.last.clone(),
            s.busy.clone(),
            s.errors.clone(),
            s.received,
        )
    })
    .unwrap_or_default();
    let theme = cx.theme();
    let (muted, danger) = (theme.muted_foreground, theme.danger);
    let mut group = group_at("devices.sync", t!("settings.devices.sync.title"), cx)
        .child(note(t!("settings.devices.sync.note"), cx))
        .child(row_at(
            "devices.sync.enabled",
            t!("settings.devices.sync.enabled"),
            Some(t!("settings.devices.sync.enabled_desc")),
            Switch::new("devices-sync")
                .checked(sync.enabled)
                .on_click(cx.listener(|p, on: &bool, _, cx| p.set_sync(None, *on, cx))),
            cx,
        ));
    if !sync.enabled {
        return group.into_any_element();
    }
    for (ix, cat) in SyncCategory::ALL.into_iter().enumerate() {
        let (label, desc) = category_text(cat);
        group = group.child(row(
            label,
            Some(desc),
            Switch::new(("devices-sync-cat", ix))
                .checked(sync.get(cat))
                .on_click(cx.listener(move |p, on: &bool, _, cx| p.set_sync(Some(cat), *on, cx))),
            cx,
        ));
    }
    let now = music_core::handoff::now_unix_ms();
    let mut devices = v_flex().gap_1().anchor("devices.sync.status");
    if prefs.paired.is_empty() {
        devices = devices.child(note(t!("settings.devices.sync.no_devices"), cx));
    }
    for device in &prefs.paired {
        let status: SharedString = if busy.contains(&device.id) {
            t!("settings.devices.sync.syncing").into()
        } else if let Some(error) = errors.get(&device.id) {
            t!("settings.devices.sync.failed", error = error).into()
        } else if let Some(at) = last.get(&device.id) {
            t!(
                "settings.devices.sync.last",
                ago = crate::i18n::format::ago(now.saturating_sub(*at) / 1000)
            )
            .into()
        } else {
            t!("settings.devices.sync.never").into()
        };
        let color = if errors.contains_key(&device.id) {
            danger
        } else {
            muted
        };
        devices = devices.child(
            h_flex()
                .gap_2()
                .justify_between()
                .child(
                    div()
                        .text_sm()
                        .truncate()
                        .child(SharedString::from(device.name.clone())),
                )
                .child(div().text_xs().text_color(color).truncate().child(status)),
        );
    }
    group
        .child(devices)
        .children(received.map(|(at, n)| {
            note(
                tp!(
                    "settings.devices.sync.received",
                    n,
                    ago = crate::i18n::format::ago(now.saturating_sub(at) / 1000)
                ),
                cx,
            )
        }))
        .child(
            h_flex().child(
                Button::new("devices-sync-now")
                    .small()
                    .outline()
                    .label(t!("settings.devices.sync.now"))
                    .loading(!busy.is_empty())
                    .disabled(!running || prefs.paired.is_empty())
                    .on_click(|_, _, cx| {
                        update_app(cx, |app, cx| app.sync_now(cx));
                    }),
            ),
        )
        .child(note(t!("settings.devices.sync.rules"), cx))
        .child(note(t!("settings.devices.sync.private"), cx))
        .into_any_element()
}
