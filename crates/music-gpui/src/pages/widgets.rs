//! Small pieces shared by the library pages: a sort menu button, a filter
//! input, a placeholder art tile, and byte / duration formatting.

use gpui::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
};

use crate::icons::Lucide;

/// "Sort: Title ↑" button with a menu of `options`; picking one calls `pick`
/// (picking the current one flips the direction, which the page handles).
pub fn sort_button<P: Render, S: Copy + PartialEq + 'static>(
    id: &'static str,
    options: &'static [(S, &'static str)],
    current: S,
    descending: bool,
    pick: fn(&mut P, S, &mut Context<P>),
    cx: &mut Context<P>,
) -> impl IntoElement + use<P, S> {
    let label = options
        .iter()
        .find(|(s, _)| *s == current)
        .map_or("Sort", |(_, l)| l);
    let page = cx.entity().downgrade();
    Button::new(id)
        .ghost()
        .small()
        .icon(if descending {
            Lucide::SortDescending
        } else {
            Lucide::SortAscending
        })
        .label(label)
        .dropdown_menu(move |mut menu, _, _| {
            for &(option, label) in options {
                let page = page.clone();
                menu = menu.item(
                    PopupMenuItem::new(label)
                        .checked(option == current)
                        .on_click(move |_, _, cx| {
                            page.update(cx, |p, cx| {
                                pick(p, option, cx);
                                cx.notify();
                            })
                            .ok();
                        }),
                );
            }
            menu
        })
}

/// A small search field; `on_change` gets the trimmed text on every edit.
pub fn filter_input<P: 'static>(
    placeholder: &str,
    window: &mut Window,
    cx: &mut Context<P>,
    on_change: fn(&mut P, String, &mut Context<P>),
) -> (Entity<InputState>, Subscription) {
    let placeholder = placeholder.to_owned();
    let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
    let sub = cx.subscribe(&input, move |page, input, event: &InputEvent, cx| {
        if matches!(event, InputEvent::Change) {
            let text = input.read(cx).value().trim().to_owned();
            on_change(page, text, cx);
            cx.notify();
        }
    });
    (input, sub)
}

pub fn filter_field(input: &Entity<InputState>) -> impl IntoElement + use<> {
    div().w(px(220.)).child(
        Input::new(input)
            .small()
            .prefix(gpui_component::IconName::Search)
            .cleanable(true),
    )
}

/// Square placeholder with an icon (playlists without artwork, local albums).
pub fn placeholder_art(icon: Lucide, side: f32, cx: &gpui::App) -> gpui::Div {
    let theme = cx.theme();
    div()
        .size(px(side))
        .flex_none()
        .rounded(px(8.))
        .bg(theme.muted)
        .flex()
        .items_center()
        .justify_center()
        .text_color(theme.muted_foreground)
        .child(Icon::new(icon).size(px(side * 0.4)))
}

/// `1.2 GB`, `640 KB` (locale-aware, see [`crate::i18n::format::bytes`]).
pub fn format_bytes(bytes: u64) -> String {
    crate::i18n::format::bytes(bytes)
}

/// `3 h 12 min`, `45 min`, `20 s`.
pub fn format_listening(ms: u64) -> String {
    crate::i18n::format::listening(ms)
}

/// "1 song" / "12 songs".
pub fn songs_label(n: impl Into<u64>) -> SharedString {
    tp!("common.songs", n.into()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(300 * 1024 * 1024), "300 MB");
        assert_eq!(format_listening(20_000), "20 s");
        assert_eq!(format_listening(45 * 60_000), "45 min");
        assert_eq!(format_listening(3 * 3_600_000 + 12 * 60_000), "3 h 12 min");
        assert_eq!(songs_label(1u32), "1 song");
    }
}
