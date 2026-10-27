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
        .label("Clear…")
        .on_click(|_, window, cx| {
            window.open_alert_dialog(cx, |alert, _, _| {
                alert
                    .title("Clear listening history?")
                    .description(
                        "Every recorded play is deleted; play counts and stats start over.",
                    )
                    .show_cancel(true)
                    .ok_text("Clear")
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
        .label("Clear")
        .on_click(|_, window, cx| {
            update_app(cx, |app, cx| app.clear_search_history(window, cx));
        });
    v_flex()
        .gap_5()
        .child(
            group("Listen history", cx)
                .child(row(
                    "Pause listen history",
                    Some("Plays aren't recorded; History and Stats stop growing."),
                    pause_listen,
                    cx,
                ))
                .child(row(
                    "Clear listen history",
                    Some("Deletes every recorded play on this computer."),
                    clear_listen,
                    cx,
                )),
        )
        .child(
            group("Search history", cx)
                .child(row(
                    "Pause search history",
                    Some("Searches you submit aren't remembered."),
                    pause_search,
                    cx,
                ))
                .child(row(
                    "Clear search history",
                    Some("Forgets every remembered search."),
                    clear_search,
                    cx,
                )),
        )
        .child(note(
            "History stays in the library database on this computer. Plays are never \
             reported to your YouTube account's history.",
            cx,
        ))
        .into_any_element()
}
