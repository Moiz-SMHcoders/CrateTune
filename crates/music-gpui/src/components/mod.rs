//! Reusable view pieces. Everything here is a function or `RenderOnce` struct
//! that pages compose; app-level actions go through [`crate::app::update_app`].
//!
//! * [`track_row::TrackRow`] – a song row with hover actions and the context menu
//!   ([`track_row::track_menu`] builds the same menu for other places).
//! * [`card::Card`] – album / playlist / artist tile; [`shelf`] (horizontal,
//!   scrollable) and [`grid`] lay them out; [`section_header`] titles a block.
//! * [`items`] – InnerTube items: cards / rows / menus for any `Item`, shelves
//!   (quick picks or cards), chips, loading + error states with retry.
//! * [`empty_state`] – centred icon + title + detail.
//! * [`artwork`] – the shared image cache and the artwork element.
//! * [`sidebar`], [`player_bar`], [`queue_panel`] – the shell's fixed parts;
//!   [`progress`] draws the progress / volume tracks (every bar style), [`bar_fx`]
//!   animates them and the bar's play / pause background.
//! * [`like::like_button`] – the heart; [`playlist_dialog`] – "Add to playlist".

pub mod artwork;
pub mod bar_fx;
pub mod card;
pub mod items;
pub mod like;
pub mod np_visualizer;
pub mod player_bar;
pub mod playlist_dialog;
pub mod progress;
pub mod queue_panel;
pub mod sidebar;
pub mod track_row;

use gpui::{
    AnyElement, App, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::{ActiveTheme as _, Icon, h_flex, scroll::ScrollableElement as _, v_flex};

use crate::icons::Lucide;

/// Title (+ optional subtitle and trailing element) above a shelf or list.
pub fn section_header(
    title: impl Into<SharedString>,
    subtitle: Option<SharedString>,
    trailing: Option<AnyElement>,
    cx: &App,
) -> impl IntoElement {
    h_flex()
        .w_full()
        .items_end()
        .justify_between()
        .gap_4()
        .child(
            v_flex()
                .min_w_0()
                .gap_0p5()
                .children(subtitle.map(|s| {
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(s)
                }))
                .child(
                    div()
                        .text_xl()
                        .font_weight(FontWeight::BOLD)
                        .truncate()
                        .child(title.into()),
                ),
        )
        .children(trailing)
}

/// Page title row (large, with an optional detail line).
pub fn page_title(
    title: impl Into<SharedString>,
    detail: Option<SharedString>,
    cx: &App,
) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(
            div()
                .text_3xl()
                .font_weight(FontWeight::BOLD)
                .child(title.into()),
        )
        .children(detail.map(|d| {
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(d)
        }))
}

/// Centred placeholder: icon, title, explanation.
pub fn empty_state(
    icon: Lucide,
    title: impl Into<SharedString>,
    detail: impl Into<SharedString>,
    cx: &App,
) -> gpui::Div {
    let theme = cx.theme();
    v_flex()
        .w_full()
        .py(px(56.))
        .items_center()
        .justify_center()
        .gap_2()
        .child(
            div()
                .size(px(64.))
                .rounded_full()
                .bg(theme.muted)
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child(Icon::new(icon).size(px(28.))),
        )
        .child(
            div()
                .pt_2()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.into()),
        )
        .child(
            div()
                .max_w(px(460.))
                .text_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(detail.into()),
        )
}

/// A horizontally scrolling row of cards under a section header.
pub fn shelf(
    id: impl Into<SharedString>,
    title: impl Into<SharedString>,
    subtitle: Option<SharedString>,
    items: Vec<AnyElement>,
    cx: &App,
) -> impl IntoElement {
    let id: SharedString = id.into();
    v_flex()
        .w_full()
        .gap_3()
        .child(section_header(title, subtitle, None, cx))
        .child(
            div()
                .id(id)
                .w_full()
                .overflow_x_scroll()
                .child(h_flex().gap_4().pb_2().children(items)),
        )
}

/// Cards wrapped into rows.
pub fn grid(items: Vec<AnyElement>) -> impl IntoElement {
    h_flex().w_full().flex_wrap().gap_4().children(items)
}

/// Standard padded, vertically scrolling page body.
pub fn page_scroll(
    id: impl Into<SharedString>,
    scroll: &gpui::ScrollHandle,
    content: impl IntoElement,
) -> impl IntoElement {
    let id: SharedString = id.into();
    div()
        .size_full()
        .relative()
        .child(
            div()
                .id(id)
                .size_full()
                .overflow_y_scroll()
                .track_scroll(scroll)
                .child(v_flex().px_8().py_6().gap_8().child(content)),
        )
        .vertical_scrollbar(scroll)
}
