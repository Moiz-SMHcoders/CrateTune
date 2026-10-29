//! Appearance → Player bar → "Show on the bar": a check box per bar item
//! (`bar_items.rs`), plus the panel methods the bar's right-click menu and the
//! scripts use.

use gpui::{AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, px};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex, v_flex,
};

use super::bar_items::{BarItem, BarItems};
use super::panel::{SettingsEvent, SettingsPanel};
use super::widgets::note;

impl SettingsPanel {
    pub fn bar_items(&self) -> BarItems {
        self.settings.player_bar.items
    }

    pub fn set_bar_items(&mut self, items: BarItems, cx: &mut Context<Self>) {
        if self.settings.player_bar.items != items {
            self.settings.player_bar.items = items;
            // The app re-renders the bar on this event.
            cx.emit(SettingsEvent::NowPlaying);
            self.changed(false, cx);
        }
    }

    pub fn set_bar_item(&mut self, item: BarItem, on: bool, cx: &mut Context<Self>) {
        let mut items = self.bar_items();
        items.set(item, on);
        self.set_bar_items(items, cx);
    }
}

/// The rows inside the "Player bar" group.
pub(super) fn render(panel: &mut SettingsPanel, cx: &mut Context<SettingsPanel>) -> AnyElement {
    let items = panel.bar_items();
    let boxes = BarItem::ALL.into_iter().map(|item| {
        h_flex().w(px(200.)).child(
            Checkbox::new(SharedString::from(format!("bar-item-{}", item.key())))
                .checked(items.shows(item))
                .label(item.title())
                .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                    this.set_bar_item(item, *checked, cx)
                })),
        )
    });
    v_flex()
        .gap_2()
        .pt_3()
        .border_t_1()
        .border_color(cx.theme().border)
        .child(
            h_flex()
                .justify_between()
                .gap_4()
                .child(gpui::div().text_sm().child(t!("bar_items.title")))
                .child(
                    Button::new("bar-items-reset")
                        .ghost()
                        .small()
                        .label(t!("bar_items.reset"))
                        .disabled(items == BarItems::default())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.set_bar_items(BarItems::default(), cx)
                        })),
                ),
        )
        .child(h_flex().flex_wrap().gap_x_4().gap_y_2().children(boxes))
        .child(note(t!("bar_items.note"), cx))
        .into_any_element()
}
