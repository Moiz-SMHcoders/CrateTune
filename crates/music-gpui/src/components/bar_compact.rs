//! The player bar on narrow windows (BACKLOG M11, after refryt's compact
//! player bar, GPL-3.0, `crates/refryt/src/ui/player_bar.rs`, and Zuno's
//! compact / expanded bar, Apache-2.0; no code copied): below
//! [`COMPACT_BELOW`] the track info takes the room, previous / play / next sit
//! beside the queue, Now Playing, mute and "More controls" buttons, and the
//! progress is a line along the bar's top (time on hover). "More controls"
//! shows the full bar (the items that fit, as before) until "Fewer controls";
//! the choice lasts for the session. Also the podcast buttons the bar shows for
//! episodes (`player_extras.rs`).

use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{
    AnyElement, App, Context, Div, InteractiveElement as _, IntoElement, ParentElement as _,
    Styled as _, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Disableable as _, IconName, Selectable as _, Sizable as _, Size,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::DropdownMenu as _,
};

use super::player_bar::{BarLayout, LayoutOptions, bar_button_style};
use crate::app::MusicApp;

/// Narrower windows collapse the bar (unless expanded).
pub const COMPACT_BELOW: f32 = 1000.;
/// Height of the collapsed bar's progress line (its hit area).
pub const LINE_HEIGHT: f32 = 12.;

/// "More controls" was pressed (this session).
static EXPANDED: AtomicBool = AtomicBool::new(false);

pub fn expanded() -> bool {
    EXPANDED.load(Ordering::Relaxed)
}

pub fn set_expanded(on: bool) {
    EXPANDED.store(on, Ordering::Relaxed);
}

/// The layout options for the bar now.
pub fn options(episode: bool) -> LayoutOptions {
    LayoutOptions {
        episode,
        collapse: !expanded(),
    }
}

/// The parts the bar view builds, arranged collapsed.
pub struct CompactParts {
    pub layers: Vec<AnyElement>,
    pub left: AnyElement,
    pub transport: AnyElement,
    pub right: AnyElement,
    pub volume: AnyElement,
    pub progress: AnyElement,
    pub tooltip: Option<AnyElement>,
}

/// The collapsed bar: track info | transport | buttons, the progress line on top.
pub fn compact_bar(
    parts: CompactParts,
    layout: &BarLayout,
    edge: bool,
    theme: &gpui_component::Theme,
) -> gpui::Stateful<Div> {
    const PAD: f32 = 16.;
    div()
        .id("player-bar")
        .relative()
        .h(px(super::player_bar::BAR_HEIGHT))
        .w_full()
        .flex_none()
        .overflow_hidden()
        .border_t_1()
        .border_color(theme.border)
        .bg(theme.secondary)
        .children(parts.layers)
        .child(
            h_flex()
                .absolute()
                .inset_0()
                .px(px(PAD))
                .gap(px(PAD))
                .child(
                    div()
                        .when(edge, |this| this.ml(px(-PAD)))
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .child(parts.left),
                )
                .child(
                    div()
                        .w(px(layout.center))
                        .h(px(42.))
                        .flex_none()
                        .child(parts.transport),
                )
                .child(
                    h_flex()
                        .w(px(layout.side))
                        .h_full()
                        .flex_none()
                        .gap_1()
                        .child(div().flex_1().min_w_0().h_full().child(parts.right))
                        .child(parts.volume),
                ),
        )
        // The progress line along the top edge; the time shows on hover.
        .child(
            div()
                .absolute()
                .top(px(-LINE_HEIGHT * 0.5 + 1.))
                .left_0()
                .right_0()
                .h(px(LINE_HEIGHT))
                .child(parts.progress),
        )
        .child(
            div()
                .absolute()
                .top(px(LINE_HEIGHT * 0.5))
                .left_0()
                .right_0()
                .h(px(44.))
                .children(parts.tooltip),
        )
}

/// "More controls" (collapsed) / "Fewer controls" (expanded on a narrow window).
pub fn more_button(compact: bool, cx: &mut Context<MusicApp>) -> Button {
    Button::new("bar-more")
        .custom(bar_button_style(cx))
        .with_size(Size::Medium)
        .icon(IconName::Ellipsis)
        .selected(!compact)
        .tooltip(if compact {
            t!("np_extras.bar.more")
        } else {
            t!("np_extras.bar.fewer")
        })
        .on_click(cx.listener(move |_, _, _, cx| {
            set_expanded(compact);
            // The bar view re-renders with the app.
            cx.notify();
        }))
}

/// A podcast's 15-second skip back / forward.
pub fn skip_button(forward: bool, enabled: bool, cx: &mut Context<MusicApp>) -> Button {
    let (id, path, tip, seconds) = if forward {
        (
            "bar-skip-forward",
            crate::player_extras::SKIP_FORWARD_PATH,
            t!("np_extras.skip_forward"),
            crate::player_extras::SKIP_SECS,
        )
    } else {
        (
            "bar-skip-back",
            crate::player_extras::SKIP_BACK_PATH,
            t!("np_extras.skip_back"),
            -crate::player_extras::SKIP_SECS,
        )
    };
    Button::new(id)
        .custom(bar_button_style(cx))
        .with_size(px(24.))
        .w(px(32.))
        .h(px(32.))
        .rounded(px(999.))
        .icon(gpui_component::Icon::empty().path(path))
        .tooltip(tip)
        .disabled(!enabled)
        .on_click(cx.listener(move |this, _, _, cx| this.seek_by(seconds, cx)))
}

/// A podcast's speed (the app's playback speed) as a small menu.
pub fn speed_button(speed: f32, cx: &App) -> AnyElement {
    Button::new("bar-speed")
        .custom(bar_button_style(cx))
        .with_size(Size::Medium)
        .rounded(px(999.))
        .label(crate::player_extras::speed_label(speed))
        .tooltip(t!("np_extras.speed"))
        .selected((speed - 1.).abs() > 0.01)
        .dropdown_menu(move |menu, _, _| crate::player_extras::speed_menu(menu, speed))
        .into_any_element()
}

impl MusicApp {
    /// `@bar-compact=on|off`: collapse / expand the narrow bar.
    pub(crate) fn set_bar_expanded(&mut self, on: bool, cx: &mut Context<Self>) {
        set_expanded(on);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::super::player_bar::{bar_layout, bar_layout_with};
    use super::*;
    use crate::settings::bar_items::BarItems;

    #[test]
    fn narrow_windows_collapse_unless_expanded() {
        let items = BarItems::default();
        let collapse = LayoutOptions {
            episode: false,
            collapse: true,
        };
        let narrow = bar_layout_with(900., false, false, true, items, collapse);
        assert!(narrow.compact && narrow.more);
        assert!(narrow.queue && narrow.now_playing && narrow.slider.is_none());
        assert!(!narrow.lyrics && !narrow.mini && !narrow.shuffle && !narrow.source_chip);
        // An armed sleep timer and a Cast session stay.
        let armed = bar_layout_with(900., true, true, true, items, collapse);
        assert!(armed.sleep && armed.cast && armed.side > narrow.side);
        // Expanded: the full bar, with the button to collapse it again.
        let expanded = bar_layout(900., false, false, true, items);
        assert!(!expanded.compact && expanded.more);
        // Wide windows never collapse and have no such button.
        let wide = bar_layout_with(1280., false, false, true, items, collapse);
        assert!(!wide.compact && !wide.more);
    }

    #[test]
    fn episodes_get_skips_and_speed() {
        let items = BarItems::default();
        let episode = LayoutOptions {
            episode: true,
            collapse: true,
        };
        let wide = bar_layout_with(1440., false, false, true, items, episode);
        assert!(wide.skips && wide.speed && !wide.shuffle && wide.repeat);
        // The skips stay when the toggles no longer fit.
        let tight = bar_layout_with(1000., false, false, true, items, episode);
        assert!(tight.skips);
        let collapsed = bar_layout_with(900., false, false, true, items, episode);
        assert!(collapsed.compact && collapsed.skips && !collapsed.speed);
        // A song: neither.
        let song = bar_layout_with(1440., false, false, true, items, LayoutOptions::default());
        assert!(!song.skips && !song.speed && song.shuffle);
    }
}
