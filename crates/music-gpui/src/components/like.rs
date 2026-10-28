//! The heart: shows whether a song is liked (local library) and toggles it
//! (`MusicApp::toggle_like`, which also rates the song on YouTube Music when
//! signed in).

use gpui::{App, ElementId, Hsla, Styled as _};
use gpui_component::{
    Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
};
use music_core::Track;

use crate::app::update_app;
use crate::icons::{AppIcon, Lucide};
use crate::store::is_liked;
use crate::theme::AppColors;

/// Ghost icon button; `color` overrides the outline colour (e.g. on Now Playing).
pub fn like_button(
    id: impl Into<ElementId>,
    track: &Track,
    color: Option<Hsla>,
    cx: &App,
) -> Button {
    let liked = is_liked(&track.video_id);
    let icon = if liked {
        Icon::new(AppIcon::HeartFilled).text_color(AppColors::get(cx).accent)
    } else {
        let icon = Icon::new(Lucide::Heart);
        match color {
            Some(c) => icon.text_color(c),
            None => icon,
        }
    };
    let track = track.clone();
    Button::new(id)
        .ghost()
        .small()
        .icon(icon)
        .tooltip(if liked {
            t!("common.remove_from_liked")
        } else {
            t!("common.like")
        })
        .on_click(move |_, window, cx| {
            // Rows and the bar are clickable themselves.
            cx.stop_propagation();
            let track = track.clone();
            update_app(cx, |app, cx| app.toggle_like(track, window, cx));
        })
}
