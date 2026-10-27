//! Account section: YouTube Music sign-in (embedded Google sign-in window on
//! macOS, or a pasted `cookie` header everywhere), the signed-in account with
//! its channels / Google account index, library sync and sign-out
//! (`account.rs` does the work).

use std::cell::Cell;

use gpui::{
    AnyElement, App, AppContext as _, Context, Focusable as _, FontWeight, IntoElement,
    ParentElement as _, SharedString, Styled as _, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    spinner::Spinner,
    v_flex,
};

use super::SettingsPanel;
use super::widgets::{group, note, row, status_line};
use crate::account::Busy;
use crate::app::{read_app, update_app};
use crate::components::artwork::artwork;
use crate::icons::Lucide;
use crate::util::{local_date, pretty_date};

const COOKIE_STEPS: &str = "1. Open music.youtube.com in your browser and sign in.\n\
     2. Open the developer tools (⌥⌘I in Safari / Chrome) → Network, reload the page.\n\
     3. Click any request to music.youtube.com (e.g. “browse”), find “Cookie” under \
        Request Headers and copy its whole value.\n\
     4. Paste it below. It is stored in the OS keychain and only sent to YouTube.";

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
            .title(div().child("Sign in with a browser cookie"))
            .w(px(560.))
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .whitespace_normal()
                            .child(COOKIE_STEPS),
                    )
                    .child(Input::new(&input))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(muted)
                                    .child("Google account index (if several are signed in):"),
                            )
                            .child(div().w(px(56.)).child(Input::new(&auth_user).small())),
                    ),
            )
            .footer(
                h_flex().justify_end().gap_2().child(
                    Button::new("cookie-sign-in")
                        .primary()
                        .label("Sign in")
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
    let Some((signed_in, status, sync, channels)) = read_app(cx, |app, _| {
        let a = &app.store.account;
        (
            a.is_signed_in(),
            a.status.clone(),
            a.sync.clone(),
            a.channels.clone(),
        )
    }) else {
        return note("Loading…", cx).into_any_element();
    };
    let config = panel.settings.library.account.clone();
    let loading_secret = config.is_some() && !panel.secrets_loaded;
    let mut content = v_flex().gap_5();

    match config.filter(|_| signed_in || loading_secret) {
        None => {
            let mut sign_in = group("YouTube Music", cx)
                .child(note(
                    "Not signed in. Everything works anonymously; signing in syncs your liked \
                     songs, saved albums, subscriptions and playlists into the library, and \
                     likes and playlist edits go back to YouTube Music.",
                    cx,
                ))
                .child(
                    h_flex()
                        .gap_2()
                        .pt_1()
                        .children(cfg!(target_os = "macos").then(|| {
                            Button::new("account-google")
                                .primary()
                                .small()
                                .icon(gpui_component::IconName::Globe)
                                .label("Sign in with Google…")
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
                                .label("Paste cookie…")
                                .on_click(|_, window, cx| open_cookie_dialog(window, cx)),
                        ),
                );
            if cfg!(target_os = "macos") {
                sign_in = sign_in.child(note(
                    "If Google refuses the sign-in window (“This browser or app may not be \
                     secure”), use “Paste cookie”.",
                    cx,
                ));
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
                .map(|t| format!("Last synced {}", pretty_date(&local_date(t))))
                .unwrap_or_else(|| "Not synced yet".into());
            let syncing = matches!(sync, Busy::Working(_));
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
                        .label("Sign out")
                        .on_click(|_, window, cx| {
                            update_app(cx, |app, cx| app.sign_out(window, cx));
                        }),
                );
            content = content.child(
                group("YouTube Music", cx)
                    .child(header)
                    .children(busy_line(&status, cx)),
            );

            let sync_button = Button::new("account-sync")
                .primary()
                .small()
                .icon(gpui_component::IconName::RefreshCw)
                .label(if syncing { "Syncing…" } else { "Sync now" })
                .disabled(syncing || !signed_in)
                .on_click(|_, window, cx| {
                    update_app(cx, |app, cx| app.sync_youtube_library(window, cx));
                });
            content = content.child(
                group("Library sync", cx)
                    .child(row(
                        "Sync your YouTube Music library",
                        Some("Liked songs, library songs, saved albums, subscriptions, playlists."),
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
                        .label(label)
                        .on_click(move |_, window, cx| {
                            let id = id.clone();
                            update_app(cx, |app, cx| app.switch_account(id, auth_user, window, cx));
                        })
                        .into_any_element()
                })
                .collect();
            let index_buttons = h_flex().gap_1().children((0..4u32).map(|n| {
                let channel = selected_channel.clone();
                Button::new(("account-authuser", n as usize))
                    .small()
                    .outline()
                    .selected(n == auth_user)
                    .label(n.to_string())
                    .on_click(move |_, window, cx| {
                        let channel = channel.clone();
                        update_app(cx, |app, cx| app.switch_account(channel, n, window, cx));
                    })
            }));
            let mut accounts = group("Accounts", cx);
            if channel_rows.len() > 1 {
                accounts = accounts
                    .child(note(
                        "Channels (brand accounts) of this Google account:",
                        cx,
                    ))
                    .child(h_flex().gap_1().flex_wrap().children(channel_rows));
            }
            accounts = accounts.child(row(
                "Google account",
                Some("When the cookie holds several Google accounts: which one (0 = first)."),
                index_buttons,
                cx,
            ));
            content = content.child(accounts);
        }
    }
    content
        .child(note(
            "Signing in uses your browser session cookie for music.youtube.com. It is kept in \
             the OS keychain and sent only to YouTube. Account features were built against \
             YouTube's web API but not yet verified with a real account.",
            cx,
        ))
        .into_any_element()
}
