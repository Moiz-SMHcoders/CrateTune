//! The Together page's friends card: what your friends are listening to, "Listen
//! together" per group, and the sessions friends invited you to
//! (`integrations/friends.rs`). Drawn from data the page reads from the app.

use gpui::{
    AnyElement, App, FontWeight, IntoElement as _, ParentElement as _, SharedString, Styled as _,
    div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use music_core::handoff::now_unix_ms;

use crate::app::update_app;
use crate::icons::Lucide;
use crate::integrations::friends::{FeedRow, FriendsPanel};
use crate::router::Route;
use crate::settings::SectionId;

fn open_settings(cx: &mut App, window: &mut gpui::Window) {
    update_app(cx, |app, cx| {
        app.navigate(
            Route::Settings {
                section: SectionId::Friends,
            },
            window,
            cx,
        );
    });
}

fn status(row: &FeedRow) -> String {
    let ago = |ms: u64| crate::i18n::format::ago(now_unix_ms().saturating_sub(ms) / 1000);
    match &row.activity {
        Some(a) if a.playing => t!("friends.feed.playing").into(),
        Some(_) => t!("friends.feed.paused", ago = ago(row.at_ms)),
        None if row.at_ms > 0 => t!("friends.feed.idle", ago = ago(row.at_ms)),
        None => t!("friends.feed.unknown").into(),
    }
}

pub fn render(data: FriendsPanel, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let (muted, border) = (theme.muted_foreground, theme.border);
    let card = v_flex()
        .gap_3()
        .p_5()
        .rounded(crate::theme::radius(12.))
        .border_1()
        .border_color(border)
        .child(
            h_flex()
                .gap_2()
                .child(Icon::new(Lucide::UsersRound).size(px(18.)))
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(SharedString::from(t!("friends.panel.title"))),
                ),
        );
    if !data.enabled {
        return card
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child(SharedString::from(t!("friends.panel.off"))),
            )
            .child(
                h_flex().child(
                    Button::new("friends-setup")
                        .small()
                        .outline()
                        .label(t!("friends.panel.set_up"))
                        .on_click(|_, window, cx| open_settings(cx, window)),
                ),
            )
            .into_any_element();
    }

    // Sessions friends started.
    let mut invites = v_flex().gap_2();
    for (ix, (invite, name, group)) in data.invites.into_iter().enumerate() {
        let link = invite.link.clone();
        invites = invites.child(
            h_flex()
                .gap_3()
                .justify_between()
                .p_2()
                .rounded(crate::theme::radius(8.))
                .bg(cx.theme().secondary)
                .child(
                    div()
                        .text_sm()
                        .truncate()
                        .child(SharedString::from(t!(
                            "friends.panel.invite",
                            name = name,
                            group = group
                        ))),
                )
                .child(
                    Button::new(("friends-join-session", ix))
                        .small()
                        .primary()
                        .label(t!("friends.panel.join"))
                        .on_click(move |_, window, cx| {
                            let link = link.clone();
                            update_app(cx, |app, cx| app.friends_join_session(&link, window, cx));
                        }),
                ),
        );
    }

    // The feed.
    let mut feed = v_flex().gap_2();
    if data.feed.is_empty() {
        feed = feed.child(
            div()
                .text_sm()
                .text_color(muted)
                .child(SharedString::from(t!("friends.panel.no_friends"))),
        );
    }
    for row in &data.feed {
        let playing = row.activity.as_ref().is_some_and(|a| a.playing);
        let song = row
            .activity
            .as_ref()
            .map(|a| {
                if a.artist.is_empty() {
                    a.title.clone()
                } else {
                    t!("friends.feed.song", title = a.title, artist = a.artist)
                }
            })
            .unwrap_or_default();
        feed = feed.child(
            h_flex()
                .gap_3()
                .child(
                    div()
                        .size(px(32.))
                        .flex_none()
                        .rounded_full()
                        .bg(cx.theme().secondary)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(SharedString::from(
                            row.name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default(),
                        )),
                )
                .child(
                    v_flex()
                        .min_w_0()
                        .flex_1()
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .truncate()
                                        .child(SharedString::from(row.name.clone())),
                                )
                                .when(playing, |this| {
                                    this.child(
                                        Icon::new(Lucide::AudioLines)
                                            .size(px(14.))
                                            .text_color(crate::theme::AppColors::get(cx).accent),
                                    )
                                }),
                        )
                        .when(!song.is_empty(), |this| {
                            this.child(div().text_sm().truncate().child(SharedString::from(song)))
                        })
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .truncate()
                                .child(SharedString::from(format!(
                                    "{} · {}",
                                    status(row),
                                    row.groups.join(", ")
                                ))),
                        ),
                ),
        );
    }

    // "Listen together" per group.
    let mut groups = h_flex().gap_2().flex_wrap();
    for (ix, (id, name, _)) in data.groups.into_iter().enumerate() {
        groups = groups.child(
            Button::new(("friends-listen", ix))
                .small()
                .outline()
                .icon(Lucide::Radio)
                .label(t!("friends.panel.listen", group = name))
                .on_click(move |_, window, cx| {
                    let id = id.clone();
                    update_app(cx, |app, cx| app.friends_listen_together(&id, window, cx));
                }),
        );
    }

    card.child(invites)
        .child(feed)
        .child(groups)
        .child(
            h_flex()
                .gap_2()
                .justify_between()
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(SharedString::from(if data.sharing {
                            t!("friends.panel.sharing")
                        } else {
                            t!("friends.panel.not_sharing")
                        })),
                )
                .child(
                    Button::new("friends-manage")
                        .ghost()
                        .xsmall()
                        .icon(Lucide::Settings2)
                        .label(t!("friends.panel.manage"))
                        .on_click(|_, window, cx| open_settings(cx, window)),
                ),
        )
        .into_any_element()
}
