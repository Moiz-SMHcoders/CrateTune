//! Privacy section: pause / clear the listen history and the search history
//! (both live only in the local library database).

use gpui::{AnyElement, Context, IntoElement, ParentElement as _, Styled as _, Window};
use gpui_component::{Sizable as _, WindowExt as _, button::Button, switch::Switch, v_flex};

use super::SettingsPanel;
use super::widgets::{group, note, row};
use crate::app::update_app;
use crate::icons::Lucide;

pub(super) fn render(
    panel: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let prefs = panel.settings.library.clone();
    let pause_listen = Switch::new("privacy-pause-listen")
        .checked(prefs.pause_listen_history)
        .on_click(cx.listener(|this, checked: &bool, _, cx| {
            let on = *checked;
            this.update_library(|l| l.pause_listen_history = on, cx);
        }));
    let pause_search = Switch::new("privacy-pause-search")
        .checked(prefs.pause_search_history)
        .on_click(cx.listener(|this, checked: &bool, _, cx| {
            let on = *checked;
            this.update_library(|l| l.pause_search_history = on, cx);
        }));
    let clear_listen = Button::new("privacy-clear-listen")
        .outline()
        .small()
        .icon(Lucide::Trash)
        .label(t!("settings.privacy.clear_button"))
        .on_click(|_, window, cx| {
            window.open_alert_dialog(cx, |alert, _, _| {
                alert
                    .title(t!("history.clear.title"))
                    .description(t!("settings.privacy.clear_listen_confirm"))
                    .show_cancel(true)
                    .ok_text(t!("common.clear"))
                    .on_ok(|_, window, cx| {
                        update_app(cx, |app, cx| app.clear_listen_history(window, cx));
                        true
                    })
            });
        });
    let clear_search = Button::new("privacy-clear-search")
        .outline()
        .small()
        .icon(Lucide::Trash)
        .label(t!("common.clear"))
        .on_click(|_, window, cx| {
            update_app(cx, |app, cx| app.clear_search_history(window, cx));
        });
    v_flex()
        .gap_5()
        .child(
            group(t!("settings.privacy.listen_history"), cx)
                .child(row(
                    t!("settings.privacy.pause_listen"),
                    Some(t!("settings.privacy.pause_listen_detail")),
                    pause_listen,
                    cx,
                ))
                .child(row(
                    t!("settings.privacy.clear_listen"),
                    Some(t!("settings.privacy.clear_listen_detail")),
                    clear_listen,
                    cx,
                )),
        )
        .child(
            group(t!("settings.privacy.search_history"), cx)
                .child(row(
                    t!("settings.privacy.pause_search"),
                    Some(t!("settings.privacy.pause_search_detail")),
                    pause_search,
                    cx,
                ))
                .child(row(
                    t!("settings.privacy.clear_search"),
                    Some(t!("settings.privacy.clear_search_detail")),
                    clear_search,
                    cx,
                )),
        )
        .child(note(t!("settings.privacy.note"), cx))
        .into_any_element()
}
