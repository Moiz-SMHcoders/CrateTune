//! Listen together (Music Together on the LAN or through a relay): host or join a
//! session, the join link, participants (approve / remove / make host), room
//! settings, the room's queue and requests to the host.

use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AnyElement, AppContext as _, ClipboardItem, Context, Entity, FontWeight, IntoElement,
    ParentElement as _, Render, ScrollHandle, SharedString, Styled as _, Task, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    spinner::Spinner,
    switch::Switch,
    tag::Tag,
    v_flex,
};
use music_core::Track;
use music_core::together::{ControlAction, Participant, RoomState, TogetherTrack};

use super::PageView;
use crate::app::{MusicApp, read_app, update_app};
use crate::components::track_row::TrackRow;
use crate::components::{page_scroll, page_title, section_header};
use crate::icons::Lucide;
use crate::integrations::together::Role;
use crate::router::Route;
use crate::settings::SectionId;

pub struct TogetherPage {
    scroll: ScrollHandle,
    link: Entity<InputState>,
    _tick: Task<()>,
}

impl TogetherPage {
    /// `last` pre-fills the join field (the last link joined).
    pub fn build(last: String, window: &mut Window, cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|cx: &mut Context<Self>| {
            let link = cx.new(|cx| InputState::new(window, cx).default_value(last));
            crate::i18n::track_placeholder(&link, k!("together.join.placeholder"), window, cx);
            let executor = cx.background_executor().clone();
            // Participants / room change on the app's poll; refresh twice a second.
            let tick = cx.spawn(async move |this, cx| {
                loop {
                    executor.timer(Duration::from_millis(500)).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            });
            Self {
                scroll: scroll.clone(),
                link,
                _tick: tick,
            }
        });
        PageView::new(page, Some(scroll))
    }
}

fn card(cx: &gpui::App) -> gpui::Div {
    v_flex()
        .gap_3()
        .p_5()
        .rounded(px(12.))
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().secondary)
}

fn muted(text: impl Into<SharedString>, cx: &gpui::App) -> gpui::Div {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

fn participant_row(
    p: &Participant,
    hosting: bool,
    transferring: bool,
    ix: usize,
    cx: &gpui::App,
) -> AnyElement {
    let id = p.id.clone();
    let mut row = h_flex()
        .gap_3()
        .py_1()
        .child(
            div()
                .size(px(30.))
                .flex_none()
                .rounded_full()
                .bg(cx.theme().accent)
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(
                    p.name
                        .chars()
                        .next()
                        .unwrap_or('?')
                        .to_uppercase()
                        .to_string(),
                ),
        )
        .child(div().flex_1().min_w_0().truncate().child(p.name.clone()));
    if p.is_host {
        row = row.child(Tag::primary().small().child(t!("together.tag.host")));
    }
    if p.is_pending {
        row = row.child(Tag::warning().small().child(t!("together.tag.waiting")));
    }
    if hosting && !p.is_host {
        if p.is_pending {
            let (a, d) = (id.clone(), id.clone());
            row = row
                .child(
                    Button::new(("tg-approve", ix))
                        .xsmall()
                        .primary()
                        .label(t!("together.people.let_in"))
                        .on_click(move |_, _, cx| {
                            update_app(cx, |app, cx| app.together_approve(&a, true, cx));
                        }),
                )
                .child(
                    Button::new(("tg-deny", ix))
                        .xsmall()
                        .ghost()
                        .label(t!("together.people.decline"))
                        .on_click(move |_, _, cx| {
                            update_app(cx, |app, cx| app.together_approve(&d, false, cx));
                        }),
                );
        } else {
            let (k, b, t) = (id.clone(), id.clone(), id);
            row = row
                .child(
                    Button::new(("tg-make-host", ix))
                        .xsmall()
                        .ghost()
                        .label(t!("together.people.make_host"))
                        .tooltip(t!("together.people.make_host_tooltip"))
                        .disabled(transferring)
                        .on_click(move |_, _, cx| {
                            update_app(cx, |app, cx| app.together_transfer(&t, cx));
                        }),
                )
                .child(
                    Button::new(("tg-kick", ix))
                        .xsmall()
                        .ghost()
                        .label(t!("together.people.remove"))
                        .on_click(move |_, _, cx| {
                            update_app(cx, |app, cx| app.together_kick(&k, false, cx));
                        }),
                )
                .child(
                    Button::new(("tg-ban", ix))
                        .xsmall()
                        .ghost()
                        .label(t!("together.people.ban"))
                        .on_click(move |_, _, cx| {
                            update_app(cx, |app, cx| app.together_kick(&b, true, cx));
                        }),
                );
        }
    }
    row.into_any_element()
}

fn room_queue(room: &RoomState, cx: &gpui::App) -> AnyElement {
    let tracks: Rc<Vec<Track>> = Rc::new(room.queue.iter().map(TogetherTrack::to_track).collect());
    let rows: Vec<AnyElement> = (0..tracks.len().min(50))
        .map(|ix| {
            TrackRow::new("tg-room", tracks.clone(), ix)
                .active(ix == room.current_index)
                .into_any_element()
        })
        .collect();
    v_flex()
        .gap_2()
        .child(section_header(
            t!("together.queue.title"),
            Some(tp!("common.songs", room.queue.len()).into()),
            None,
            cx,
        ))
        .child(v_flex().children(rows))
        .into_any_element()
}

impl Render for TogetherPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let data = read_app(cx, |app, cx| {
            let t = &app.integrations.together;
            let prefs = app.settings.read(cx).settings().together.clone();
            let role = match &t.role {
                Role::Idle => 0,
                Role::Joining(_) => 1,
                Role::Hosting(_) => 2,
                Role::Guest(_) => 3,
            };
            let link = match &t.role {
                Role::Hosting(h) => Some((
                    h.join.to_link(),
                    h.join.to_compact(),
                    h.join.host.clone(),
                    h.join.port,
                    h.join.is_relay(),
                    h.transferring.clone(),
                )),
                _ => None,
            };
            let guest = match &t.role {
                Role::Guest(g) => Some((
                    g.join.host.clone(),
                    g.pending || g.taking_over,
                    g.room.clone(),
                    g.client.clock(),
                )),
                Role::Joining(host) => Some((host.clone(), false, None, Default::default())),
                _ => None,
            };
            (
                role,
                link,
                guest,
                t.participants(),
                t.error.clone(),
                t.log.iter().rev().take(12).cloned().collect::<Vec<_>>(),
                prefs,
            )
        });
        let Some((role, link, guest, participants, error, log, prefs)) = data else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let danger = theme.danger;
        let mut content = v_flex().gap_6().child(page_title(
            t!("nav.together"),
            Some(t!("together.page.subtitle").into()),
            cx,
        ));
        if let Some(e) = &error {
            content = content.child(div().text_sm().text_color(danger).child(e.clone()));
        }

        match role {
            // ---- idle: host or join
            0 => {
                let link_input = self.link.clone();
                let host = card(cx)
                    .flex_1()
                    .min_w(px(300.))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(t!("together.host.title")),
                    )
                    .child(muted(host_summary(&prefs), cx))
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                Button::new("tg-host")
                                    .primary()
                                    .icon(Lucide::UsersRound)
                                    .label(t!("together.host.start"))
                                    .on_click(|_, window, cx| {
                                        update_app(cx, |app, cx| app.together_host(window, cx));
                                    }),
                            )
                            .child(
                                Button::new("tg-host-relay")
                                    .outline()
                                    .icon(Lucide::Globe)
                                    .label(t!("together.host.via_relay"))
                                    .tooltip(match prefs.relay() {
                                        Some(_) => t!("together.host.via_relay_tooltip"),
                                        None => t!("together.host.no_relay_tooltip"),
                                    })
                                    .disabled(prefs.relay().is_none())
                                    .on_click(|_, window, cx| {
                                        update_app(cx, |app, cx| {
                                            app.together_host_relay(window, cx, |_| {})
                                        });
                                    }),
                            )
                            .child(
                                Button::new("tg-settings")
                                    .ghost()
                                    .label(t!("together.host.settings"))
                                    .on_click(|_, window, cx| {
                                        update_app(cx, |app, cx| {
                                            app.navigate(
                                                Route::Settings {
                                                    section: SectionId::Together,
                                                },
                                                window,
                                                cx,
                                            )
                                        });
                                    }),
                            ),
                    );
                let join = card(cx)
                    .flex_1()
                    .min_w(px(300.))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(t!("together.join.title")),
                    )
                    .child(muted(t!("together.join.hint"), cx))
                    .child(Input::new(&self.link))
                    .child(
                        Button::new("tg-join")
                            .primary()
                            .label(t!("together.join.button"))
                            .on_click(move |_, window, cx| {
                                let link = link_input.read(cx).value().to_string();
                                update_app(cx, |app, cx| app.together_join(&link, window, cx));
                            }),
                    );
                content = content.child(
                    h_flex()
                        .gap_4()
                        .flex_wrap()
                        .items_start()
                        .child(host)
                        .child(join),
                );
            }
            // ---- joining
            1 => {
                let host = guest.as_ref().map(|g| g.0.clone()).unwrap_or_default();
                content = content.child(
                    card(cx)
                        .child(
                            h_flex()
                                .gap_3()
                                .child(Spinner::new())
                                .child(t!("together.connecting", host = host)),
                        )
                        .child(
                            Button::new("tg-cancel")
                                .small()
                                .ghost()
                                .label(t!("common.cancel"))
                                .on_click(|_, _, cx| {
                                    update_app(cx, |app, cx| app.together_leave(cx));
                                }),
                        ),
                );
            }
            // ---- hosting
            2 => {
                let (link, compact, host, port, relayed, transferring) = link.unwrap_or_default();
                let (copy_link, copy_compact) = (link.clone(), compact.clone());
                let guests = participants.iter().filter(|p| !p.is_host).count();
                content = content.child(
                    card(cx)
                        .child(
                            h_flex()
                                .justify_between()
                                .child(
                                    div()
                                        .text_lg()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(tp!("together.hosting.title", guests)),
                                )
                                .child(
                                    Button::new("tg-end")
                                        .small()
                                        .danger()
                                        .label(t!("together.hosting.end"))
                                        .on_click(|_, _, cx| {
                                            update_app(cx, |app, cx| app.together_leave(cx));
                                        }),
                                ),
                        )
                        .child(muted(
                            if relayed {
                                t!("together.hosting.share_relay", host = host)
                            } else {
                                t!("together.hosting.share_lan", host = host, port = port)
                            },
                            cx,
                        ))
                        .children(transferring.map(|name| {
                            h_flex()
                                .gap_2()
                                .child(Spinner::new().small())
                                .child(muted(t!("together.log.handing_over", name = name), cx))
                        }))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .p_2()
                                        .rounded(px(6.))
                                        .bg(theme.background)
                                        .text_sm()
                                        .truncate()
                                        .child(link),
                                )
                                .child(
                                    Button::new("tg-copy")
                                        .small()
                                        .outline()
                                        .label(t!("together.hosting.copy_link"))
                                        .on_click(move |_, window, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                copy_link.clone(),
                                            ));
                                            crate::app::notify(
                                                window,
                                                cx,
                                                t!("together.hosting.link_copied"),
                                            );
                                        }),
                                )
                                .child(
                                    Button::new("tg-copy-compact")
                                        .small()
                                        .ghost()
                                        .label(t!("together.hosting.copy_code"))
                                        .tooltip(if relayed {
                                            t!("together.hosting.copy_code_tooltip_relay")
                                        } else {
                                            t!("together.hosting.copy_code_tooltip")
                                        })
                                        .on_click(move |_, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                copy_compact.clone(),
                                            ));
                                        }),
                                ),
                        )
                        .child(
                            h_flex()
                                .gap_6()
                                .flex_wrap()
                                .child(setting_switch(
                                    "tg-allow-add",
                                    t!("together.room.allow_add"),
                                    prefs.allow_guests_to_add_tracks,
                                    |t, on| t.allow_guests_to_add_tracks = on,
                                ))
                                .child(setting_switch(
                                    "tg-allow-control",
                                    t!("together.room.allow_control"),
                                    prefs.allow_guests_to_control_playback,
                                    |t, on| t.allow_guests_to_control_playback = on,
                                ))
                                .child(setting_switch(
                                    "tg-approval",
                                    t!("together.room.approve"),
                                    prefs.require_host_approval,
                                    |t, on| t.require_host_approval = on,
                                )),
                        ),
                );
            }
            // ---- guest
            _ => {
                let (host, pending, room, clock) = guest.unwrap_or_default();
                let can_control = room
                    .as_ref()
                    .is_some_and(|r| r.settings.allow_guests_to_control_playback);
                let can_add = room
                    .as_ref()
                    .is_none_or(|r| r.settings.allow_guests_to_add_tracks);
                let playing = room.as_ref().is_some_and(|r| r.is_playing);
                let host_name = participants
                    .iter()
                    .find(|p| p.is_host)
                    .map_or(host.clone(), |p| p.name.clone());
                let control =
                    |id: &'static str, icon: Lucide, tip: &'static str, action: ControlAction| {
                        Button::new(id)
                            .small()
                            .ghost()
                            .icon(icon)
                            .tooltip(tip)
                            .disabled(!can_control)
                            .on_click(move |_, window, cx| {
                                let action = action.clone();
                                update_app(cx, |app, cx| app.together_request(action, window, cx));
                            })
                    };
                let now = room.as_ref().and_then(|r| r.current().cloned());
                let mut session = card(cx).child(
                    h_flex()
                        .justify_between()
                        .child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(t!("together.guest.listening_with", name = host_name)),
                        )
                        .child(
                            Button::new("tg-leave")
                                .small()
                                .outline()
                                .label(t!("together.guest.leave"))
                                .on_click(|_, _, cx| {
                                    update_app(cx, |app, cx| app.together_leave(cx));
                                }),
                        ),
                );
                if pending {
                    session = session.child(
                        h_flex()
                            .gap_2()
                            .child(Spinner::new().small())
                            .child(muted(t!("together.status.waiting_approval"), cx)),
                    );
                } else {
                    session = session
                        .child(match &now {
                            Some(t) => v_flex()
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(t.title.clone()),
                                )
                                .child(muted(t.artists.join(", "), cx)),
                            None => v_flex().child(muted(t!("together.guest.nothing_playing"), cx)),
                        })
                        .child(
                            h_flex()
                                .gap_1()
                                .flex_wrap()
                                .child(control(
                                    "tg-prev",
                                    Lucide::SkipBack,
                                    t!("together.guest.ask_previous"),
                                    ControlAction::SkipPrevious,
                                ))
                                .child(control(
                                    "tg-toggle",
                                    if playing { Lucide::Pause } else { Lucide::Play },
                                    t!("together.guest.ask_play_pause"),
                                    if playing {
                                        ControlAction::Pause
                                    } else {
                                        ControlAction::Play
                                    },
                                ))
                                .child(control(
                                    "tg-next",
                                    Lucide::SkipForward,
                                    t!("together.guest.ask_next"),
                                    ControlAction::SkipNext,
                                ))
                                .child(muted(
                                    if can_control {
                                        t!("together.guest.can_control")
                                    } else {
                                        t!("together.guest.host_controls")
                                    },
                                    cx,
                                )),
                        )
                        .child(muted(
                            if can_add {
                                t!("together.guest.suggest_hint")
                            } else {
                                t!("together.guest.no_suggestions")
                            },
                            cx,
                        ))
                        .child(muted(
                            t!(
                                "together.guest.clock",
                                offset = clock.offset_ms,
                                rtt = clock.rtt_ms
                            ),
                            cx,
                        ));
                }
                content = content.child(session);
                if let Some(room) = room.filter(|r| !r.queue.is_empty()) {
                    content = content.child(room_queue(&room, cx));
                }
            }
        }

        if !participants.is_empty() {
            let hosting = role == 2;
            let transferring = read_app(cx, |app, _| {
                matches!(&app.integrations.together.role, Role::Hosting(h) if h.transferring.is_some())
            })
            .unwrap_or(false);
            content = content.child(
                v_flex()
                    .gap_2()
                    .child(section_header(
                        tp!("together.people.title", participants.len()),
                        None,
                        None,
                        cx,
                    ))
                    .children(
                        participants
                            .iter()
                            .enumerate()
                            .map(|(ix, p)| participant_row(p, hosting, transferring, ix, cx)),
                    ),
            );
        }
        if !log.is_empty() {
            content = content.child(
                v_flex()
                    .gap_1()
                    .child(section_header(t!("together.activity"), None, None, cx))
                    .children(log.into_iter().map(|line| muted(line, cx))),
            );
        }
        page_scroll("together-page", &self.scroll, content).into_any_element()
    }
}

/// "Others follow what you play, as … on port …. Guests can … You approve each guest."
fn host_summary(prefs: &crate::integrations::config::TogetherPrefs) -> String {
    let guests = match (
        prefs.allow_guests_to_add_tracks,
        prefs.allow_guests_to_control_playback,
    ) {
        (true, true) => t!("together.host.guests_add_and_control"),
        (true, false) => t!("together.host.guests_add_only"),
        (false, true) => t!("together.host.guests_control_only"),
        (false, false) => t!("together.host.guests_neither"),
    };
    let approval = if prefs.require_host_approval {
        t!("together.host.approval")
    } else {
        ""
    };
    let intro = t!(
        "together.host.intro",
        name = prefs.name(),
        port = prefs.port
    );
    t!(
        "together.host.summary",
        intro = intro,
        guests = guests,
        approval = approval
    )
    .trim_end()
    .to_owned()
}

fn setting_switch(
    id: &'static str,
    label: &'static str,
    on: bool,
    set: fn(&mut crate::integrations::config::TogetherPrefs, bool),
) -> impl IntoElement {
    Switch::new(id)
        .checked(on)
        .label(label)
        .on_click(move |checked: &bool, _, cx| {
            let on = *checked;
            update_app(cx, |app, cx| {
                app.settings
                    .update(cx, |panel, cx| panel.set_together(|t| set(t, on), cx));
                app.together_broadcast(true, cx);
            });
        })
}
