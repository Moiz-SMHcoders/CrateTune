//! Scrobbling & Discord section: Last.fm (desktop auth flow), ListenBrainz
//! (token validation), scrobble rules, Discord Rich Presence with activity
//! customization, recent scrobble errors.

use gpui::{
    AnyElement, Context, FontWeight, IntoElement, ParentElement as _, SharedString, Styled as _,
    Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::Input,
    switch::Switch,
    v_flex,
};
use music_core::discord::DiscordStatus;

use super::SettingsPanel;
use super::panel::{LastFmFlow, Validation};
use super::prefs::{ActivityType, ButtonTarget, ImageSource};
use super::prefs_ui::{ActivityField, choice, slider_row};
use super::widgets::{Anchor as _, field, note, row, row_at, section_title, status_line};

pub(super) fn render(
    panel: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let theme = cx.theme();
    let danger = theme.danger;
    let success = theme.success;
    let muted = theme.muted_foreground;
    let s = &panel.settings;

    // ---- Last.fm
    let lastfm_status: (SharedString, gpui::Hsla) = match (&s.lastfm.user, &panel.lastfm) {
        (_, LastFmFlow::Busy(what)) => ((*what).into(), muted),
        (_, LastFmFlow::Waiting { error: None, .. }) => {
            (t!("settings.integrations.lastfm_approve").into(), muted)
        }
        (_, LastFmFlow::Waiting { error: Some(e), .. }) => (e.clone().into(), danger),
        (_, LastFmFlow::Failed(e)) => (e.clone().into(), danger),
        (Some(user), LastFmFlow::Idle) => (
            t!("settings.integrations.connected_as", user = user).into(),
            success,
        ),
        (None, LastFmFlow::Idle) => (t!("common.not_connected").into(), muted),
    };
    let lastfm_buttons = match (&s.lastfm.user, &panel.lastfm) {
        (_, LastFmFlow::Waiting { .. }) => h_flex()
            .gap_2()
            .child(
                Button::new("lastfm-finish")
                    .small()
                    .primary()
                    .label(t!("settings.integrations.finish"))
                    .on_click(cx.listener(|this, _, _, cx| this.lastfm_finish(cx))),
            )
            .child(
                Button::new("lastfm-cancel")
                    .small()
                    .ghost()
                    .label(t!("common.cancel"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.lastfm = LastFmFlow::Idle;
                        cx.notify();
                    })),
            ),
        (Some(_), _) => h_flex().gap_2().child(
            Button::new("lastfm-disconnect")
                .small()
                .outline()
                .label(t!("common.disconnect"))
                .on_click(cx.listener(|this, _, _, cx| this.lastfm_disconnect(cx))),
        ),
        (None, flow) => h_flex()
            .gap_2()
            .child(
                Button::new("lastfm-connect")
                    .small()
                    .primary()
                    .label(t!("common.connect"))
                    .loading(matches!(flow, LastFmFlow::Busy(_)))
                    .on_click(cx.listener(|this, _, _, cx| this.lastfm_connect(cx))),
            )
            .when(!s.lastfm.api_key.is_empty(), |this| {
                this.child(
                    Button::new("lastfm-clear")
                        .small()
                        .ghost()
                        .label(t!("settings.integrations.forget_key"))
                        .on_click(cx.listener(|this, _, window, cx| this.lastfm_clear(window, cx))),
                )
            }),
    };
    let lastfm = v_flex()
        .gap_2()
        .anchor("integrations.lastfm")
        .child(
            h_flex()
                .justify_between()
                .child(section_title("Last.fm")) // i18n-ignore
                .child(lastfm_buttons),
        )
        .child(field(
            t!("settings.integrations.api_key"),
            Input::new(&panel.lastfm_key).small(),
            cx,
        ))
        .child(field(
            t!("settings.integrations.shared_secret"),
            Input::new(&panel.lastfm_secret).small().mask_toggle(),
            cx,
        ))
        .child(status_line(lastfm_status.0, lastfm_status.1));

    // ---- ListenBrainz
    let lb_status: (SharedString, gpui::Hsla) = match (&s.listenbrainz.user, &panel.lb) {
        (_, Validation::Busy) => (t!("settings.integrations.checking_token").into(), muted),
        (_, Validation::Failed(e)) => (e.clone().into(), danger),
        (Some(user), Validation::Idle) => (
            t!("settings.integrations.connected_as", user = user).into(),
            success,
        ),
        (None, Validation::Idle) => (t!("common.not_connected").into(), muted),
    };
    let lb_button = if s.listenbrainz.user.is_some() {
        Button::new("lb-disconnect")
            .small()
            .outline()
            .label(t!("common.disconnect"))
            .on_click(cx.listener(|this, _, window, cx| this.lb_disconnect(window, cx)))
    } else {
        Button::new("lb-validate")
            .small()
            .primary()
            .label(t!("settings.integrations.validate"))
            .loading(matches!(panel.lb, Validation::Busy))
            .on_click(cx.listener(|this, _, _, cx| this.lb_validate(cx)))
    };
    let listenbrainz = v_flex()
        .gap_2()
        .anchor("integrations.listenbrainz")
        .child(
            h_flex()
                .justify_between()
                .child(section_title("ListenBrainz")) // i18n-ignore
                .child(lb_button),
        )
        .child(field(
            t!("settings.integrations.user_token"),
            Input::new(&panel.lb_token).small().mask_toggle(),
            cx,
        ))
        .child(status_line(lb_status.0, lb_status.1));

    // ---- Discord
    let enabled = s.discord.enabled;
    let discord_status: (SharedString, gpui::Hsla) = match (&panel.discord_status, enabled) {
        (_, false) => (t!("common.off").into(), muted),
        (_, true) if s.discord.client_id.is_empty() => {
            (t!("settings.integrations.discord_enter_id").into(), muted)
        }
        (DiscordStatus::Connected, true) => (
            t!("settings.integrations.discord_connected").into(),
            success,
        ),
        (DiscordStatus::Disconnected { error }, true) => (
            t!("settings.integrations.discord_unreachable", error = error).into(),
            danger,
        ),
        (DiscordStatus::Idle, true) => (t!("settings.integrations.discord_waiting").into(), muted),
    };
    let discord = v_flex()
        .gap_2()
        .anchor("integrations.discord")
        .child(
            h_flex()
                .justify_between()
                .child(section_title(t!("settings.integrations.discord_title")))
                .child(
                    Switch::new("discord-enabled")
                        .checked(enabled)
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.set_discord_enabled(*checked, cx)
                        })),
                ),
        )
        .child(field(
            t!("settings.integrations.application_id"),
            Input::new(&panel.discord_id).small(),
            cx,
        ))
        .child(status_line(discord_status.0, discord_status.1));

    // ---- Discord activity
    let act = s.discord.activity.clone();
    let input = |field: ActivityField| {
        panel
            .prefs
            .activity
            .iter()
            .find(|(f, _)| *f == field)
            .map(|(_, input)| Input::new(input).small())
            .expect("every activity field has an input")
    };
    let kind = choice(
        "discord-kind",
        &ActivityType::ALL,
        act.kind,
        ActivityType::title,
        |this, k, cx| this.set_activity_kind(k, cx),
        cx,
    );
    let large = choice(
        "discord-large",
        &ImageSource::ALL,
        act.large_image,
        ImageSource::title,
        |this, src, cx| this.set_activity_image(false, src, cx),
        cx,
    );
    let small = choice(
        "discord-small",
        &ImageSource::ALL,
        act.small_image,
        ImageSource::title,
        |this, src, cx| this.set_activity_image(true, src, cx),
        cx,
    );
    let button_target = |index: usize, cx: &Context<SettingsPanel>| {
        let pick: fn(&mut SettingsPanel, ButtonTarget, &mut Context<SettingsPanel>) = if index == 0
        {
            |this, t, cx| this.set_activity_button(0, t, cx)
        } else {
            |this, t, cx| this.set_activity_button(1, t, cx)
        };
        choice(
            if index == 0 {
                "discord-button-0"
            } else {
                "discord-button-1"
            },
            &ButtonTarget::ALL,
            act.buttons[index].target,
            ButtonTarget::title,
            pick,
            cx,
        )
    };
    let button0 = button_target(0, cx);
    let button1 = button_target(1, cx);
    let paused = Switch::new("discord-paused")
        .checked(act.show_when_paused)
        .on_click(
            cx.listener(|this, checked: &bool, _, cx| this.set_show_when_paused(*checked, cx)),
        );
    let mut activity = v_flex()
        .gap_2()
        .pl_3()
        .border_l_2()
        .border_color(theme.border)
        .anchor("integrations.discord.activity")
        .child(section_title(t!("discord.activity.title")))
        .child(row(t!("discord.activity.type"), None, kind, cx))
        .child(field(
            t!("discord.activity.name"),
            input(ActivityField::Name),
            cx,
        ))
        .child(field(
            t!("discord.activity.details"),
            input(ActivityField::Details),
            cx,
        ))
        .child(field(
            t!("discord.activity.state"),
            input(ActivityField::State),
            cx,
        ))
        .child(note(t!("discord.activity.placeholders_note"), cx))
        .child(row(t!("discord.activity.large_image"), None, large, cx));
    if act.large_image == ImageSource::Custom {
        activity = activity.child(field(
            t!("discord.activity.large_image"),
            input(ActivityField::LargeImage),
            cx,
        ));
    }
    activity = activity.child(row(t!("discord.activity.small_image"), None, small, cx));
    if act.small_image == ImageSource::Custom {
        activity = activity.child(field(
            t!("discord.activity.small_image"),
            input(ActivityField::SmallImage),
            cx,
        ));
    }
    for (index, target) in [(0, button0), (1, button1)] {
        activity = activity.child(row(
            t!("discord.activity.button", number = index + 1),
            None,
            target,
            cx,
        ));
        if act.buttons[index].target != ButtonTarget::Off {
            activity = activity.child(field(
                t!("discord.activity.label"),
                input(ActivityField::ButtonLabel(index)),
                cx,
            ));
        }
        if act.buttons[index].target == ButtonTarget::Custom {
            activity = activity.child(field(
                t!("discord.activity.url"),
                input(ActivityField::ButtonUrl(index)),
                cx,
            ));
        }
    }
    let activity = activity
        .child(row_at(
            "integrations.discord.activity.show_paused",
            t!("discord.activity.show_paused"),
            Some(t!("discord.activity.show_paused_desc")),
            paused,
            cx,
        ))
        .child(note(t!("discord.activity.custom_note"), cx));
    let discord = discord.child(activity);

    // ---- scrobble rules
    let r = s.scrobble_rules;
    let rules = v_flex()
        .gap_2()
        .anchor("integrations.rules")
        .child(section_title(t!("scrobble.rules.title")))
        .child(slider_row(
            t!("scrobble.rules.played"),
            t!("scrobble.rules.percent", percent = r.percent),
            &panel.prefs.scrobble_percent,
            cx,
        ))
        .child(slider_row(
            t!("scrobble.rules.or_at_most"),
            format_secs(r.max_delay_secs),
            &panel.prefs.scrobble_delay,
            cx,
        ))
        .child(slider_row(
            t!("scrobble.rules.ignore_under"),
            format_secs(r.min_track_secs),
            &panel.prefs.scrobble_min,
            cx,
        ))
        .child(note(t!("scrobble.rules.note"), cx));

    // ---- footer: errors + where things are stored
    let errors = (!panel.scrobble_errors.is_empty()).then(|| {
        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(danger)
                            .child(tp!("scrobble.errors", panel.scrobble_errors.len())),
                    )
                    .child(
                        Button::new("errors-clear")
                            .xsmall()
                            .ghost()
                            .label(t!("common.clear"))
                            .on_click(cx.listener(|this, _, _, cx| this.clear_errors(cx))),
                    ),
            )
            .children(panel.scrobble_errors.iter().rev().map(|e| {
                div()
                    .text_xs()
                    .text_color(muted)
                    .line_clamp(2)
                    .child(e.clone())
            }))
    });
    let secrets_line = if panel.secrets_loaded {
        t!("settings.integrations.secrets", store = panel.secret_store)
    } else {
        t!("settings.integrations.secrets_loading").into()
    };
    let path_line = match &panel.path {
        Some(p) => t!("settings.integrations.settings_path", path = p.display()),
        None => t!("settings.integrations.not_saved").into(),
    };
    let footer = v_flex()
        .gap_1()
        .pt_2()
        .border_t_1()
        .border_color(theme.border)
        .children(errors)
        .child(note(secrets_line, cx))
        .child(note(path_line, cx))
        .when_some(panel.file_error.clone(), |this, e| {
            this.child(status_line(e.into(), danger))
        });

    v_flex()
        .gap_5()
        .child(lastfm)
        .child(listenbrainz)
        .child(rules)
        .child(discord)
        .child(footer)
        .into_any_element()
}

fn format_secs(secs: u32) -> String {
    if secs >= 60 && secs.is_multiple_of(60) {
        t!("time.minutes", minutes = secs / 60)
    } else if secs >= 60 {
        t!(
            "time.minutes",
            minutes = format!("{}:{:02}", secs / 60, secs % 60)
        )
    } else {
        t!("time.seconds", seconds = secs)
    }
}
