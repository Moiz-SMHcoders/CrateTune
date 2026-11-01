//! Appearance → Layout → Sidebar items and Sidebar playlists (`sidebar_prefs.rs`):
//! a check box per entry with up / down buttons (or drag a row onto another), Reset,
//! and the playlists section's options; plus the panel methods the sidebar's menus
//! and the scripts use (`@sidebar-items`, `@sidebar-playlists`).

use gpui::{
    AnyElement, AppContext as _, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    switch::Switch,
    v_flex,
};

use super::panel::{SettingsEvent, SettingsPanel};
use super::prefs_ui::choice;
use super::sidebar_prefs::{LIKED_ID, PLAYLIST_LIMITS, PlaylistOrder, SidebarConfig};
use super::widgets::{Anchor as _, note, row_at, stack_at};
use crate::icons::Lucide;
use crate::pages::NavItem;

impl SettingsPanel {
    pub fn sidebar(&self) -> &SidebarConfig {
        &self.settings.sidebar
    }

    /// Edits the sidebar's options; the app re-renders the sidebar when they changed.
    pub fn set_sidebar(&mut self, edit: impl FnOnce(&mut SidebarConfig), cx: &mut Context<Self>) {
        let before = self.settings.sidebar.clone();
        edit(&mut self.settings.sidebar);
        self.settings.sidebar.normalize();
        if self.settings.sidebar != before {
            cx.emit(SettingsEvent::Appearance);
            self.changed(false, cx);
            cx.notify();
        }
    }

    /// Pins a playlist (library id or `liked`) to the top of the sidebar's list, or unpins it.
    pub fn set_sidebar_pinned(&mut self, id: String, pinned: bool, cx: &mut Context<Self>) {
        self.set_sidebar(|s| s.playlists.set_pinned(&id, pinned), cx);
    }
}

/// A sidebar entry being dragged in the editor.
#[derive(Clone)]
struct DraggedEntry {
    key: &'static str,
    title: SharedString,
}

impl Render for DraggedEntry {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_3()
            .py_1p5()
            .rounded(crate::theme::radius(6.))
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border)
            .text_sm()
            .shadow_md()
            .child(self.title.clone())
    }
}

/// The "Sidebar items" editor and the "Sidebar playlists" options, inside Layout.
pub(super) fn render(panel: &mut SettingsPanel, cx: &mut Context<SettingsPanel>) -> AnyElement {
    v_flex()
        .gap_2()
        .child(render_items(panel, cx))
        .child(render_playlists(panel, cx))
        .into_any_element()
}

fn render_items(panel: &mut SettingsPanel, cx: &mut Context<SettingsPanel>) -> AnyElement {
    let config = panel.sidebar().clone();
    let theme = cx.theme();
    let (border, muted, hover, drop_bg) = (
        theme.border,
        theme.muted_foreground,
        theme.list_hover,
        theme.drop_target,
    );
    let count = config.order.len();
    let rows =
        config.entries().enumerate().map(|(i, (item, shown))| {
            let key = item.key();
            let title = crate::i18n::text(item.title());
            let (icon, label): (Option<Lucide>, SharedString) = match item {
                NavItem::Page { icon, .. } => (Some(*icon), title.into()),
                NavItem::Group { .. } => (None, t!("sidebar.items.heading", name = title).into()),
            };
            h_flex()
                .id(SharedString::from(format!("sidebar-item-{key}")))
                .gap_2()
                .px_2()
                .py_0p5()
                .rounded(crate::theme::radius(6.))
                .hover(move |s| s.bg(hover))
                .on_drag(
                    DraggedEntry {
                        key,
                        title: label.clone(),
                    },
                    |entry, _, _, cx| cx.new(|_| entry.clone()),
                )
                .drag_over::<DraggedEntry>(move |style, _, _, _| style.bg(drop_bg))
                .on_drop(cx.listener(move |this, dragged: &DraggedEntry, _, cx| {
                    let from = dragged.key;
                    this.set_sidebar(|s| s.move_to(from, key), cx);
                }))
                .child(
                    div()
                        .flex_none()
                        .cursor_grab()
                        .text_color(muted)
                        .child(Icon::new(Lucide::GripVertical).xsmall()),
                )
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_2()
                        .child(
                            Checkbox::new(SharedString::from(format!("sidebar-item-check-{key}")))
                                .checked(shown)
                                .on_click(cx.listener(move |this, on: &bool, _, cx| {
                                    let on = *on;
                                    this.set_sidebar(|s| s.set_shown(key, on), cx);
                                })),
                        )
                        .children(icon.map(|icon| {
                            div()
                                .flex_none()
                                .text_color(muted)
                                .child(Icon::new(icon).small())
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .when(!shown, |this| this.text_color(muted))
                                .child(label),
                        ),
                )
                .child(
                    Button::new(SharedString::from(format!("sidebar-item-up-{key}")))
                        .ghost()
                        .xsmall()
                        .icon(Lucide::ArrowUp)
                        .tooltip(t!("queue.tooltip.move_up"))
                        .disabled(i == 0)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_sidebar(|s| s.nudge(key, -1), cx)
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("sidebar-item-down-{key}")))
                        .ghost()
                        .xsmall()
                        .icon(Lucide::ArrowDown)
                        .tooltip(t!("queue.tooltip.move_down"))
                        .disabled(i + 1 == count)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_sidebar(|s| s.nudge(key, 1), cx)
                        })),
                )
        });
    v_flex()
        .gap_2()
        .anchor("appearance.layout.sidebar_items")
        .pt_3()
        .border_t_1()
        .border_color(border)
        .child(
            h_flex()
                .justify_between()
                .gap_4()
                .child(div().text_sm().child(t!("sidebar.items.title")))
                .child(
                    Button::new("sidebar-items-reset")
                        .ghost()
                        .small()
                        .label(t!("sidebar.items.reset"))
                        .disabled(config.items_are_default())
                        .on_click(
                            cx.listener(|this, _, _, cx| this.set_sidebar(|s| s.reset_items(), cx)),
                        ),
                ),
        )
        .child(note(t!("sidebar.items.note"), cx))
        .child(v_flex().max_w(px(380.)).gap_0p5().children(rows))
        .into_any_element()
}

fn render_playlists(panel: &mut SettingsPanel, cx: &mut Context<SettingsPanel>) -> AnyElement {
    let p = panel.sidebar().playlists.clone();
    let border = cx.theme().border;
    let show = Switch::new("sidebar-playlists-show")
        .checked(p.show)
        .on_click(cx.listener(|this, on: &bool, _, cx| {
            let on = *on;
            this.set_sidebar(|s| s.playlists.show = on, cx);
        }));
    let toggle = |id: &'static str, label: &'static str, on: bool| {
        Button::new(id)
            .small()
            .map(|b| if on { b.primary() } else { b.outline() })
            .selected(on)
            .label(label)
    };
    let sources = h_flex()
        .gap_1()
        .flex_wrap()
        .child(
            toggle(
                "sidebar-playlists-youtube",
                t!("sidebar.playlists.youtube"),
                p.youtube,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set_sidebar(|s| s.playlists.youtube = !p.youtube, cx)
            })),
        )
        .child(
            toggle(
                "sidebar-playlists-local",
                t!("sidebar.playlists.local"),
                p.local,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set_sidebar(|s| s.playlists.local = !p.local, cx)
            })),
        )
        .child(
            toggle(
                "sidebar-playlists-liked",
                t!("route.library_tab.liked"),
                p.liked,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set_sidebar(|s| s.playlists.liked = !p.liked, cx)
            })),
        );
    let sort = choice(
        "sidebar-playlists-sort",
        PlaylistOrder::ALL,
        p.sort,
        PlaylistOrder::title,
        |this, sort, cx| this.set_sidebar(|s| s.playlists.sort = sort, cx),
        cx,
    );
    let limits = h_flex()
        .gap_1()
        .flex_wrap()
        .children(PLAYLIST_LIMITS.iter().map(|&limit| {
            let selected = p.limit == limit;
            let label: SharedString = if limit == 0 {
                t!("sidebar.playlists.limit_all").into()
            } else {
                limit.to_string().into()
            };
            Button::new(SharedString::from(format!(
                "sidebar-playlists-limit-{limit}"
            )))
            .small()
            .map(|b| if selected { b.primary() } else { b.outline() })
            .selected(selected)
            .label(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set_sidebar(|s| s.playlists.limit = limit, cx)
            }))
        }));
    let artwork = Switch::new("sidebar-playlists-artwork")
        .checked(p.artwork)
        .on_click(cx.listener(|this, on: &bool, _, cx| {
            let on = *on;
            this.set_sidebar(|s| s.playlists.artwork = on, cx);
        }));
    let pinned = p.pinned.len();
    v_flex()
        .gap_2()
        .pt_3()
        .border_t_1()
        .border_color(border)
        .child(row_at(
            "appearance.layout.sidebar_playlists",
            t!("sidebar.playlists.title"),
            Some(t!("sidebar.playlists.note")),
            show,
            cx,
        ))
        .when(p.show, |this| {
            this.child(stack_at(
                "appearance.layout.sidebar_playlists.sources",
                t!("sidebar.playlists.sources"),
                Some(t!("sidebar.playlists.sources_note")),
                sources,
                cx,
            ))
            .child(stack_at(
                "appearance.layout.sidebar_playlists.sort",
                t!("sidebar.playlists.sort"),
                Some(t!("sidebar.playlists.sort_note")),
                sort,
                cx,
            ))
            .child(stack_at(
                "appearance.layout.sidebar_playlists.limit",
                t!("sidebar.playlists.limit"),
                None,
                limits,
                cx,
            ))
            .child(row_at(
                "appearance.layout.sidebar_playlists.artwork",
                t!("sidebar.playlists.artwork"),
                Some(t!("sidebar.playlists.artwork_note")),
                artwork,
                cx,
            ))
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(div().flex_1().child(note(
                        tp!("sidebar.playlists.pinned_count", pinned as u64),
                        cx,
                    )))
                    .child(
                        Button::new("sidebar-playlists-unpin-all")
                            .ghost()
                            .small()
                            .icon(Lucide::PinOff)
                            .label(t!("sidebar.playlists.unpin_all"))
                            .disabled(pinned == 0)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_sidebar(|s| s.playlists.pinned.clear(), cx)
                            })),
                    ),
            )
        })
        .into_any_element()
}

impl crate::app::MusicApp {
    /// `@sidebar-items=KEY:on|off|up|down`, `@sidebar-items=reset` and
    /// `@sidebar-playlists=show:on|youtube:off|local:on|liked:off|sort:recent|name|custom|limit:5|artwork:off|pin:NAME|unpin:NAME|expand:on`
    /// (saved like a click; `pin` / `unpin` take a playlist's name or `liked`).
    pub(crate) fn sidebar_script_action(
        &mut self,
        name: &str,
        arg: Option<&str>,
        cx: &mut Context<crate::app::MusicApp>,
    ) -> bool {
        let arg = arg.unwrap_or_default();
        let (option, value) = arg.split_once(':').unwrap_or((arg, ""));
        let on = value != "off";
        let panel = self.settings.clone();
        match name {
            "sidebar-items" => {
                let key = option.to_owned();
                let known = NavItem::find(&key).is_some();
                match (option, value) {
                    ("reset", _) => {
                        panel.update(cx, |p, cx| p.set_sidebar(|s| s.reset_items(), cx))
                    }
                    (_, "up" | "down") if known => {
                        let delta = if value == "up" { -1 } else { 1 };
                        panel.update(cx, |p, cx| p.set_sidebar(|s| s.nudge(&key, delta), cx));
                    }
                    (_, "on" | "off") if known => {
                        panel.update(cx, |p, cx| p.set_sidebar(|s| s.set_shown(&key, on), cx));
                    }
                    _ => eprintln!("music-gpui: @sidebar-items wants KEY:on|off|up|down or reset"),
                }
            }
            "sidebar-playlists" => match option {
                "show" | "youtube" | "local" | "liked" | "artwork" => {
                    let option = option.to_owned();
                    panel.update(cx, |p, cx| {
                        p.set_sidebar(
                            |s| {
                                let p = &mut s.playlists;
                                *match option.as_str() {
                                    "show" => &mut p.show,
                                    "youtube" => &mut p.youtube,
                                    "local" => &mut p.local,
                                    "liked" => &mut p.liked,
                                    _ => &mut p.artwork,
                                } = on;
                            },
                            cx,
                        )
                    });
                }
                "sort" => match PlaylistOrder::parse(value) {
                    Some(sort) => {
                        panel.update(cx, |p, cx| p.set_sidebar(|s| s.playlists.sort = sort, cx))
                    }
                    None => {
                        eprintln!("music-gpui: @sidebar-playlists=sort: wants recent|name|custom")
                    }
                },
                "limit" => match value.parse::<u32>() {
                    Ok(limit) => {
                        panel.update(cx, |p, cx| p.set_sidebar(|s| s.playlists.limit = limit, cx))
                    }
                    Err(_) => eprintln!("music-gpui: @sidebar-playlists=limit: wants 5|10|20|0"),
                },
                "pin" | "unpin" => {
                    let id = if value == LIKED_ID {
                        Some(LIKED_ID.to_owned())
                    } else {
                        self.sidebar_playlist_id(value)
                    };
                    match id {
                        Some(id) => {
                            let pin = option == "pin";
                            panel.update(cx, |p, cx| p.set_sidebar_pinned(id, pin, cx));
                        }
                        None => eprintln!("music-gpui: no sidebar playlist called {value:?}"),
                    }
                }
                "expand" => self.set_sidebar_playlists_expanded(on, cx),
                _ => eprintln!(
                    "music-gpui: @sidebar-playlists wants show:|youtube:|local:|liked:|sort:|limit:|artwork:|pin:|unpin:|expand:"
                ),
            },
            _ => return false,
        }
        cx.notify();
        true
    }
}
