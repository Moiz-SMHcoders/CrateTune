//! Left navigation: app title, the browse / library entries from
//! [`crate::pages::NAV`] (without Home when the custom top bar shows it), and
//! Settings at the bottom.
//!
//! Appearance → Layout → Sidebar: **Full** (icons and titles; drag its right
//! edge for the width, 176–360 px), **Icons only** (a 68 px rail with tooltips)
//! or **Expand on hover** (the rail; hovering it lays the full sidebar over the
//! page — one render per hover change, nothing animates). The queue panel's
//! left edge drags the same way. A drag re-renders the shell per mouse move
//! and saves the width once, on release.

use gpui::{
    AnyElement, App, Context, Div, FontWeight, InteractiveElement as _, IntoElement, MouseButton,
    MouseMoveEvent, ParentElement as _, Pixels, SharedString, StatefulInteractiveElement as _,
    Styled as _, deferred, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};

use crate::app::MusicApp;
use crate::i18n::{Key, text};
use crate::icons::Lucide;
use crate::pages::{NAV, NavItem};
use crate::router::Route;
use crate::settings::SectionId;
use crate::settings::look_prefs::{QUEUE_WIDTHS, SIDEBAR_WIDTHS, SidebarMode};
use crate::theme::AppColors;

/// The classic (default) width of the full sidebar.
pub const SIDEBAR_WIDTH: f32 = 216.;
/// The icon rail's width.
pub const RAIL_WIDTH: f32 = 68.;
/// The grab area along a panel's edge.
const HANDLE: f32 = 6.;

/// Which panel's edge is dragged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelEdge {
    Sidebar,
    Queue,
}

/// Hover and drag state of the sidebar and the queue panel.
#[derive(Debug, Default)]
pub struct Panels {
    /// Expand on hover: the pointer is over the rail / over the expanded panel.
    hover_rail: bool,
    hover_panel: bool,
    /// A drag in progress: the edge, where it started (x) and the width then.
    drag: Option<(PanelEdge, Pixels, f32)>,
    /// The width while dragging (saved on release).
    live: Option<(PanelEdge, f32)>,
}

impl Panels {
    pub fn resizing(&self) -> bool {
        self.drag.is_some()
    }

    fn expanded(&self) -> bool {
        self.hover_rail || self.hover_panel
    }
}

/// The width a drag from `from` (x) with the panel `start` wide to `x` gives
/// `edge`, in its range.
pub fn dragged_width(edge: PanelEdge, from: f32, start: f32, x: f32) -> f32 {
    let (lo, hi) = match edge {
        PanelEdge::Sidebar => SIDEBAR_WIDTHS,
        PanelEdge::Queue => QUEUE_WIDTHS,
    };
    // The sidebar grows to the right, the queue panel (on the right) to the left.
    let delta = match edge {
        PanelEdge::Sidebar => x - from,
        PanelEdge::Queue => from - x,
    };
    (start + delta).clamp(lo, hi).round()
}

impl MusicApp {
    fn sidebar_mode(&self, cx: &App) -> SidebarMode {
        self.settings.read(cx).settings().effects.sidebar
    }

    /// The full sidebar's width (while dragging: the dragged one).
    fn full_sidebar_width(&self, cx: &App) -> f32 {
        match self.panels.live {
            Some((PanelEdge::Sidebar, w)) => w,
            _ => self.settings.read(cx).settings().effects.sidebar_width,
        }
    }

    /// The width the sidebar takes in the window's layout, px.
    pub(crate) fn sidebar_width(&self, cx: &App) -> f32 {
        match self.sidebar_mode(cx) {
            SidebarMode::Full => self.full_sidebar_width(cx),
            SidebarMode::Rail | SidebarMode::Hover => RAIL_WIDTH,
        }
    }

    /// The queue panel's width, px.
    pub(crate) fn queue_width(&self, cx: &App) -> f32 {
        match self.panels.live {
            Some((PanelEdge::Queue, w)) => w,
            _ => self.settings.read(cx).settings().effects.queue_width,
        }
    }

    /// The grab strip along `edge`'s inner side (an absolute child of the panel).
    pub(crate) fn panel_resize_handle(&self, edge: PanelEdge, cx: &mut Context<Self>) -> Div {
        let width = match edge {
            PanelEdge::Sidebar => self.full_sidebar_width(cx),
            PanelEdge::Queue => self.queue_width(cx),
        };
        let line = cx.theme().ring;
        let dragging = self.panels.drag.is_some_and(|(e, ..)| e == edge);
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .w(px(HANDLE))
            .map(|this| match edge {
                PanelEdge::Sidebar => this.right(px(-HANDLE / 2.)),
                PanelEdge::Queue => this.left(px(-HANDLE / 2.)),
            })
            .child(
                div()
                    .id(match edge {
                        PanelEdge::Sidebar => "sidebar-resize",
                        PanelEdge::Queue => "queue-resize",
                    })
                    .size_full()
                    .cursor(gpui::CursorStyle::ResizeLeftRight)
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .w(px(2.))
                            .h_full()
                            .when(dragging, |this| this.bg(line))
                            .when(!dragging, |this| {
                                this.hover(move |s| s.bg(line.opacity(0.6)))
                            }),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, e: &gpui::MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.panels.drag = Some((edge, e.position.x, width));
                            this.panels.live = Some((edge, width));
                            cx.notify();
                        }),
                    ),
            )
    }

    /// The pointer moved during a drag (`app.rs` listens on the root).
    pub(crate) fn drag_panel_edge(&mut self, e: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some((edge, from, start)) = self.panels.drag else {
            return;
        };
        if e.pressed_button != Some(MouseButton::Left) {
            // Released outside the window.
            self.end_panel_resize(cx);
            return;
        }
        let w = dragged_width(edge, f32::from(from), start, f32::from(e.position.x));
        if self.panels.live != Some((edge, w)) {
            self.panels.live = Some((edge, w));
            cx.notify();
        }
    }

    /// The drag ended: keep the width (one settings save).
    pub(crate) fn end_panel_resize(&mut self, cx: &mut Context<Self>) {
        self.panels.drag = None;
        if let Some((edge, w)) = self.panels.live {
            let (sidebar, queue) = match edge {
                PanelEdge::Sidebar => (Some(w), None),
                PanelEdge::Queue => (None, Some(w)),
            };
            self.settings
                .update(cx, |s, cx| s.set_panel_widths(sidebar, queue, cx));
        }
        self.panels.live = None;
        cx.notify();
    }

    /// `@sidebar-hover`: shows the expanded panel as if the pointer were on
    /// the rail (screenshots, checks).
    pub(crate) fn set_sidebar_hover(&mut self, on: bool, cx: &mut Context<Self>) {
        self.panels.hover_rail = on;
        cx.notify();
    }

    pub(crate) fn render_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        match self.sidebar_mode(cx) {
            SidebarMode::Full => {
                let width = self.full_sidebar_width(cx);
                let handle = self.panel_resize_handle(PanelEdge::Sidebar, cx);
                div()
                    .relative()
                    .flex_none()
                    .h_full()
                    .child(self.sidebar_panel(false, width, "", cx))
                    .child(handle)
                    .into_any_element()
            }
            SidebarMode::Rail => div()
                .flex_none()
                .h_full()
                .child(self.sidebar_panel(true, RAIL_WIDTH, "", cx))
                .into_any_element(),
            SidebarMode::Hover => {
                let width = self.full_sidebar_width(cx);
                let overlay = self.panels.expanded().then(|| {
                    let panel = self
                        .sidebar_panel(false, width, "hover-", cx)
                        .bg(cx.theme().sidebar)
                        .shadow_xl();
                    // Painted after the page, over it.
                    deferred(
                        div()
                            .id("sidebar-expanded")
                            .absolute()
                            .top_0()
                            .left_0()
                            .h_full()
                            .occlude()
                            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                                if this.panels.hover_panel != *hovered {
                                    this.panels.hover_panel = *hovered;
                                    cx.notify();
                                }
                            }))
                            .child(panel),
                    )
                    .with_priority(1)
                });
                div()
                    .id("sidebar-rail")
                    .relative()
                    .flex_none()
                    .h_full()
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        if this.panels.hover_rail != *hovered {
                            this.panels.hover_rail = *hovered;
                            cx.notify();
                        }
                    }))
                    .child(self.sidebar_panel(true, RAIL_WIDTH, "", cx))
                    .children(overlay)
                    .into_any_element()
            }
        }
    }

    /// The sidebar itself: icons and titles, or icons only (`rail`); `prefix`
    /// keeps element ids apart when both are on screen.
    fn sidebar_panel(
        &mut self,
        rail: bool,
        width: f32,
        prefix: &'static str,
        cx: &mut Context<Self>,
    ) -> Div {
        let current = self.router.route().clone();
        let colors = AppColors::get(cx);
        let chrome = self.chrome_bg(cx);
        let theme = cx.theme();
        let (bg, border, muted, fg, hover, active_bg) = (
            chrome,
            theme.sidebar_border,
            theme.muted_foreground,
            theme.sidebar_foreground,
            theme.list_hover,
            theme.sidebar_accent,
        );

        let entry =
            |id: SharedString, icon: Lucide, title: Key, route: Route, cx: &mut Context<Self>| {
                let active = route.same_page(&current);
                h_flex()
                    .id(id)
                    .px_3()
                    .py(crate::theme::space(7.))
                    .gap_3()
                    .rounded(crate::theme::radius(8.))
                    .text_sm()
                    .cursor_pointer()
                    .when(rail, |this| this.justify_center())
                    .text_color(if active { fg } else { muted })
                    .when(active, |this| {
                        this.bg(active_bg).font_weight(FontWeight::MEDIUM)
                    })
                    .when(!active, |this| {
                        this.hover(move |s| s.bg(hover).text_color(fg))
                    })
                    .child(
                        div()
                            .text_color(if active { colors.accent } else { muted })
                            .child(Icon::new(icon).small()),
                    )
                    // One line in the row's remaining width: GPUI wraps / truncates
                    // by summing unshaped per-character widths, which overestimates
                    // Devanagari; in a box exactly as wide as the shaped text a final
                    // vowel sign would wrap or be cut off.
                    .when(!rail, |this| {
                        this.child(div().flex_1().min_w_0().truncate().child(text(title)))
                    })
                    .when(rail, |this| {
                        this.tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(text(title)).build(window, cx)
                        })
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.navigate(route.clone(), window, cx)
                    }))
            };
        let group_label = |label: Key| {
            if rail {
                // A thin divider instead of the group's name.
                return div().mx_3().mt_3().mb_1().h(px(1.)).bg(border);
            }
            div()
                .px_3()
                .pt_4()
                .pb_1()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(muted)
                .truncate()
                .child(text(label))
        };

        // Home stays in the sidebar too, also with the custom top bar's Home button.
        let mut list = v_flex().gap_0p5();
        for (i, item) in NAV.iter().enumerate() {
            match item {
                NavItem::Group(label) => list = list.child(group_label(*label)),
                NavItem::Page { title, icon, route } => {
                    list = list.child(entry(
                        SharedString::from(format!("{prefix}nav-{i}")),
                        *icon,
                        *title,
                        route(),
                        cx,
                    ))
                }
            }
        }

        v_flex()
            .w(px(width))
            .flex_none()
            .h_full()
            .bg(bg)
            .border_r_1()
            .border_color(border)
            .px_2()
            .py_3()
            .child(
                h_flex()
                    .px_3()
                    .pb_2()
                    .gap_2()
                    .when(rail, |this| this.px_0().justify_center())
                    .child(crate::logo::logo(26.))
                    .when(!rail, |this| {
                        this.child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::BOLD)
                                .child(crate::identity::APP_NAME),
                        )
                    }),
            )
            .child(
                div()
                    .id(SharedString::from(format!("{prefix}sidebar-scroll")))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(list),
            )
            .child(div().pt_2().border_t_1().border_color(border).child(entry(
                SharedString::from(format!("{prefix}nav-settings")),
                Lucide::Settings,
                k!("nav.settings"),
                Route::Settings {
                    section: match &current {
                        Route::Settings { section } => *section,
                        _ => SectionId::default(),
                    },
                },
                cx,
            )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dragging_stays_in_range() {
        assert_eq!(dragged_width(PanelEdge::Sidebar, 200., 216., 250.), 266.);
        assert_eq!(
            dragged_width(PanelEdge::Sidebar, 200., 216., 0.),
            SIDEBAR_WIDTHS.0
        );
        assert_eq!(
            dragged_width(PanelEdge::Sidebar, 200., 216., 900.),
            SIDEBAR_WIDTHS.1
        );
        // The queue panel sits on the right: moving left widens it.
        assert_eq!(dragged_width(PanelEdge::Queue, 900., 340., 860.), 380.);
        assert_eq!(
            dragged_width(PanelEdge::Queue, 900., 340., 2000.),
            QUEUE_WIDTHS.0
        );
        assert!((SIDEBAR_WIDTHS.0..=SIDEBAR_WIDTHS.1).contains(&SIDEBAR_WIDTH));
    }
}
