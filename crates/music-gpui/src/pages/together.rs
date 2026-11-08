//! Listen together (Music Together on the LAN or through a relay). Idle: "Start
//! listening together" (the sheet that asks how: mode, who can join, connection) and
//! "Join a session" (one field for a room code or a link), and the friends card
//! (friend groups' live sessions). In a session: the mode and what you may do, the
//! invite (room code and link, or "only this group"), the room's settings, people,
//! the shared queue with who queued what, reactions, suggestions and the activity.

use std::time::Duration;

use gpui::{
    AnyElement, AppContext as _, ClipboardItem, Context, Entity, FontWeight, IntoElement,
    ParentElement as _, Render, ScrollHandle, SharedString, Styled as _, Task, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    spinner::Spinner,
    switch::Switch,
    tag::Tag,
    v_flex,
};
use music_core::together::{
    Participant, REACTIONS, RoomSettings, SessionMode, Suggestion, display_code,
};

use super::PageView;
use crate::app::{MusicApp, read_app, update_app};
use crate::components::{page_scroll, page_title, section_header};
use crate::icons::Lucide;
use crate::integrations::together::{
    Role, SentState, SentSuggestion, mode_name, relay_hint, relay_host_name,
};
use crate::integrations::together_ui::{
    avatar, mode_icon, permission_line, reaction_color, reaction_icon, reaction_label,
};
use crate::router::Route;
use crate::settings::SectionId;

pub struct TogetherPage {
    scroll: ScrollHandle,
    join: Entity<InputState>,
    _tick: Task<()>,
}

impl TogetherPage {
    /// `last` pre-fills the join field (the last link joined).
    pub fn build(last: String, window: &mut Window, cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|cx: &mut Context<Self>| {
            let join = cx.new(|cx| InputState::new(window, cx).default_value(last));
            crate::i18n::track_placeholder(&join, k!("together.join.any_placeholder"), window, cx);
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
                join,
                _tick: tick,
            }
        });
        PageView::new(page, Some(scroll))
    }
}

/// What the page shows, read from the app before drawing.
#[derive(Default)]
struct View {
    /// 0 idle, 1 joining / reconnecting, 2 in a session.
    state: u8,
    joining: String,
    hosting: bool,
    mode: SessionMode,
    room: RoomSettings,
    host_name: String,
    line: String,
    pending: bool,
    can_switch: bool,
    can_remove: bool,
    invite: Option<Invite>,
    group: Option<(String, String)>,
    transferring: Option<String>,
    leaving: bool,
    people: Vec<Participant>,
    me: Option<String>,
    /// Up next: title, artists, who queued it, queued (not the list).
    queue: Vec<(String, String, Option<String>, bool)>,
    now: Option<(String, String)>,
    context: Option<String>,
    waiting: Vec<Suggestion>,
    sent: Vec<SentSuggestion>,
    clock: Option<(i64, u64)>,
    log: Vec<String>,
    error: Option<String>,
}

#[derive(Default, Clone)]
struct Invite {
    link: String,
    compact: String,
    host: String,
    port: u16,
    relayed: bool,
    code: Option<String>,
    notice: Option<String>,
}

fn card(cx: &gpui::App) -> gpui::Div {
    v_flex()
        .gap_3()
        .p_5()
        .rounded(crate::theme::radius(12.))
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

fn title(text: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_lg()
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.into())
}

fn read_view(app: &MusicApp, cx: &gpui::App) -> View {
    let t = &app.integrations.together;
    let mut view = View {
        error: t.error.clone(),
        log: t.log.iter().rev().take(10).cloned().collect(),
        ..View::default()
    };
    match &t.role {
        Role::Idle => return view,
        Role::Joining(host) => {
            view.state = 1;
            view.joining = t!("together.connecting", host = host);
            return view;
        }
        Role::Recovering(_) => {
            view.state = 1;
            view.joining = t!("together.can.reconnecting").into();
            return view;
        }
        _ => view.state = 2,
    }
    let p = t.permissions().unwrap_or_default();
    view.hosting = matches!(t.role, Role::Hosting(_));
    view.mode = t.mode().unwrap_or_default();
    view.room = t.room_settings().unwrap_or_default();
    view.host_name = t.host_name().unwrap_or_default();
    view.line = permission_line(app);
    view.can_switch = p.switch_mode;
    view.can_remove = p.moderate;
    view.people = t.participants();
    view.me = t.me();
    view.waiting = app.together_suggestions();
    view.group = t.group().and_then(|g| {
        let groups = &app.settings.read(cx).settings().friends.groups;
        groups
            .iter()
            .find(|x| x.id() == g)
            .map(|x| (g.to_owned(), x.name().to_owned()))
    });
    match &t.role {
        Role::Hosting(h) => {
            view.invite = Some(Invite {
                link: h.link(),
                compact: h.join.to_compact(),
                host: h.join.host.clone(),
                port: h.join.port,
                relayed: h.join.is_relay(),
                code: h.code.clone(),
                notice: h.notice.clone(),
            });
            view.transferring = h.transferring.clone();
            view.leaving = h.leaving.is_some();
        }
        Role::Guest(g) => {
            view.pending = g.pending || g.taking_over;
            view.sent = g.suggestions.iter().rev().cloned().collect();
            view.clock = Some((g.client.clock().offset_ms, g.client.clock().rtt_ms));
        }
        _ => {}
    }
    view.now = app
        .queue
        .current()
        .map(|t| (t.title.clone(), t.artists.clone()));
    view.context = app
        .queue
        .context()
        .map(|c| c.name.clone())
        .filter(|n| !n.is_empty());
    view.queue = app
        .queue
        .upcoming()
        .iter()
        .take(40)
        .map(|i| {
            (
                i.track.title.clone(),
                i.track.artists.clone(),
                t.added_by(i.uid).map(str::to_owned),
                i.source == crate::queue::Source::User,
            )
        })
        .collect();
    view
}

impl Render for TogetherPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let data = read_app(cx, |app, cx| {
            (
                read_view(app, cx),
                app.settings.read(cx).settings().together.clone(),
                app.friends_panel(cx),
            )
        });
        let Some((view, prefs, friends)) = data else {
            return div().into_any_element();
        };
        let danger = cx.theme().danger;
        let mut content = v_flex().gap_6().child(page_title(
            t!("nav.together"),
            Some(t!("together.page.subtitle").into()),
            cx,
        ));
        if let Some(e) = &view.error {
            content = content.child(div().text_sm().text_color(danger).child(e.clone()));
        }
        match view.state {
            0 => {
                content = content.child(self.idle_cards(&prefs, cx));
                content = content.child(super::friends_panel::render(friends, cx));
            }
            1 => {
                content = content.child(
                    card(cx)
                        .child(
                            h_flex()
                                .gap_3()
                                .child(Spinner::new())
                                .child(SharedString::from(view.joining.clone())),
                        )
                        .child(
                            h_flex().child(
                                Button::new("tg-cancel")
                                    .small()
                                    .ghost()
                                    .label(t!("common.cancel"))
                                    .on_click(|_, _, cx| {
                                        update_app(cx, |app, cx| app.together_stop(cx));
                                    }),
                            ),
                        ),
                );
            }
            _ => {
                content = content.child(session_card(&view, cx));
                if view.hosting {
                    content = content.child(invite_card(&view, cx));
                    content = content.child(room_card(&view, cx));
                    if !view.waiting.is_empty() {
                        content = content.child(suggestions_card(&view.waiting, cx));
                    }
                }
                if !view.sent.is_empty() && view.mode != SessionMode::Friends {
                    content = content.child(sent_card(&view.sent, cx));
                }
                if !view.pending {
                    content = content.child(queue_section(&view, cx));
                }
                content = content.child(people_section(&view, cx));
                if view.group.is_some() || !friends.sessions.is_empty() {
                    content = content.child(super::friends_panel::render(friends, cx));
                }
            }
        }
        if !view.log.is_empty() {
            content = content.child(
                v_flex()
                    .gap_1()
                    .child(section_header(t!("together.activity"), None, None, cx))
                    .children(view.log.iter().map(|line| muted(line.clone(), cx))),
            );
        }
        if let Some((offset, rtt)) = view.clock {
            content = content.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(t!(
                        "together.guest.clock",
                        offset = offset,
                        rtt = rtt
                    ))),
            );
        }
        page_scroll("together-page", &self.scroll, content).into_any_element()
    }
}

impl TogetherPage {
    /// Idle: start (the sheet asks how) and join (a code or a link).
    fn idle_cards(
        &self,
        prefs: &crate::integrations::config::TogetherPrefs,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let accent = crate::theme::AppColors::get(cx).accent;
        let relay_name = relay_hint(&prefs.relay_url)
            .as_deref()
            .and_then(relay_host_name)
            .unwrap_or_default();
        let network = match prefs.host_mode {
            crate::integrations::config::HostMode::Relay => {
                t!("together.host.mode_relay_note", relay = relay_name)
            }
            crate::integrations::config::HostMode::Direct => {
                t!("together.host.mode_direct_note").to_owned()
            }
        };
        let start = card(cx)
            .flex_1()
            .min_w(px(300.))
            .child(
                h_flex()
                    .gap_2()
                    .child(Icon::new(Lucide::Radio).size(px(20.)).text_color(accent))
                    .child(title(t!("together.start.card_title"))),
            )
            .child(muted(t!("together.start.card_text"), cx))
            .child(v_flex().gap_1().children(SessionMode::ALL.map(|mode| {
                h_flex()
                    .gap_2()
                    .child(Icon::new(mode_icon(mode)).size(px(14.)).text_color(accent))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(mode_name(mode)),
                    )
                    .child(muted(
                        crate::integrations::together_ui::mode_description(mode),
                        cx,
                    ))
            })))
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("tg-host")
                            .primary()
                            .icon(Lucide::Radio)
                            .label(t!("together.start.open"))
                            .on_click(|_, window, cx| {
                                update_app(cx, |app, cx| {
                                    app.together_open_start_sheet(None, window, cx)
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
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(network)),
            );
        let join_input = self.join.clone();
        let join = card(cx)
            .flex_1()
            .min_w(px(300.))
            .child(title(t!("together.join.title")))
            .child(muted(t!("together.join.any_hint"), cx))
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(Input::new(&self.join)))
                    .child(
                        Button::new("tg-join")
                            .primary()
                            .label(t!("together.join.button"))
                            .on_click(move |_, window, cx| {
                                let raw = join_input.read(cx).value().to_string();
                                update_app(cx, |app, cx| app.together_join(&raw, window, cx));
                            }),
                    ),
            );
        h_flex()
            .gap_4()
            .flex_wrap()
            .items_start()
            .child(start)
            .child(join)
            .into_any_element()
    }
}

/// The mode, what you may do, and leave / end.
fn session_card(view: &View, cx: &gpui::App) -> AnyElement {
    let accent = crate::theme::AppColors::get(cx).accent;
    let friends = view.mode == SessionMode::Friends;
    let heading = match (view.mode, view.hosting) {
        (SessionMode::Friends, _) => t!("together.session.friends").to_owned(),
        (SessionMode::Host, true) => t!("together.session.hosting").to_owned(),
        (SessionMode::Dj, true) => t!("together.session.dj_you").to_owned(),
        (SessionMode::Host, false) => {
            t!("together.guest.listening_with", name = view.host_name)
        }
        (SessionMode::Dj, false) => t!("together.session.dj", name = view.host_name),
    };
    let badge = h_flex()
        .gap_1p5()
        .px_2()
        .py_0p5()
        .rounded_full()
        .bg(accent.opacity(0.15))
        .child(
            Icon::new(mode_icon(view.mode))
                .size(px(14.))
                .text_color(accent),
        )
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(accent)
                .child(SharedString::from(t!(
                    "together.mode.badge",
                    mode = mode_name(view.mode)
                ))),
        );
    let ends = view.hosting && !friends;
    let leave = Button::new("tg-end")
        .small()
        .when(ends, |b| b.danger())
        .when(!ends, |b| b.outline())
        .icon(Lucide::LogOut)
        .label(if ends {
            t!("together.hosting.end")
        } else {
            t!("together.guest.leave")
        })
        .loading(view.leaving)
        .disabled(view.leaving)
        .on_click(|_, _, cx| {
            update_app(cx, |app, cx| app.together_leave(cx));
        });
    let mut switch = h_flex().gap_1().flex_wrap();
    if view.can_switch {
        for mode in SessionMode::ALL {
            let selected = mode == view.mode;
            switch = switch.child(
                Button::new(SharedString::from(format!("tg-mode-{}", mode.key())))
                    .small()
                    .map(|b| if selected { b.primary() } else { b.outline() })
                    .icon(mode_icon(mode))
                    .label(mode_name(mode))
                    .on_click(move |_, window, cx| {
                        if !selected {
                            update_app(cx, |app, cx| app.together_confirm_mode(mode, window, cx));
                        }
                    }),
            );
        }
    }
    let native = if view.can_control_hint() {
        t!("together.session.native_hint").to_owned()
    } else {
        t!("together.session.native_hint_locked", name = view.host_name)
    };
    let mut card = card(cx)
        .child(
            h_flex()
                .gap_3()
                .justify_between()
                .child(
                    v_flex()
                        .gap_1()
                        .min_w_0()
                        .child(badge)
                        .child(title(heading)),
                )
                .child(leave),
        )
        .child(muted(view.line.clone(), cx));
    if view.pending {
        card = card.child(
            h_flex()
                .gap_2()
                .child(Spinner::new().small())
                .child(muted(t!("together.status.waiting_approval"), cx)),
        );
    } else {
        card = card
            .when_some(view.now.clone(), |card, (song, artists)| {
                card.child(
                    v_flex()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(song))
                        .child(muted(artists, cx)),
                )
            })
            .when(view.now.is_none(), |card| {
                card.child(muted(t!("together.guest.nothing_playing"), cx))
            })
            .child(h_flex().gap_1().children(REACTIONS.iter().map(|emoji| {
                Button::new(SharedString::from(format!("tg-react-{emoji}")))
                    .ghost()
                    .small()
                    .icon(
                        Icon::new(reaction_icon(emoji))
                            .size(px(18.))
                            .text_color(reaction_color(emoji)),
                    )
                    .tooltip(reaction_label(emoji))
                    .disabled(view.now.is_none())
                    .on_click(move |_, _, cx| {
                        update_app(cx, |app, cx| app.together_react(emoji, cx));
                    })
            })))
            .child(muted(native, cx));
    }
    if view.can_switch {
        card = card.child(
            v_flex()
                .gap_1p5()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().muted_foreground)
                        .child(t!("together.panel.change_mode")),
                )
                .child(switch),
        );
    }
    if let Some(name) = &view.transferring {
        card = card.child(
            h_flex()
                .gap_2()
                .child(Spinner::new().small())
                .child(muted(t!("together.log.handing_over", name = name), cx)),
        );
    }
    card.into_any_element()
}

impl View {
    /// This person's buttons and keys act on the session (not just follow it).
    fn can_control_hint(&self) -> bool {
        self.hosting
            || self.mode == SessionMode::Friends
            || (self.mode == SessionMode::Host && self.room.allow_guests_to_control_playback)
    }
}

/// Host: the room code and link (or "only this group").
fn invite_card(view: &View, cx: &gpui::App) -> AnyElement {
    let Some(invite) = view.invite.clone() else {
        return div().into_any_element();
    };
    let theme = cx.theme();
    let mut card = card(cx).child(title(t!("together.invite.title")));
    if let Some(notice) = &invite.notice {
        card = card.child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.warning)
                .child(notice.clone()),
        );
    }
    if let Some((group, name)) = view.group.clone() {
        return card
            .child(muted(t!("together.invite.group_only", group = name), cx))
            .child(
                h_flex().child(
                    Button::new("tg-invite-group")
                        .small()
                        .outline()
                        .icon(Lucide::UsersRound)
                        .label(t!("together.invite.again"))
                        .on_click(move |_, window, cx| {
                            let group = group.clone();
                            update_app(cx, |app, cx| {
                                app.friends_listen_together(&group, window, cx)
                            });
                        }),
                ),
            )
            .into_any_element();
    }
    let Invite {
        link,
        compact,
        host,
        port,
        relayed,
        code,
        ..
    } = invite;
    let has_code = code.is_some();
    if let Some(code) = code {
        let shown = display_code(&code);
        let copy = shown.clone();
        card = card.child(
            v_flex()
                .gap_1()
                .child(muted(t!("together.hosting.code_label"), cx))
                .child(
                    h_flex()
                        .gap_3()
                        .flex_wrap()
                        .child(div().text_3xl().font_weight(FontWeight::BOLD).child(shown))
                        .child(
                            Button::new("tg-copy-room-code")
                                .small()
                                .outline()
                                .icon(Lucide::Copy)
                                .label(t!("together.hosting.copy_code"))
                                .on_click(move |_, window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                                    crate::app::notify(
                                        window,
                                        cx,
                                        t!("together.hosting.code_copied"),
                                    );
                                }),
                        ),
                )
                .child(muted(t!("together.hosting.code_hint"), cx)),
        );
    }
    let (copy_link, copy_compact) = (link.clone(), compact);
    card.child(muted(
        if relayed {
            t!("together.hosting.share_relay", host = host)
        } else {
            t!("together.hosting.share_lan", host = host, port = port)
        },
        cx,
    ))
    .child(
        h_flex()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .p_2()
                    .rounded(crate::theme::radius(6.))
                    .bg(theme.background)
                    .text_sm()
                    .truncate()
                    .child(link),
            )
            .child(
                Button::new("tg-copy")
                    .small()
                    .outline()
                    .icon(Lucide::Link)
                    .label(t!("together.hosting.copy_link"))
                    .on_click(move |_, window, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_link.clone()));
                        crate::app::notify(window, cx, t!("together.hosting.link_copied"));
                    }),
            )
            .when(!has_code, |row| {
                row.child(
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
                            cx.write_to_clipboard(ClipboardItem::new_string(copy_compact.clone()));
                        }),
                )
            }),
    )
    .when(relayed, |card| {
        card.child(muted(t!("together.hosting.relay_privacy"), cx))
    })
    .into_any_element()
}

/// Host: the session's settings (only those the mode leaves open).
fn room_card(view: &View, cx: &gpui::App) -> AnyElement {
    let room = view.room;
    let switch =
        |id: &'static str, label: &'static str, on: bool, set: fn(&mut RoomSettings, bool)| {
            Switch::new(id)
                .checked(on)
                .label(label)
                .on_click(move |checked: &bool, _, cx| {
                    let on = *checked;
                    update_app(cx, |app, cx| app.together_set_room(|r| set(r, on), cx));
                })
        };
    let mut switches = h_flex().gap_6().flex_wrap();
    if view.mode == SessionMode::Host {
        switches = switches
            .child(switch(
                "tg-allow-add",
                t!("together.room.allow_add"),
                room.allow_guests_to_add_tracks,
                |r, on| r.allow_guests_to_add_tracks = on,
            ))
            .child(switch(
                "tg-allow-control",
                t!("together.room.allow_control"),
                room.allow_guests_to_control_playback,
                |r, on| r.allow_guests_to_control_playback = on,
            ));
    }
    switches = switches.child(switch(
        "tg-approval",
        t!("together.room.approve"),
        room.require_host_approval_to_join,
        |r, on| r.require_host_approval_to_join = on,
    ));
    if view.mode != SessionMode::Friends {
        switches = switches.child(switch(
            "tg-approve-suggestions",
            t!("together.room.approve_suggestions"),
            room.require_approval_for_suggestions,
            |r, on| r.require_approval_for_suggestions = on,
        ));
    }
    let note = match view.mode {
        SessionMode::Friends => Some(t!("together.room.friends_note")),
        SessionMode::Dj => Some(t!("together.room.dj_note")),
        SessionMode::Host => None,
    };
    card(cx)
        .child(title(t!("together.room.title")))
        .children(note.map(|n| muted(n, cx)))
        .child(switches)
        .into_any_element()
}

/// The shared queue: up next, with who queued each song.
fn queue_section(view: &View, cx: &gpui::App) -> AnyElement {
    let muted_color = cx.theme().muted_foreground;
    let mut rows = v_flex().gap_1();
    if view.queue.is_empty() {
        rows = rows.child(muted(t!("together.queue.empty"), cx));
    }
    for (title_text, artists, by, queued) in &view.queue {
        rows = rows.child(
            h_flex()
                .gap_3()
                .py_1()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().truncate().child(title_text.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted_color)
                                .truncate()
                                .child(artists.clone()),
                        ),
                )
                .when_some(by.clone().filter(|_| *queued), |row, by| {
                    row.child(
                        h_flex()
                            .gap_1p5()
                            .flex_none()
                            .child(avatar(&by, 20., None))
                            .child(div().text_xs().text_color(muted_color).child(
                                SharedString::from(t!("together.queue.added_by", name = by)),
                            )),
                    )
                }),
        );
    }
    v_flex()
        .gap_2()
        .child(section_header(
            tp!("together.queue.up_next", view.queue.len()),
            view.context.clone().map(SharedString::from),
            None,
            cx,
        ))
        .child(rows)
        .into_any_element()
}

/// Who is in the session, with what the host (or anyone, with Friends) can do.
fn people_section(view: &View, cx: &gpui::App) -> AnyElement {
    let friends = view.mode == SessionMode::Friends;
    let busy = view.transferring.is_some();
    let rows = view.people.iter().enumerate().map(|(ix, p)| {
        let mine = view.me.as_deref() == Some(p.id.as_str());
        let id = p.id.clone();
        let mut row = h_flex()
            .gap_3()
            .py_1()
            .child(avatar(&p.name, 30., None))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(if mine {
                        t!("together.panel.you_suffix", name = p.name)
                    } else {
                        p.name.clone()
                    })),
            );
        if p.is_host && !friends {
            row = row.child(
                Tag::primary()
                    .small()
                    .child(if view.mode == SessionMode::Dj {
                        t!("together.tag.dj")
                    } else {
                        t!("together.tag.host")
                    }),
            );
        }
        if p.is_pending {
            row = row.child(Tag::warning().small().child(t!("together.tag.waiting")));
        }
        if view.hosting && !p.is_host {
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
                let (k, b, t) = (id.clone(), id.clone(), id.clone());
                // Nobody hosts a Friends session: no "Make host" there.
                if !friends {
                    row = row.child(
                        Button::new(("tg-make-host", ix))
                            .xsmall()
                            .ghost()
                            .label(t!("together.people.make_host"))
                            .tooltip(t!("together.people.make_host_tooltip"))
                            .disabled(busy)
                            .on_click(move |_, _, cx| {
                                update_app(cx, |app, cx| app.together_transfer(&t, cx));
                            }),
                    );
                }
                row = row
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
        } else if view.can_remove && !mine && !p.is_host && !p.is_pending {
            let k = id.clone();
            row = row.child(
                Button::new(("tg-kick", ix))
                    .xsmall()
                    .ghost()
                    .label(t!("together.people.remove"))
                    .on_click(move |_, _, cx| {
                        update_app(cx, |app, cx| app.together_kick(&k, false, cx));
                    }),
            );
        }
        row.into_any_element()
    });
    v_flex()
        .gap_2()
        .child(section_header(
            tp!("together.people.title", view.people.len()),
            None,
            None,
            cx,
        ))
        .children(rows)
        .into_any_element()
}

/// Host: guests' suggestions waiting for Approve / Reject.
fn suggestions_card(waiting: &[Suggestion], cx: &gpui::App) -> AnyElement {
    let rows = waiting.iter().map(|s| {
        let (approve, reject) = (s.id.clone(), s.id.clone());
        h_flex()
            .gap_3()
            .py_1()
            .child(avatar(&s.participant.name, 24., None))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .child(s.track.title.clone()),
                    )
                    .child(muted(
                        t!(
                            "together.suggestions.from",
                            artists = s.track.artists.join(", "),
                            name = s.participant.name
                        ),
                        cx,
                    )),
            )
            .child(
                Button::new(SharedString::from(format!("tg-sug-ok-{}", s.id)))
                    .small()
                    .primary()
                    .icon(IconName::Check)
                    .label(t!("together.suggestions.approve"))
                    .on_click(move |_, window, cx| {
                        let id = approve.clone();
                        update_app(cx, |app, cx| {
                            app.together_decide_suggestion(&id, true, window, cx)
                        });
                    }),
            )
            .child(
                Button::new(SharedString::from(format!("tg-sug-no-{}", s.id)))
                    .small()
                    .ghost()
                    .icon(IconName::Close)
                    .label(t!("together.suggestions.reject"))
                    .on_click(move |_, window, cx| {
                        let id = reject.clone();
                        update_app(cx, |app, cx| {
                            app.together_decide_suggestion(&id, false, window, cx)
                        });
                    }),
            )
    });
    card(cx)
        .child(title(tp!("together.suggestions.title", waiting.len())))
        .child(muted(t!("together.suggestions.hint"), cx))
        .children(rows)
        .into_any_element()
}

/// Guest: the songs it suggested and what the host decided.
fn sent_card(sent: &[SentSuggestion], cx: &gpui::App) -> AnyElement {
    let theme = cx.theme();
    let (ok, bad, muted_color) = (theme.success, theme.danger, theme.muted_foreground);
    let rows = sent.iter().map(|s| {
        let (label, color) = match &s.state {
            SentState::Sent => (t!("together.suggestions.sent").to_owned(), muted_color),
            SentState::Pending => (t!("together.suggestions.pending").to_owned(), muted_color),
            SentState::Approved => (t!("together.suggestions.approved").to_owned(), ok),
            SentState::Rejected(None) => (t!("together.suggestions.rejected").to_owned(), bad),
            SentState::Rejected(Some(reason)) => (
                t!("together.suggestions.rejected_because", reason = reason),
                bad,
            ),
        };
        h_flex()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .child(s.track.title.clone()),
            )
            .child(div().flex_none().text_sm().text_color(color).child(label))
    });
    card(cx)
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(t!("together.suggestions.yours")),
        )
        .children(rows)
        .into_any_element()
}
