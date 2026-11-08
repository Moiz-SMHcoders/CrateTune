//! What a Music Together session looks like outside its page: the listeners chip in
//! the player bar (stacked initials and a count, the session's mode, a quiet activity
//! note now and then) and the session panel it opens (mode badge and switch, who is
//! listening, reactions, invite, leave); reactions floating up over the bar; the
//! "Start listening together" sheet; the confirmation before a mode switch.
//!
//! Nothing here runs without a session: the chip and the overlay are `None`, the poll
//! part returns at once. Reactions animate only while on screen (and not at all with
//! Animations off; a short fade with Reduced).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    Anchor, Animation, AnimationExt as _, AnyElement, App, ClipboardItem, Context, FontWeight,
    Hsla, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, hsla, prelude::FluentBuilder as _,
    px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    popover::Popover,
    spinner::Spinner,
    v_flex,
};
use music_core::together::{NoteKind, Participant, REACTIONS, SessionMode, display_code};

use super::config::HostMode;
use super::together::{REACTION_SHOW, Role, StartOptions, mode_name};
use crate::app::{MusicApp, notify, read_app, update_app};
use crate::icons::Lucide;

/// Initials shown stacked in the chip.
const STACKED: usize = 3;

/// The icon of a mode (the badge, the chip, the sheet).
pub fn mode_icon(mode: SessionMode) -> Lucide {
    match mode {
        SessionMode::Friends => Lucide::Users,
        SessionMode::Host => Lucide::Crown,
        SessionMode::Dj => Lucide::Disc3,
    }
}

/// One line on what a mode means (the sheet, the mode picker).
pub fn mode_description(mode: SessionMode) -> &'static str {
    match mode {
        SessionMode::Friends => t!("together.mode.friends_desc"),
        SessionMode::Host => t!("together.mode.host_desc"),
        SessionMode::Dj => t!("together.mode.dj_desc"),
    }
}

/// The icon of a reaction.
pub fn reaction_icon(emoji: &str) -> Lucide {
    match emoji {
        "heart" => Lucide::Heart,
        "fire" => Lucide::Flame,
        "laugh" => Lucide::FaceGrinning,
        "rock" => Lucide::HandMetal,
        "party" => Lucide::PartyPopper,
        _ => Lucide::Star,
    }
}

pub fn reaction_label(emoji: &str) -> &'static str {
    match emoji {
        "heart" => t!("together.react.heart"),
        "fire" => t!("together.react.fire"),
        "laugh" => t!("together.react.laugh"),
        "rock" => t!("together.react.rock"),
        "party" => t!("together.react.party"),
        _ => t!("together.react.star"),
    }
}

/// A reaction's colour (the same on every device, readable in light and dark).
pub fn reaction_color(emoji: &str) -> Hsla {
    match emoji {
        "heart" => hsla(350. / 360., 0.75, 0.55, 1.),
        "fire" => hsla(22. / 360., 0.9, 0.52, 1.),
        "laugh" => hsla(45. / 360., 0.9, 0.5, 1.),
        "rock" => hsla(275. / 360., 0.6, 0.6, 1.),
        "party" => hsla(190. / 360., 0.7, 0.45, 1.),
        _ => hsla(50. / 360., 0.95, 0.5, 1.),
    }
}

/// A person's colour, from their name (stable across devices and runs).
pub fn person_color(name: &str) -> Hsla {
    let hash = name.bytes().fold(0x811c_9dc5u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    });
    hsla((hash % 360) as f32 / 360., 0.5, 0.45, 1.)
}

/// An initial in a coloured circle (the chip, the panel, the queue's attribution).
pub fn avatar(name: &str, size: f32, ring: Option<Hsla>) -> gpui::Div {
    let initial = name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".into());
    div()
        .size(px(size))
        .flex_none()
        .rounded_full()
        .bg(person_color(name))
        .when_some(ring, |d, ring| d.border_2().border_color(ring))
        .flex()
        .items_center()
        .justify_center()
        .text_color(gpui::white())
        .text_size(px((size * 0.5).round()))
        .font_weight(FontWeight::SEMIBOLD)
        .child(SharedString::from(initial))
}

/// What an activity note says.
pub fn note_text(who: &str, kind: NoteKind, count: u32, title: Option<&str>) -> String {
    match kind {
        NoteKind::Joined => t!("together.note.joined", name = who),
        NoteKind::Left => t!("together.note.left", name = who),
        NoteKind::Played => t!("together.note.played", name = who),
        NoteKind::Paused => t!("together.note.paused", name = who),
        NoteKind::Skipped => tp!("together.note.skipped", count, name = who),
        NoteKind::WentBack => t!("together.note.went_back", name = who),
        NoteKind::Added => tp!("together.note.added", count, name = who),
        NoteKind::Removed => match title.filter(|t| !t.is_empty() && count == 1) {
            Some(title) => t!("together.note.removed_song", name = who, title = title),
            None => tp!("together.note.removed", count, name = who),
        },
        NoteKind::Moved => t!("together.note.moved", name = who),
        NoteKind::Cleared => t!("together.note.cleared", name = who),
        NoteKind::Started => match title.filter(|t| !t.is_empty()) {
            Some(title) => t!("together.note.started", name = who, title = title),
            None => t!("together.note.started_plain", name = who),
        },
        NoteKind::Mode => {
            let mode = title.and_then(SessionMode::parse).unwrap_or_default();
            t!("together.note.mode", name = who, mode = mode_name(mode))
        }
        NoteKind::Other => t!("together.note.unknown", name = who),
    }
}

/// What this person may do, in a line (the panel, the page).
pub fn permission_line(app: &MusicApp) -> String {
    let together = &app.integrations.together;
    let host = together.host_name().unwrap_or_default();
    let mode = together.mode().unwrap_or_default();
    if together.reconnecting() {
        return t!("together.can.reconnecting").into();
    }
    match &together.role {
        Role::Recovering(_) => t!("together.can.reconnecting").into(),
        Role::Joining(_) => t!("together.can.joining").into(),
        Role::Guest(g) if g.pending => t!("together.can.waiting", name = host),
        Role::Hosting(_) => match mode {
            SessionMode::Friends => t!("together.can.friends").into(),
            SessionMode::Host => t!("together.can.host_you").into(),
            SessionMode::Dj => t!("together.can.dj_you").into(),
        },
        Role::Guest(_) => {
            let p = together.permissions().unwrap_or_default();
            match mode {
                SessionMode::Friends => t!("together.can.friends").into(),
                SessionMode::Dj if p.add_directly => t!("together.can.dj_guest", name = host),
                SessionMode::Dj => t!("together.can.dj_guest_approval", name = host),
                SessionMode::Host if p.control => {
                    t!("together.can.host_guest_control", name = host)
                }
                SessionMode::Host if p.add => t!("together.can.host_guest", name = host),
                SessionMode::Host => t!("together.can.host_guest_listen", name = host),
            }
        }
        Role::Idle => String::new(),
    }
}

impl MusicApp {
    /// From the poll (every 250 ms in a session): toasts and hints from places without
    /// the window, reactions that are done, the activity note moving on.
    pub(super) fn together_poll_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let together = &mut self.integrations.together;
        for toast in std::mem::take(&mut together.toasts) {
            notify(window, cx, toast);
        }
        let together = &mut self.integrations.together;
        if let Some((_, hint)) = together.pending_hint.take() {
            notify(window, cx, hint);
        }
        let together = &mut self.integrations.together;
        let before = together.reactions.len();
        together
            .reactions
            .retain(|r| r.at.elapsed() < REACTION_SHOW);
        let mut changed = together.reactions.len() != before;
        let shown = together
            .notes
            .current(Instant::now())
            .map(|n| (n.who.clone(), n.kind, n.count, n.title.clone()));
        if shown != together.note_shown {
            together.note_shown = shown;
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    /// Reactions are moving on screen (the window then keeps the page cached).
    pub(crate) fn together_reactions_live(&self) -> bool {
        crate::effects::motion() == crate::effects::Motion::Full
            && !self.integrations.together.reactions.is_empty()
    }

    /// The listeners chip in the player bar (with its session panel), in a session or
    /// while joining.
    pub(super) fn together_chip(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let together = &self.integrations.together;
        let theme = cx.theme();
        let (muted, border, ring) = (theme.muted_foreground, theme.border, theme.background);
        let open = together.panel_open;
        let mode = together.mode().unwrap_or_default();
        let busy = match &together.role {
            Role::Idle => return None,
            Role::Joining(_) => Some(t!("together.chip.joining")),
            Role::Recovering(_) => Some(t!("together.chip.reconnecting")),
            Role::Guest(g) if g.pending => Some(t!("together.chip.waiting")),
            _ if together.reconnecting() => Some(t!("together.chip.reconnecting")),
            _ => None,
        };
        let people: Vec<Participant> = together
            .participants()
            .into_iter()
            .filter(|p| !p.is_pending)
            .collect();
        let note = together
            .note_shown
            .as_ref()
            .map(|(who, kind, count, title)| note_text(who, *kind, *count, title.as_deref()));
        let mut content = h_flex().gap_1p5().items_center().h(px(26.)).px_2();
        match busy {
            Some(label) => {
                content = content
                    .child(Spinner::new().small())
                    .child(div().text_xs().text_color(muted).child(label));
            }
            None => {
                content = content.child(
                    Icon::new(super::together_ui::mode_icon(mode))
                        .size(px(13.))
                        .text_color(crate::theme::AppColors::get(cx).accent),
                );
                let mut stack = h_flex();
                for (ix, p) in people.iter().take(STACKED).enumerate() {
                    let face = avatar(&p.name, 20., Some(ring));
                    stack = stack.child(if ix == 0 { face } else { face.ml(px(-6.)) });
                }
                content = content.child(stack);
                content = content.child(match note {
                    Some(note) => div()
                        .max_w(px(180.))
                        .truncate()
                        .text_xs()
                        .child(SharedString::from(note)),
                    None => div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(SharedString::from(people.len().to_string())),
                });
            }
        }
        let tooltip = t!("together.chip.tooltip", mode = mode_name(mode));
        let trigger = div()
            .id("together-chip")
            .rounded_full()
            .border_1()
            .border_color(if open {
                crate::theme::AppColors::get(cx).accent
            } else {
                border
            })
            .cursor_pointer()
            .hover(|s| s.bg(theme.list_hover))
            .child(content);
        let trigger = Button::new("together-chip-button")
            .ghost()
            .rounded(px(999.))
            .tooltip(tooltip)
            .child(trigger);
        Some(
            Popover::new("together-panel")
                .anchor(Anchor::BottomCenter)
                .open(open)
                .on_open_change(|open, _, cx| {
                    let open = *open;
                    update_app(cx, |app, cx| {
                        app.integrations.together.panel_open = open;
                        if !open {
                            app.integrations.together.mode_picker = false;
                        }
                        cx.notify();
                    });
                })
                .trigger(trigger)
                .content(|_, _, cx| together_panel(cx))
                .into_any_element(),
        )
    }

    /// Reactions floating up over the player bar (or over Now Playing).
    pub(crate) fn render_reactions(&self, covered: bool, cx: &App) -> Option<AnyElement> {
        let reactions = &self.integrations.together.reactions;
        if reactions.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let (bg, border) = (theme.popover, theme.border);
        let motion = crate::effects::motion();
        let now = Instant::now();
        let bottom = if covered {
            24.
        } else {
            crate::components::player_bar::BAR_HEIGHT + 12.
        };
        let mut column = v_flex().gap_1p5().items_end();
        for r in reactions {
            let pill = h_flex()
                .gap_1p5()
                .px_2p5()
                .py_1()
                .rounded_full()
                .bg(bg)
                .border_1()
                .border_color(border)
                .shadow_md()
                .child(
                    Icon::new(reaction_icon(&r.emoji))
                        .size(px(16.))
                        .text_color(reaction_color(&r.emoji)),
                )
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .child(SharedString::from(r.who.clone())),
                );
            let id = SharedString::from(format!("together-reaction-{}", r.id));
            let element = match motion {
                crate::effects::Motion::Full => {
                    // Started a moment ago (a redraw): the animation goes on from there.
                    let left = REACTION_SHOW.saturating_sub(now.duration_since(r.at));
                    div()
                        .id(id.clone())
                        .child(pill)
                        .with_animation(
                            id,
                            Animation::new(left.max(Duration::from_millis(1))),
                            |el, t| {
                                // Rises and fades in quickly, holds, fades out at the end.
                                let opacity = (t * 8.).min(1.) * ((1. - t) * 5.).min(1.);
                                el.opacity(opacity).mb(px(t * 36.))
                            },
                        )
                        .into_any_element()
                }
                crate::effects::Motion::Reduced => div()
                    .id(id.clone())
                    .child(pill)
                    .with_animation(id, Animation::new(Duration::from_millis(150)), |el, t| {
                        el.opacity(t)
                    })
                    .into_any_element(),
                crate::effects::Motion::Off => pill.into_any_element(),
            };
            column = column.child(element);
        }
        Some(
            div()
                .absolute()
                .right(px(20.))
                .bottom(px(bottom))
                .child(column)
                .into_any_element(),
        )
    }

    /// "Start listening together": the sheet that asks how (`group`: started from a
    /// friend group, which is then preselected under "Who can join").
    pub fn together_open_start_sheet(
        &mut self,
        group: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prefs = self.settings.read(cx).settings().clone();
        let groups: Vec<(String, String)> = if prefs.friends.enabled {
            prefs
                .friends
                .groups
                .iter()
                .map(|g| (g.id().to_owned(), g.name().to_owned()))
                .collect()
        } else {
            Vec::new()
        };
        let mode = match &group {
            Some(g) => prefs.friends.group_mode(g),
            None => prefs.together.last_mode,
        };
        let choice = Rc::new(RefCell::new(StartOptions {
            mode,
            group: group.filter(|g| groups.iter().any(|(id, _)| id == g)),
            network: prefs.together.host_mode,
        }));
        let groups = Rc::new(groups);
        window.open_dialog(cx, move |dialog, _, cx| {
            let theme = cx.theme();
            let (muted, border, hover, accent) = (
                theme.muted_foreground,
                theme.border,
                theme.list_hover,
                crate::theme::AppColors::get(cx).accent,
            );
            let current = choice.borrow().clone();
            let pick = |id: SharedString,
                        selected: bool,
                        icon: Option<Lucide>,
                        title: SharedString,
                        line: Option<SharedString>,
                        set: Box<dyn Fn(&mut StartOptions)>| {
                let choice = choice.clone();
                h_flex()
                    .id(id)
                    .gap_3()
                    .p_2p5()
                    .rounded(crate::theme::radius(8.))
                    .border_1()
                    .border_color(if selected { accent } else { border })
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .when_some(icon, |row, icon| {
                        row.child(Icon::new(icon).size(px(18.)).text_color(if selected {
                            accent
                        } else {
                            muted
                        }))
                    })
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .children(line.map(|l| div().text_xs().text_color(muted).child(l))),
                    )
                    .when(selected, |row| {
                        row.child(
                            Icon::new(Lucide::CircleCheck)
                                .size(px(16.))
                                .text_color(accent),
                        )
                    })
                    .on_click(move |_, window, _| {
                        set(&mut choice.borrow_mut());
                        window.refresh();
                    })
            };
            let mut modes = v_flex().gap_1p5();
            for mode in SessionMode::ALL {
                modes = modes.child(pick(
                    SharedString::from(format!("tg-start-mode-{}", mode.key())),
                    current.mode == mode,
                    Some(mode_icon(mode)),
                    mode_name(mode).into(),
                    Some(mode_description(mode).into()),
                    Box::new(move |c| c.mode = mode),
                ));
            }
            let mut who = v_flex().gap_1p5().child(pick(
                "tg-start-anyone".into(),
                current.group.is_none(),
                Some(Lucide::Link),
                t!("together.start.anyone").into(),
                None,
                Box::new(|c| c.group = None),
            ));
            for (id, name) in groups.iter() {
                let group = id.clone();
                who = who.child(pick(
                    SharedString::from(format!("tg-start-group-{id}")),
                    current.group.as_deref() == Some(id.as_str()),
                    Some(Lucide::UsersRound),
                    t!("together.start.group", group = name).into(),
                    Some(t!("together.start.group_note").into()),
                    Box::new(move |c| c.group = Some(group.clone())),
                ));
            }
            let network = |id: &'static str, label: &'static str, mode: HostMode| {
                let choice = choice.clone();
                let selected = current.network == mode;
                Button::new(id)
                    .small()
                    .map(|b| if selected { b.primary() } else { b.outline() })
                    .label(label)
                    .on_click(move |_, window, _| {
                        choice.borrow_mut().network = mode;
                        window.refresh();
                    })
            };
            let start = {
                let choice = choice.clone();
                move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
                    let options = choice.borrow().clone();
                    update_app(cx, |app, cx| {
                        app.together_start_with_choice(options, window, cx)
                    });
                    window.close_dialog(cx);
                }
            };
            dialog.title(t!("together.start.title")).w(px(460.)).child(
                v_flex()
                    .gap_4()
                    .child(section(t!("together.start.mode"), muted).child(modes))
                    .child(section(t!("together.start.who"), muted).child(who))
                    .child(
                        section(t!("together.start.network"), muted).child(
                            h_flex()
                                .gap_1()
                                .flex_wrap()
                                .child(network(
                                    "tg-start-relay",
                                    t!("together.host.mode_relay"),
                                    HostMode::Relay,
                                ))
                                .child(network(
                                    "tg-start-direct",
                                    t!("together.host.mode_direct"),
                                    HostMode::Direct,
                                )),
                        ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .child(
                                Button::new("tg-start-cancel")
                                    .ghost()
                                    .label(t!("common.cancel"))
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            )
                            .child(
                                Button::new("tg-start-go")
                                    .primary()
                                    .icon(Lucide::Radio)
                                    .label(t!("together.start.button"))
                                    .on_click(start),
                            ),
                    ),
            )
        });
    }

    /// The sheet's choice: remembered (the mode for code sessions, a group's mode for
    /// its sessions), then started; a group's members are invited.
    pub(crate) fn together_start_with_choice(
        &mut self,
        options: StartOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mode = options.mode;
        match &options.group {
            Some(group) => {
                let group = group.clone();
                self.settings.update(cx, |panel, cx| {
                    panel.set_friends_prefs(
                        |f| {
                            f.group_modes.insert(group, mode);
                        },
                        cx,
                    )
                });
            }
            None => {
                self.settings.update(cx, |panel, cx| {
                    panel.set_together(|t| t.last_mode = mode, cx)
                });
            }
        }
        match options.group.clone() {
            Some(group) => self.friends_start_session(&group, options, window, cx),
            None => self.together_start(options, window, cx, |_, _| {}),
        }
    }

    /// Asks before switching the session's mode (what changes, for whom).
    pub fn together_confirm_mode(
        &mut self,
        mode: SessionMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let hosting = matches!(self.integrations.together.role, Role::Hosting(_));
        let line = match (mode, hosting) {
            (SessionMode::Friends, _) => t!("together.switch.friends"),
            (SessionMode::Host, true) => t!("together.switch.host_you"),
            (SessionMode::Dj, true) => t!("together.switch.dj_you"),
            (SessionMode::Host, false) => t!("together.switch.host_guest"),
            (SessionMode::Dj, false) => t!("together.switch.dj_guest"),
        };
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(t!("together.switch.title", mode = mode_name(mode)))
                .w(px(380.))
                .child(
                    v_flex().gap_4().child(div().text_sm().child(line)).child(
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .child(
                                Button::new("tg-switch-cancel")
                                    .ghost()
                                    .label(t!("common.cancel"))
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            )
                            .child(
                                Button::new("tg-switch-go")
                                    .primary()
                                    .icon(mode_icon(mode))
                                    .label(t!("together.switch.confirm"))
                                    .on_click(move |_, window, cx| {
                                        update_app(cx, |app, cx| app.together_set_mode(mode, cx));
                                        window.close_dialog(cx);
                                    }),
                            ),
                    ),
                )
        });
    }

    /// The link (and code) to invite people with, when there is one to share (not for
    /// a friend group's session, whose members got it privately).
    pub(crate) fn together_invite(&self) -> Option<(String, Option<String>)> {
        match &self.integrations.together.role {
            Role::Hosting(host) if host.group.is_none() => Some((host.link(), host.code.clone())),
            _ => None,
        }
    }
}

fn section(title: &'static str, muted: Hsla) -> gpui::Div {
    v_flex().gap_2().child(
        div()
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(muted)
            .child(title),
    )
}

/// What the session panel shows (read from the app before drawing).
struct PanelData {
    mode: SessionMode,
    can_switch: bool,
    can_remove: bool,
    picker: bool,
    line: String,
    people: Vec<Participant>,
    me: Option<String>,
    invite: Option<(String, Option<String>)>,
    group: Option<String>,
    hosting: bool,
    in_session: bool,
    playing: bool,
}

/// The listeners chip's panel.
fn together_panel(cx: &mut App) -> AnyElement {
    let Some(data) = read_app(cx, |app, cx| {
        let together = &app.integrations.together;
        let p = together.permissions().unwrap_or_default();
        let group = together.group().and_then(|g| {
            app.settings
                .read(cx)
                .settings()
                .friends
                .groups
                .iter()
                .find(|x| x.id() == g)
                .map(|x| x.name().to_owned())
        });
        PanelData {
            mode: together.mode().unwrap_or_default(),
            can_switch: p.switch_mode && together.in_session(),
            can_remove: p.moderate,
            picker: together.mode_picker,
            line: permission_line(app),
            people: together.participants(),
            me: together.me(),
            invite: app.together_invite(),
            group,
            hosting: matches!(together.role, Role::Hosting(_)),
            in_session: together.in_session(),
            playing: app.queue.current().is_some(),
        }
    }) else {
        return div().into_any_element();
    };
    let theme = cx.theme();
    let (muted, border, accent, hover) = (
        theme.muted_foreground,
        theme.border,
        crate::theme::AppColors::get(cx).accent,
        theme.list_hover,
    );
    let divider = || div().h(px(1.)).w_full().bg(border);
    let friends = data.mode == SessionMode::Friends;

    // Mode badge and switch.
    let badge = h_flex()
        .gap_1p5()
        .px_2()
        .py_0p5()
        .rounded_full()
        .bg(accent.opacity(0.15))
        .child(
            Icon::new(mode_icon(data.mode))
                .size(px(13.))
                .text_color(accent),
        )
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(accent)
                .child(SharedString::from(t!(
                    "together.mode.badge",
                    mode = mode_name(data.mode)
                ))),
        );
    let header = h_flex()
        .gap_2()
        .justify_between()
        .child(badge)
        .when(data.can_switch, |row| {
            row.child(
                Button::new("together-panel-mode")
                    .ghost()
                    .xsmall()
                    .label(t!("together.panel.change_mode"))
                    .selected(data.picker)
                    .on_click(|_, _, cx| {
                        update_app(cx, |app, cx| {
                            let picker = &mut app.integrations.together.mode_picker;
                            *picker = !*picker;
                            cx.notify();
                        });
                    }),
            )
        });
    let mut panel = v_flex().w(px(320.)).gap_3().child(header).child(
        div()
            .text_sm()
            .text_color(muted)
            .child(SharedString::from(data.line)),
    );
    if let Some(group) = &data.group {
        panel = panel.child(
            h_flex()
                .gap_1p5()
                .child(
                    Icon::new(Lucide::UsersRound)
                        .size(px(13.))
                        .text_color(muted),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(SharedString::from(t!(
                            "together.panel.group_only",
                            group = group
                        ))),
                ),
        );
    }
    if data.picker {
        let mut modes = v_flex().gap_1();
        for mode in SessionMode::ALL {
            let current = mode == data.mode;
            modes = modes.child(
                h_flex()
                    .id(SharedString::from(format!("together-pick-{}", mode.key())))
                    .gap_2()
                    .p_2()
                    .rounded(crate::theme::radius(6.))
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .child(
                        Icon::new(mode_icon(mode))
                            .size(px(16.))
                            .text_color(if current { accent } else { muted }),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(mode_name(mode)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(mode_description(mode)),
                            ),
                    )
                    .when(current, |row| {
                        row.child(
                            Icon::new(Lucide::CircleCheck)
                                .size(px(14.))
                                .text_color(accent),
                        )
                    })
                    .when(!current, |row| {
                        row.on_click(move |_, window, cx| {
                            update_app(cx, |app, cx| {
                                app.integrations.together.mode_picker = false;
                                app.integrations.together.panel_open = false;
                                app.together_confirm_mode(mode, window, cx);
                            });
                        })
                    }),
            );
        }
        panel = panel.child(modes);
    }

    // Who is listening.
    let mut people = v_flex().gap_1();
    for (ix, p) in data.people.iter().enumerate() {
        let mine = data.me.as_deref() == Some(p.id.as_str());
        let name = if mine {
            t!("together.panel.you_suffix", name = p.name)
        } else {
            p.name.clone()
        };
        let id = p.id.clone();
        people = people.child(
            h_flex()
                .gap_2()
                .py_0p5()
                .child(avatar(&p.name, 24., None))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .child(SharedString::from(name)),
                )
                // Nobody hosts a Friends session: no tag.
                .when(p.is_host && !friends, |row| {
                    row.child(div().text_xs().text_color(muted).child(
                        if data.mode == SessionMode::Dj {
                            t!("together.tag.dj")
                        } else {
                            t!("together.tag.host")
                        },
                    ))
                })
                .when(p.is_pending, |row| {
                    row.child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(t!("together.tag.waiting")),
                    )
                })
                .when(data.can_remove && !mine && !p.is_host, |row| {
                    row.child(
                        Button::new(("together-panel-remove", ix))
                            .ghost()
                            .xsmall()
                            .icon(Lucide::X)
                            .tooltip(t!("together.people.remove"))
                            .on_click(move |_, _, cx| {
                                let id = id.clone();
                                update_app(cx, |app, cx| app.together_kick(&id, false, cx));
                            }),
                    )
                }),
        );
    }
    panel = panel.child(divider()).child(
        v_flex()
            .gap_1p5()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(muted)
                    .child(SharedString::from(tp!(
                        "together.panel.listening",
                        data.people.iter().filter(|p| !p.is_pending).count()
                    ))),
            )
            .child(people),
    );

    // Reactions.
    if data.in_session {
        let mut row = h_flex().gap_1();
        for emoji in REACTIONS {
            row = row.child(
                Button::new(SharedString::from(format!("together-react-{emoji}")))
                    .ghost()
                    .small()
                    .icon(
                        Icon::new(reaction_icon(emoji))
                            .size(px(18.))
                            .text_color(reaction_color(emoji)),
                    )
                    .tooltip(reaction_label(emoji))
                    .disabled(!data.playing)
                    .on_click(move |_, _, cx| {
                        update_app(cx, |app, cx| app.together_react(emoji, cx));
                    }),
            );
        }
        panel = panel.child(divider()).child(
            h_flex()
                .gap_2()
                .justify_between()
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(t!("together.react.title")),
                )
                .child(row),
        );
    }

    // Invite, the page, leave.
    let mut actions = h_flex().gap_2().flex_wrap();
    if let Some((link, code)) = data.invite.clone() {
        actions = actions.child(
            Button::new("together-panel-invite")
                .small()
                .outline()
                .icon(Lucide::Link)
                .label(match &code {
                    Some(code) => t!("together.panel.copy_code", code = display_code(code)),
                    None => t!("together.panel.copy_link").into(),
                })
                .on_click(move |_, window, cx| {
                    let text = code.as_deref().map(display_code).unwrap_or(link.clone());
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    notify(window, cx, t!("together.hosting.link_copied"));
                }),
        );
    }
    actions = actions.child(
        Button::new("together-panel-page")
            .small()
            .ghost()
            .label(t!("together.panel.open_page"))
            .on_click(|_, window, cx| {
                update_app(cx, |app, cx| {
                    app.integrations.together.panel_open = false;
                    app.navigate(crate::router::Route::Together, window, cx);
                });
            }),
    );
    // Hosting a Host or DJ session ends it for everyone; a Friends session goes on.
    let ends = data.hosting && !friends;
    actions = actions.child(
        Button::new("together-panel-leave")
            .small()
            .when(ends, |b| b.danger())
            .when(!ends, |b| b.outline())
            .icon(Lucide::LogOut)
            .label(if ends {
                t!("together.panel.end")
            } else {
                t!("together.panel.leave")
            })
            .on_click(|_, _, cx| {
                update_app(cx, |app, cx| {
                    app.integrations.together.panel_open = false;
                    app.together_leave(cx);
                });
            }),
    );
    panel.child(divider()).child(actions).into_any_element()
}
