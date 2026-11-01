//! Appearance → Page tabs: "Show tabs" and "Restore tabs at start" (`page_tabs.rs`).

use gpui::{Context, IntoElement, ParentElement as _};
use gpui_component::{Disableable as _, switch::Switch};

use super::SettingsPanel;
use super::widgets::{group_at, note, row_at};

pub(super) fn render(panel: &SettingsPanel, cx: &mut Context<SettingsPanel>) -> impl IntoElement {
    let config = &panel.settings.tabs;
    let show = Switch::new("page-tabs-show")
        .checked(config.show)
        .on_click(cx.listener(|this, on: &bool, _, cx| this.set_page_tabs_shown(*on, cx)));
    let restore = Switch::new("page-tabs-restore")
        .checked(config.restore)
        .disabled(!config.show)
        .on_click(cx.listener(|this, on: &bool, _, cx| this.set_page_tabs_restore(*on, cx)));
    group_at("appearance.page_tabs", t!("settings.page_tabs.group"), cx)
        .child(row_at(
            "appearance.page_tabs.show",
            t!("settings.page_tabs.show"),
            Some(t!("settings.page_tabs.show_note")),
            show,
            cx,
        ))
        .child(row_at(
            "appearance.page_tabs.restore",
            t!("settings.page_tabs.restore"),
            Some(t!("settings.page_tabs.restore_note")),
            restore,
            cx,
        ))
        .child(note(t!("settings.page_tabs.note"), cx))
}
