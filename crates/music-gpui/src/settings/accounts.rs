//! Account section: YouTube Music sign-in (embedded Google sign-in window on
//! macOS, or a pasted `cookie` header everywhere), the signed-in account with
//! its channels / Google account index, library sync and sign-out
//! (`account.rs` does the work).

use std::cell::Cell;

use gpui::{
    AnyElement, App, AppContext as _, Context, Focusable as _, FontWeight, IntoElement,
    ParentElement as _, SharedString, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    spinner::Spinner,
    v_flex,
};
use music_core::innertube::GoogleAccount;

use super::SettingsPanel;
use super::widgets::{group, group_at, note, row, status_line};
use crate::account::{Busy, GoogleAccountsView};
use crate::app::{read_app, update_app};
use crate::components::artwork::artwork;
use crate::icons::Lucide;
use crate::util::{local_date, pretty_date};

/// One button per Google account of the login: avatar, name, e-mail.
fn google_account_buttons(
    list: &[GoogleAccount],
    current: u32,
    busy: bool,
    cx: &mut App,
) -> Vec<AnyElement> {
    list.iter()
        .map(|account| {
            let n = account.auth_user;
            let info = &account.info;
            let label = match &info.email {
                Some(email) if *email != info.name => format!("{} · {email}", info.name),
                _ => info.name.clone(),
            };
            Button::new(("account-google", n as usize))
                .small()
                .map(|b| {
                    if n == current {
                        b.primary()
                    } else {
                        b.outline()
                    }
                })
                .disabled(busy)
                .child(artwork(info.thumbnail_url.clone(), px(18.), px(9.), cx))
                .child(SharedString::from(label))
                .on_click(move |_, window, cx| {
                    if n == current {
                        return;
                    }
                    // Another Google account: its own default channel.
                    update_app(cx, |app, cx| app.switch_account(None, n, window, cx));
                })
                .into_any_element()
        })
        .collect()
}

/// Opens the paste-cookie dialog.
pub fn open_cookie_dialog(window: &mut Window, cx: &mut App) {
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("SID=…; HSID=…; SAPISID=…; …")
            .masked(true)
    });
    let auth_user = cx.new(|cx| InputState::new(window, cx).default_value("0"));
    let focus_once = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if focus_once.replace(false) {
            let handle = input.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        let muted = cx.theme().muted_foreground;
        let (input2, auth2) = (input.clone(), auth_user.clone());
        dialog
            .title(div().child(t!("settings.account.cookie_title")))
            .w(px(560.))
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .whitespace_normal()
                            .child(t!("settings.account.cookie_steps")),
                    )
                    .child(Input::new(&input))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .text_color(muted)
                                    .child(t!("settings.account.cookie_index")),
                            )
                            .child(
                                div()
                                    .w(px(56.))
                                    .flex_none()
                                    .child(Input::new(&auth_user).small()),
                            ),
                    ),
            )
            .footer(
                h_flex().justify_end().gap_2().child(
                    Button::new("cookie-sign-in")
                        .primary()
                        .label(t!("common.sign_in"))
                        .on_click(move |_, window, cx| {
                            let cookie = input2.read(cx).value().to_string();
                            let index = auth2.read(cx).value().trim().parse().unwrap_or(0);
                            if cookie.trim().is_empty() {
                                return;
                            }
                            update_app(cx, |app, cx| {
                                app.sign_in_with_cookie(cookie, index, window, cx)
                            });
                            window.close_dialog(cx);
                        }),
                ),
            )
    });
}

fn busy_line(busy: &Busy, cx: &App) -> Option<AnyElement> {
    let theme = cx.theme();
    match busy {
        Busy::Idle => None,
        Busy::Working(text) => Some(
            h_flex()
                .gap_2()
                .child(Spinner::new().small())
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(text.clone())),
                )
                .into_any_element(),
        ),
        Busy::Done(text) => {
            Some(status_line(text.clone().into(), theme.muted_foreground).into_any_element())
        }
        Busy::Failed(text) => {
            Some(status_line(text.clone().into(), theme.danger).into_any_element())
        }
    }
}

pub(super) fn render(
    panel: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let Some((signed_in, status, sync, channels, google)) = read_app(cx, |app, _| {
        let a = &app.store.account;
        (
            a.is_signed_in(),
            a.status.clone(),
            a.sync.clone(),
            a.channels.clone(),
            a.google_accounts_view(),
        )
    }) else {
        return note(t!("common.loading"), cx).into_any_element();
    };
    let config = panel.settings.library.account.clone();
    let loading_secret = config.is_some() && !panel.secrets_loaded;
    let mut content = v_flex().gap_5();

    match config.filter(|_| signed_in || loading_secret) {
        None => {
            let mut sign_in = group_at("account.youtube", t!("common.youtube_music"), cx)
                .child(note(t!("settings.account.not_signed_in"), cx))
                .child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .pt_1()
                        .children(cfg!(target_os = "macos").then(|| {
                            Button::new("account-google")
                                .primary()
                                .small()
                                .icon(gpui_component::IconName::Globe)
                                .label(t!("settings.account.sign_in_google"))
                                .on_click(|_, _, cx| {
                                    #[cfg(target_os = "macos")]
                                    crate::login_webview::open(cx);
                                    #[cfg(not(target_os = "macos"))]
                                    let _ = cx;
                                })
                        }))
                        .child(
                            Button::new("account-cookie")
                                .outline()
                                .small()
                                .icon(Lucide::Cookie)
                                .label(t!("settings.account.paste_cookie"))
                                .on_click(|_, window, cx| open_cookie_dialog(window, cx)),
                        ),
                );
            if cfg!(target_os = "macos") {
                sign_in = sign_in.child(note(t!("settings.account.google_refused"), cx));
            }
            content = content.child(sign_in.children(busy_line(&status, cx)));
        }
        Some(account) => {
            let theme = cx.theme();
            let muted = theme.muted_foreground;
            let avatar = artwork(account.thumbnail_url.clone(), px(56.), px(28.), cx);
            let secondary = [account.handle.clone(), account.email.clone()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
            let last_sync = account
                .last_sync_at
                .map(|t| {
                    t!(
                        "settings.account.last_synced",
                        date = pretty_date(&local_date(t))
                    )
                })
                .unwrap_or_else(|| t!("settings.account.not_synced").into());
            let syncing = matches!(sync, Busy::Working(_));
            // A switch (or sign-in check) is running: the account choices wait.
            let busy = matches!(status, Busy::Working(_));
            let auth_user = account.auth_user;
            let selected_channel = account.data_sync_id.clone();
            let header = h_flex()
                .gap_3()
                .child(avatar)
                .child(
                    v_flex()
                        .min_w_0()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(SharedString::from(account.name.clone())),
                        )
                        .child(div().text_sm().text_color(muted).child(secondary)),
                )
                .child(div().flex_1())
                .child(
                    Button::new("account-sign-out")
                        .outline()
                        .small()
                        .icon(Lucide::LogOut)
                        .label(t!("common.sign_out"))
                        .on_click(|_, window, cx| {
                            update_app(cx, |app, cx| app.sign_out(window, cx));
                        }),
                );
            content = content.child(
                group_at("account.youtube", t!("common.youtube_music"), cx)
                    .child(header)
                    .children(busy_line(&status, cx)),
            );

            let sync_button = Button::new("account-sync")
                .primary()
                .small()
                .icon(gpui_component::IconName::RefreshCw)
                .label(if syncing {
                    t!("settings.account.syncing")
                } else {
                    t!("settings.account.sync_now")
                })
                .disabled(syncing || !signed_in)
                .on_click(|_, window, cx| {
                    update_app(cx, |app, cx| app.sync_youtube_library(window, cx));
                });
            content = content.child(
                group_at("account.sync", t!("settings.account.sync_group"), cx)
                    .child(row(
                        t!("settings.account.sync_row"),
                        Some(t!("settings.account.sync_row_desc")),
                        sync_button,
                        cx,
                    ))
                    .child(note(last_sync, cx))
                    .children(busy_line(&sync, cx)),
            );

            let channel_rows: Vec<AnyElement> = channels
                .iter()
                .enumerate()
                .map(|(ix, ch)| {
                    let selected = match &selected_channel {
                        Some(id) => ch.data_sync_id.as_ref() == Some(id),
                        None => ch.selected,
                    };
                    let id = ch.data_sync_id.clone();
                    let label = match &ch.handle {
                        Some(h) => format!("{} ({h})", ch.name),
                        None => ch.name.clone(),
                    };
                    Button::new(("account-channel", ix))
                        .small()
                        .outline()
                        .selected(selected)
                        .disabled(busy)
                        .label(label)
                        .on_click(move |_, window, cx| {
                            let id = id.clone();
                            update_app(cx, |app, cx| app.switch_account(id, auth_user, window, cx));
                        })
                        .into_any_element()
                })
                .collect();
            let mut accounts = group(t!("settings.account.accounts_group"), cx);
            let mut shown = false;
            // Google accounts of the login first: channels belong to the one in use.
            match google {
                GoogleAccountsView::Hidden => {}
                GoogleAccountsView::Single => {
                    accounts = accounts.child(note(t!("settings.account.google_single"), cx));
                    shown = true;
                }
                GoogleAccountsView::Choices(list) => {
                    let choices = google_account_buttons(&list, auth_user, busy, cx);
                    accounts = accounts
                        .child(row(
                            t!("settings.account.google_index"),
                            Some(t!("settings.account.google_index_desc")),
                            div(),
                            cx,
                        ))
                        .child(h_flex().gap_1().flex_wrap().children(choices));
                    shown = true;
                }
            }
            if channel_rows.len() > 1 {
                accounts = accounts
                    .child(note(t!("settings.account.channels"), cx))
                    .child(h_flex().gap_1().flex_wrap().children(channel_rows));
                shown = true;
            }
            if shown {
                content = content.child(accounts);
            }
        }
    }
    content
        .child(note(t!("settings.account.cookie_note"), cx))
        .into_any_element()
}
