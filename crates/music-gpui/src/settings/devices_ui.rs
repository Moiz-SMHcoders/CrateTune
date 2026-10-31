//! Settings → Devices (device hand-off, `integrations/devices.rs`): this device's
//! name, pairing (show a code with its link and QR code, or enter one), the paired
//! devices, and the relay for other networks.

use gpui::{
    AnyElement, AppContext as _, ClipboardItem, Context, Entity, FontWeight, IntoElement,
    ParentElement as _, SharedString, Styled as _, Subscription, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    switch::Switch,
    v_flex,
};

use super::SettingsPanel;
use super::file::Settings;
use super::panel::SettingsEvent;
use super::widgets::{Anchor as _, field_at, group_at, note, row_at, status_line};
use crate::app::{read_app, update_app};
use crate::integrations::config::{DevicesPrefs, relay_url_valid};
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
fn code_box(code: &str, cx: &gpui::App) -> impl IntoElement {
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
        .child(connection)
        .into_any_element()
}
