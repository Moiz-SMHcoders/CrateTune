//! The thumbs down: shows whether a song is disliked and toggles it
//! (`MusicApp::toggle_dislike`, which also rates the song on YouTube Music when
//! signed in and may skip it). The player bar's button (an optional bar item, off
//! by default) and Now Playing's plain icon button; track menus have an entry.

use gpui::{
    App, Div, ElementId, Hsla, InteractiveElement as _, ParentElement as _, Pixels,
    StatefulInteractiveElement as _, Styled as _, div, svg,
};
use gpui_component::{
    Icon, IconNamed as _, Sizable as _,
    button::{Button, ButtonVariants as _},
};
use music_core::Track;

use crate::app::update_app;
use crate::icons::{AppIcon, Lucide};
use crate::store::is_disliked;
use crate::theme::AppColors;

fn tooltip(disliked: bool) -> &'static str {
    if disliked {
        t!("common.remove_dislike")
    } else {
        t!("common.dislike")
    }
}

/// Ghost icon button (the player bar).
pub fn dislike_button(id: impl Into<ElementId>, track: &Track, cx: &App) -> Button {
    let disliked = is_disliked(&track.video_id);
    let icon = if disliked {
        Icon::new(AppIcon::ThumbsDownFilled).text_color(AppColors::get(cx).accent)
    } else {
        Icon::new(Lucide::ThumbsDown)
    };
    let track = track.clone();
    Button::new(id)
        .ghost()
        .small()
        .icon(icon)
        .tooltip(tooltip(disliked))
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            let track = track.clone();
            update_app(cx, |app, cx| app.toggle_dislike(track, window, cx));
        })
}

/// Now Playing: a plain icon button like its heart (`now_playing::like_icon_button`).
pub fn dislike_icon_button(
    id: &'static str,
    track: &Track,
    side: Pixels,
    idle: Hsla,
    hover: Hsla,
    cx: &App,
) -> gpui::Stateful<Div> {
    let disliked = is_disliked(&track.video_id);
    let (path, color) = if disliked {
        (AppIcon::ThumbsDownFilled.path(), AppColors::get(cx).accent)
    } else {
        (Lucide::ThumbsDown.path(), idle)
    };
    let tip = tooltip(disliked);
    let track = track.clone();
    div()
        .id(id)
        .size(side)
        .flex_none()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tip).build(window, cx))
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            let track = track.clone();
            update_app(cx, |app, cx| app.toggle_dislike(track, window, cx));
        })
        .child(svg().size(side * 0.45).path(path).text_color(color))
}
