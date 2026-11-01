//! Left navigation: app title, the browse / library entries from
//! [`crate::pages::NAV`] (without Home when the custom top bar shows it), and
//! Settings at the bottom.
//!
//! Appearance → Layout → Sidebar: **Full** (icons and titles; drag its right
//! edge for the width, 176–360 px), **Icons only** (a 68 px rail with tooltips)
//! or **Expand on hover** (the rail; hovering it slides the full sidebar out over
//! the page). A button in the sidebar's header collapses it to the rail and back.
//! Switching between the full sidebar and the rail, and the hover panel, slide
//! with an eased width (`anim::Tween`): frames only while it moves (~200 ms, per
//! Settings → Animations), none at rest. The queue panel's left edge drags the
//! same way as the sidebar's. A drag re-renders the shell per mouse move and saves
//! the width once, on release.

use gpui::{
    AnyElement, App, Context, Div, FontWeight, InteractiveElement as _, IntoElement, MouseButton,
    MouseMoveEvent, ParentElement as _, Pixels, SharedString, StatefulInteractiveElement as _,
    Styled as _, deferred, div, prelude::FluentBuilder as _, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};

use crate::anim::{Frame, Spec, Tween, ease_out_cubic};
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
/// Sliding out / back in.
const EXPAND: Spec = Spec::ms(200, ease_out_cubic);
const COLLAPSE: Spec = Spec::ms(170, ease_out_cubic);
/// Expand on hover waits this long, so passing over the rail doesn't open it.
const HOVER_DELAY: std::time::Duration = std::time::Duration::from_millis(70);
/// A rail entry's icon sits this much further right than a full one's (it is
/// centred in the rail); the sliding sidebar moves it between the two.
const RAIL_NUDGE: f32 = 6.;

/// Which panel's edge is dragged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelEdge {
    Sidebar,
    Queue,
}

/// Hover, drag and slide state of the sidebar and the queue panel.
#[derive(Default)]
pub struct Panels {
    /// Expand on hover: the pointer is over the rail / over the expanded panel.
    hover_rail: bool,
    hover_panel: bool,
    /// A drag in progress: the edge, where it started (x) and the width then.
    drag: Option<(PanelEdge, Pixels, f32)>,
    /// The width while dragging (saved on release).
    live: Option<(PanelEdge, f32)>,
    /// The sidebar column's width in the layout (full ↔ rail), and the hover
    /// panel's width over the page; created at the first render.
    column: Option<Tween<f32>>,
    overlay: Option<Tween<f32>>,
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

    pub(crate) fn render_sidebar(
        &mut self,
        frame: &mut Frame,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mode = self.sidebar_mode(cx);
        let full = self.full_sidebar_width(cx);
        let now = frame.now;

        // The column in the layout slides between the full width and the rail.
        let target = if mode == SidebarMode::Full {
            full
        } else {
            RAIL_WIDTH
        };
        let dragging = self.panels.drag.is_some();
        let column = self.panels.column.get_or_insert_with(|| Tween::new(target));
        if dragging {
            // Follows the pointer as it is.
            *column = Tween::new(target);
        } else {
            let spec = if target > RAIL_WIDTH {
                EXPAND
            } else {
                COLLAPSE
            };
            column.animate_to(target, spec, now);
        }
        let width = frame.get(column);
        let sliding = (width - target).abs() > 0.5;
        let shown = if sliding {
            // On its way: the full sidebar, clipped, its titles fading.
            div()
                .flex_none()
                .h_full()
                .w(px(width))
                .overflow_hidden()
                .child(self.sidebar_panel(false, full, labels(width, full), "", cx))
                .into_any_element()
        } else if mode == SidebarMode::Full {
            let handle = self.panel_resize_handle(PanelEdge::Sidebar, cx);
            div()
                .relative()
                .flex_none()
                .h_full()
                .child(self.sidebar_panel(false, full, 1., "", cx))
                .child(handle)
                .into_any_element()
        } else {
            div()
                .flex_none()
                .h_full()
                .child(self.sidebar_panel(true, RAIL_WIDTH, 0., "", cx))
                .into_any_element()
        };
        if mode != SidebarMode::Hover {
            self.panels.overlay = None;
            return shown;
        }

        // Expand on hover: the full sidebar slides out over the page.
        let expanded = self.panels.expanded();
        let overlay = self
            .panels
            .overlay
            .get_or_insert_with(|| Tween::new(RAIL_WIDTH));
        if expanded {
            overlay.animate_to(full, EXPAND.delayed(HOVER_DELAY), now);
        } else {
            overlay.animate_to(RAIL_WIDTH, COLLAPSE, now);
        }
        let over = frame.get(overlay);
        let panel = (over > RAIL_WIDTH + 0.5).then(|| {
            let bg = cx.theme().sidebar;
            let panel = self.sidebar_panel(false, full, labels(over, full), "hover-", cx);
            // Painted after the page, over it.
            deferred(
                div()
                    .id("sidebar-expanded")
                    .absolute()
                    .top_0()
                    .left_0()
                    .h_full()
                    .w(px(over))
                    .overflow_hidden()
                    .bg(bg)
                    .shadow_xl()
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
            .child(shown)
            .children(panel)
            .into_any_element()
    }

    /// Settings → Appearance → Layout → Sidebar, from the sidebar's own button.
    fn set_sidebar(&mut self, mode: SidebarMode, cx: &mut Context<Self>) {
        self.panels.hover_rail = false;
        self.panels.hover_panel = false;
        self.settings
            .update(cx, |s, cx| s.set_sidebar_mode(mode, cx));
    }

    /// The sidebar itself: icons and titles, or icons only (`rail`); `prefix`
    /// keeps element ids apart when both are on screen. `labels` is how visible the
    /// titles are (1 at rest, between 0 and 1 while sliding), which also moves the
    /// icons towards where the rail shows them.
    fn sidebar_panel(
        &mut self,
        rail: bool,
        width: f32,
        labels: f32,
        prefix: &'static str,
        cx: &mut Context<Self>,
    ) -> Div {
        let mode = self.sidebar_mode(cx);
        let nudge = if rail { 0. } else { RAIL_NUDGE * (1. - labels) };
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
                    .when(nudge > 0., |this| this.pl(px(12. + nudge)))
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
                        this.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .when(labels < 1., |d| d.opacity(labels))
                                .child(text(title)),
                        )
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
        // The rail shows a thin divider instead of the group's name, in a box as
        // tall as the name's, so nothing moves up or down while the sidebar slides.
        let group_label = |label: Key| {
            let shown = if rail { 0. } else { labels };
            div()
                .relative()
                .px_3()
                .pt_4()
                .pb_1()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(muted.opacity(shown))
                .truncate()
                .child(text(label))
                .when(shown < 1., |this| {
                    this.child(
                        div()
                            .absolute()
                            .left_3()
                            .right_3()
                            .bottom(px(10.))
                            .h(px(1.))
                            .bg(border.opacity(1. - shown)),
                    )
                })
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
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_lg()
                                .font_weight(FontWeight::BOLD)
                                .when(labels < 1., |d| d.opacity(labels))
                                .child(crate::identity::APP_NAME),
                        )
                        .child(self.sidebar_toggle(mode, prefix, labels, cx))
                    }),
            )
            .when(rail && mode == SidebarMode::Rail, |this| {
                this.child(
                    h_flex()
                        .justify_center()
                        .pb_2()
                        .child(self.sidebar_toggle(mode, prefix, 1., cx)),
                )
            })
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

impl MusicApp {
    /// The header's button: collapse the full sidebar to the rail, expand the rail,
    /// or keep the hover panel open (Appearance → Layout → Sidebar).
    fn sidebar_toggle(
        &mut self,
        mode: SidebarMode,
        prefix: &'static str,
        labels: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (icon, tip, next) = match mode {
            SidebarMode::Full => (
                Lucide::PanelLeftClose,
                t!("sidebar.collapse"),
                SidebarMode::Rail,
            ),
            SidebarMode::Rail => (
                Lucide::PanelLeftOpen,
                t!("sidebar.expand"),
                SidebarMode::Full,
            ),
            SidebarMode::Hover => (
                Lucide::PanelLeftOpen,
                t!("sidebar.keep_open"),
                SidebarMode::Full,
            ),
        };
        div()
            .flex_none()
            .when(labels < 1., |d| d.opacity(labels))
            .child(
                Button::new(SharedString::from(format!("{prefix}sidebar-toggle")))
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(icon))
                    .tooltip(tip)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_sidebar(next, cx))),
            )
    }
}

/// How visible the titles are at `width` (0 at the rail, 1 at `full`).
fn labels(width: f32, full: f32) -> f32 {
    ((width - RAIL_WIDTH) / (full - RAIL_WIDTH).max(1.)).clamp(0., 1.)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_fade_with_the_width() {
        assert_eq!(labels(RAIL_WIDTH, SIDEBAR_WIDTH), 0.);
        assert_eq!(labels(SIDEBAR_WIDTH, SIDEBAR_WIDTH), 1.);
        let mid = labels((RAIL_WIDTH + SIDEBAR_WIDTH) / 2., SIDEBAR_WIDTH);
        assert!((mid - 0.5).abs() < 1e-4);
        assert_eq!(labels(40., SIDEBAR_WIDTH), 0.);
    }

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
