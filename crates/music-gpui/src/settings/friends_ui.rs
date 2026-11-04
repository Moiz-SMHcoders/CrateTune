//! Settings → Friends (friend groups and the activity feed, `integrations/friends.rs`):
//! the switch, your name, sharing and the private session, your groups (create,
//! invite with a code / link / QR code, join, leave, remove), and the recovery phrase.

use gpui::{
    AnyElement, AppContext as _, ClipboardItem, Context, Entity, FontWeight, IntoElement as _,
    ParentElement as _, SharedString, Styled as _, Subscription, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    switch::Switch,
    tag::Tag,
    v_flex,
};
use music_core::friends::Identity;

use super::SettingsPanel;
use super::devices_ui::code_box;
use super::file::Settings;
use super::panel::SettingsEvent;
use super::widgets::{Anchor as _, field_at, group_at, long_note, note, row_at, status_line};
use crate::app::{read_app, update_app};
use crate::integrations::config::FriendsPrefs;
use crate::integrations::devices::PairStatus;
use crate::secrets::Secret;

pub struct FriendsUi {
    name: Entity<InputState>,
    group: Entity<InputState>,
    code: Entity<InputState>,
    phrase: Entity<InputState>,
    show_phrase: bool,
}

impl FriendsUi {
    pub fn new(
        settings: &Settings,
        window: &mut Window,
        cx: &mut Context<SettingsPanel>,
    ) -> (Self, Vec<Subscription>) {
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(settings.devices.name())
                .default_value(settings.friends.name.clone())
        });
        let group = cx.new(|cx| InputState::new(window, cx));
        crate::i18n::track_placeholder(
            &group,
            k!("settings.friends.group_placeholder"),
            window,
            cx,
        );
        let code = cx.new(|cx| InputState::new(window, cx).placeholder("ABCD-EFGH"));
        let phrase = cx.new(|cx| InputState::new(window, cx));
        crate::i18n::track_placeholder(
            &phrase,
            k!("settings.friends.phrase_placeholder"),
            window,
            cx,
        );
        let subscriptions = vec![
            cx.subscribe(
                &name,
                |panel: &mut SettingsPanel, input, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        let value = input.read(cx).value().trim().to_owned();
                        panel.set_friends_prefs(|f| f.name = value, cx);
                    }
                },
            ),
            // The app reads the settings: not while this panel is being updated.
            cx.subscribe(
                &group,
                |_: &mut SettingsPanel, input, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        let value = input.read(cx).value().to_string();
                        cx.defer(move |cx| create_group_later(value, cx));
                    }
                },
            ),
            cx.subscribe(
                &code,
                |_: &mut SettingsPanel, input, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        let value = input.read(cx).value().to_string();
                        cx.defer(move |cx| {
                            update_app(cx, |app, cx| app.friends_join(&value, cx));
                        });
                    }
                },
            ),
        ];
        (
            Self {
                name,
                group,
                code,
                phrase,
                show_phrase: false,
            },
            subscriptions,
        )
    }
}

/// "Create a group" from the button: the click already runs in the window, so this
/// must not update the window again (GPUI refuses that, and the click did nothing).
fn create_group(name: &str, window: &mut Window, cx: &mut gpui::App) {
    update_app(cx, |app, cx| app.friends_create_group(name, window, cx));
}

/// "Create a group" from Enter in the name field: deferred out of the panel's update,
/// so the window is free to update here.
fn create_group_later(name: String, cx: &mut gpui::App) {
    let Some(window) = crate::app::app_entity(cx).and_then(|a| a.read(cx).main_window) else {
        return;
    };
    if let Err(e) = window.update(cx, |_, window, cx| create_group(&name, window, cx)) {
        eprintln!("music-gpui: friends: creating a group: {e}");
    }
}

impl SettingsPanel {
    pub fn set_friends_prefs(&mut self, f: impl FnOnce(&mut FriendsPrefs), cx: &mut Context<Self>) {
        let before = self.settings.friends.clone();
        f(&mut self.settings.friends);
        if self.settings.friends != before {
            self.changed(false, cx);
            // Friends ride on the device hand-off node.
            cx.emit(SettingsEvent::Devices);
            cx.notify();
        }
    }

    /// The member key, made (and stored in the keychain) on first use. `None` until
    /// the keychain was read, or when a key was made before but can't be read now (a
    /// new one would make this a stranger to the groups).
    pub fn friend_identity_or_create(&mut self, cx: &mut Context<Self>) -> Option<Identity> {
        if !self.secrets_loaded {
            return None;
        }
        if let Some(identity) = self
            .secret(Secret::FriendKey)
            .and_then(|k| Identity::from_secret(&k))
        {
            return Some(identity);
        }
        if self.settings.friends.key_set && !self.settings.friends.groups.is_empty() {
            eprintln!("music-gpui: the member key can't be read; friends stay off");
            return None;
        }
        let identity = Identity::generate();
        self.set_friend_identity(&identity, cx);
        Some(identity)
    }

    /// Stores a member key (made here, or restored from a recovery phrase).
    pub fn set_friend_identity(&mut self, identity: &Identity, cx: &mut Context<Self>) {
        self.put_secret(Secret::FriendKey, Some(identity.to_secret()), cx);
        self.settings.friends.key_set = true;
        self.changed(false, cx);
    }

    /// Script hook (`@set=friends-name:…`, `friends-group`, `friends-code`, `friends-phrase`).
    pub fn set_friends_field(
        &mut self,
        field: &str,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let input = match field {
            "friends-name" => &self.friends_ui.name,
            "friends-group" => &self.friends_ui.group,
            "friends-code" => &self.friends_ui.code,
            "friends-phrase" => &self.friends_ui.phrase,
            _ => return false,
        };
        let value = value.to_owned();
        input.update(cx, |input, cx| input.set_value(value.clone(), window, cx));
        if field == "friends-name" {
            self.set_friends_prefs(|f| f.name = value.trim().to_owned(), cx);
        }
        true
    }

    /// `@friends-phrase=on|off`: shows the recovery phrase (screenshots).
    pub fn show_recovery_phrase(&mut self, on: bool, cx: &mut Context<Self>) {
        self.friends_ui.show_phrase = on;
        cx.notify();
    }
}

pub(super) fn render(
    panel: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let prefs = panel.settings.friends.clone();
    let theme = cx.theme();
    let (muted, danger, success) = (theme.muted_foreground, theme.danger, theme.success);
    let intro = group_at("friends.enabled", t!("settings.friends.title"), cx)
        .child(long_note(t!("settings.friends.intro"), cx))
        .child(row_at(
            "friends.switch",
            t!("settings.friends.enabled"),
            Some(t!("settings.friends.enabled_desc")),
            Switch::new("friends-enabled")
                .checked(prefs.enabled)
                .on_click(cx.listener(|p, on: &bool, _, cx| {
                    let on = *on;
                    p.set_friends_prefs(|f| f.enabled = on, cx);
                })),
            cx,
        ))
        .child(
            div()
                .anchor("friends.privacy")
                .child(long_note(t!("settings.friends.privacy"), cx)),
        );
    if !prefs.enabled {
        return v_flex().gap_5().child(intro).into_any_element();
    }
    // The app's state only (the settings panel is being updated while it renders).
    let (me, private, invite, join) = read_app(cx, |app, _| {
        let f = &app.integrations.friends;
        (
            f.member_id.clone(),
            f.private_session,
            f.invite.clone(),
            f.join.clone(),
        )
    })
    .unwrap_or_default();

    let you = group_at("friends.you", t!("settings.friends.you"), cx)
        .child(field_at(
            "friends.name",
            t!("settings.friends.name"),
            Input::new(&panel.friends_ui.name).small(),
            cx,
        ))
        .child(note(t!("settings.friends.name_note"), cx))
        .child(row_at(
            "friends.share",
            t!("settings.friends.share"),
            Some(t!("settings.friends.share_desc")),
            Switch::new("friends-share")
                .checked(prefs.share)
                .on_click(cx.listener(|p, on: &bool, _, cx| {
                    let on = *on;
                    p.set_friends_prefs(|f| f.share = on, cx);
                })),
            cx,
        ))
        .child(row_at(
            "friends.private",
            t!("settings.friends.private"),
            Some(t!("settings.friends.private_desc")),
            Switch::new("friends-private")
                .checked(private)
                .on_click(|on: &bool, _, cx| {
                    let on = *on;
                    update_app(cx, |app, cx| app.friends_set_private(on, cx));
                }),
            cx,
        ))
        .children(me.as_ref().map(|id| {
            note(
                t!(
                    "settings.friends.member_id",
                    id = music_core::handoff::short_id(id)
                ),
                cx,
            )
        }));

    let mut groups = group_at("friends.groups", t!("settings.friends.groups"), cx);
    if prefs.groups.is_empty() {
        groups = groups.child(long_note(t!("settings.friends.no_groups"), cx));
    }
    for (gx, g) in prefs.groups.iter().enumerate() {
        let gid = g.id().to_owned();
        let owner = me.as_deref() == Some(g.header.owner.as_str());
        let mut members = v_flex().gap_1();
        for (mx, m) in g.current().into_iter().enumerate() {
            let you = me.as_deref() == Some(m.member.as_str());
            let label = if you {
                t!("settings.friends.you_name", name = m.name)
            } else {
                m.name.clone()
            };
            let (group_id, member) = (gid.clone(), m.member.clone());
            members = members.child(
                h_flex()
                    .gap_2()
                    .justify_between()
                    .child(
                        h_flex()
                            .gap_2()
                            .min_w_0()
                            .child(div().text_sm().truncate().child(SharedString::from(label)))
                            .when(m.member == g.header.owner, |this| {
                                this.child(
                                    Tag::secondary().small().child(t!("settings.friends.owner")),
                                )
                            }),
                    )
                    .when(owner && !you, |this| {
                        this.child(
                            Button::new(("friends-remove", gx * 100 + mx))
                                .xsmall()
                                .ghost()
                                .label(t!("settings.friends.remove"))
                                .on_click(move |_, _, cx| {
                                    let (g, m) = (group_id.clone(), member.clone());
                                    update_app(cx, |app, cx| app.friends_remove(&g, &m, cx));
                                }),
                        )
                    }),
            );
        }
        let shown = invite
            .as_ref()
            .filter(|(id, _)| *id == gid)
            .map(|(_, o)| o.clone());
        let (invite_id, leave_id) = (gid.clone(), gid.clone());
        let mut card = v_flex()
            .gap_2()
            .p_3()
            .rounded(crate::theme::radius(8.))
            .bg(cx.theme().secondary)
            .child(
                h_flex()
                    .gap_2()
                    .justify_between()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .truncate()
                            .child(SharedString::from(g.name().to_owned())),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new(("friends-invite", gx))
                                    .small()
                                    .outline()
                                    .label(t!("settings.friends.invite"))
                                    .on_click(move |_, _, cx| {
                                        let id = invite_id.clone();
                                        update_app(cx, |app, cx| app.friends_invite(&id, cx));
                                    }),
                            )
                            .child(
                                Button::new(("friends-leave", gx))
                                    .small()
                                    .ghost()
                                    .label(t!("settings.friends.leave"))
                                    .on_click(move |_, _, cx| {
                                        let id = leave_id.clone();
                                        update_app(cx, |app, cx| app.friends_leave(&id, cx));
                                    }),
                            ),
                    ),
            )
            .child(members);
        if let Some(offer) = shown {
            let link = offer.link.to_invite_link();
            let copy = link.clone();
            card = card.child(
                h_flex()
                    .gap_4()
                    .items_start()
                    .anchor("friends.invite")
                    .child(crate::qr::qr_code(&link, px(104.), cx))
                    .child(
                        v_flex()
                            .gap_2()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .child(SharedString::from(t!("settings.friends.invite_shown"))),
                            )
                            .child(code_box(&offer.code, cx))
                            .child(long_note(t!("settings.friends.invite_expires"), cx))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        Button::new("friends-copy-link")
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
                                        Button::new("friends-hide-code")
                                            .small()
                                            .ghost()
                                            .label(t!("common.cancel"))
                                            .on_click(|_, _, cx| {
                                                update_app(cx, |app, cx| {
                                                    app.friends_cancel_invite(cx)
                                                });
                                            }),
                                    ),
                            ),
                    ),
            );
        }
        groups = groups.child(card);
    }
    let group_input = panel.friends_ui.group.clone();
    let code_input = panel.friends_ui.code.clone();
    let looking = join == PairStatus::Looking;
    groups = groups
        .child(
            div()
                .anchor("friends.create")
                .text_sm()
                .child(SharedString::from(t!("settings.friends.create"))),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&panel.friends_ui.group).small()),
                )
                .child(
                    Button::new("friends-create")
                        .small()
                        .primary()
                        .label(t!("settings.friends.create_button"))
                        .on_click(move |_, window, cx| {
                            let name = group_input.read(cx).value().to_string();
                            create_group(&name, window, cx);
                        }),
                ),
        )
        .child(
            div()
                .anchor("friends.join")
                .text_sm()
                .child(SharedString::from(t!("settings.friends.join"))),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&panel.friends_ui.code).small()),
                )
                .child(
                    Button::new("friends-join")
                        .small()
                        .outline()
                        .label(t!("settings.friends.join_button"))
                        .loading(looking)
                        .on_click(move |_, _, cx| {
                            let value = code_input.read(cx).value().to_string();
                            update_app(cx, |app, cx| app.friends_join(&value, cx));
                        }),
                ),
        )
        .child(note(t!("settings.friends.join_note"), cx))
        .children(match &join {
            PairStatus::Idle => None,
            PairStatus::Looking => Some(status_line(t!("settings.friends.joining").into(), muted)),
            PairStatus::Failed(e) => Some(status_line(e.clone().into(), danger)),
            PairStatus::Paired(name) => Some(status_line(
                t!("settings.friends.joined", group = name).into(),
                success,
            )),
        });

    // The recovery phrase: shown on request; restoring replaces this identity.
    let phrase = panel
        .friends_ui
        .show_phrase
        .then(|| panel.secret(Secret::FriendKey))
        .flatten()
        .and_then(|k| Identity::from_secret(&k))
        .map(|i| i.recovery_phrase());
    let phrase_input = panel.friends_ui.phrase.clone();
    let recovery = group_at("friends.recovery", t!("settings.friends.recovery"), cx)
        .child(long_note(t!("settings.friends.recovery_note"), cx))
        .child(match phrase {
            Some(phrase) => {
                let copy = phrase.clone();
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .px_3()
                            .py_2()
                            .rounded(crate::theme::radius(8.))
                            .bg(cx.theme().secondary)
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(SharedString::from(phrase)),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("friends-copy-phrase")
                                    .small()
                                    .outline()
                                    .label(t!("settings.friends.copy_phrase"))
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            copy.clone(),
                                        ));
                                    }),
                            )
                            .child(
                                Button::new("friends-hide-phrase")
                                    .small()
                                    .ghost()
                                    .label(t!("settings.friends.hide_phrase"))
                                    .on_click(
                                        cx.listener(|p, _, _, cx| {
                                            p.show_recovery_phrase(false, cx)
                                        }),
                                    ),
                            ),
                    )
                    .into_any_element()
            }
            None => h_flex()
                .child(
                    Button::new("friends-show-phrase")
                        .small()
                        .outline()
                        .label(t!("settings.friends.show_phrase"))
                        .on_click(cx.listener(|p, _, _, cx| p.show_recovery_phrase(true, cx))),
                )
                .into_any_element(),
        })
        .child(
            div()
                .anchor("friends.restore")
                .text_sm()
                .child(SharedString::from(t!("settings.friends.restore"))),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&panel.friends_ui.phrase).small()),
                )
                .child(
                    Button::new("friends-restore")
                        .small()
                        .outline()
                        .label(t!("settings.friends.restore_button"))
                        .on_click(move |_, window, cx| {
                            let value = phrase_input.read(cx).value().to_string();
                            let done =
                                update_app(cx, |app, cx| app.friends_restore(&value, window, cx))
                                    .unwrap_or(false);
                            if done {
                                phrase_input.update(cx, |i, cx| i.set_value("", window, cx));
                            }
                        }),
                ),
        )
        .child(long_note(t!("settings.friends.restore_note"), cx));

    v_flex()
        .gap_5()
        .child(intro)
        .child(you)
        .child(groups)
        .child(recovery)
        .into_any_element()
}
