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
use super::widgets::{field, note, row, section_title, status_line};

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
        (_, LastFmFlow::Waiting { error: None, .. }) => (
            "Approve the app in your browser, then press Finish.".into(),
            muted,
        ),
        (_, LastFmFlow::Waiting { error: Some(e), .. }) => (e.clone().into(), danger),
        (_, LastFmFlow::Failed(e)) => (e.clone().into(), danger),
        (Some(user), LastFmFlow::Idle) => (format!("Connected as {user}").into(), success),
        (None, LastFmFlow::Idle) => ("Not connected".into(), muted),
    };
    let lastfm_buttons = match (&s.lastfm.user, &panel.lastfm) {
        (_, LastFmFlow::Waiting { .. }) => h_flex()
            .gap_2()
            .child(
                Button::new("lastfm-finish")
                    .small()
                    .primary()
                    .label("Finish")
                    .on_click(cx.listener(|this, _, _, cx| this.lastfm_finish(cx))),
            )
            .child(
                Button::new("lastfm-cancel")
                    .small()
                    .ghost()
                    .label("Cancel")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.lastfm = LastFmFlow::Idle;
                        cx.notify();
                    })),
            ),
        (Some(_), _) => h_flex().gap_2().child(
            Button::new("lastfm-disconnect")
                .small()
                .outline()
                .label("Disconnect")
                .on_click(cx.listener(|this, _, _, cx| this.lastfm_disconnect(cx))),
        ),
        (None, flow) => h_flex()
            .gap_2()
            .child(
                Button::new("lastfm-connect")
                    .small()
                    .primary()
                    .label("Connect")
                    .loading(matches!(flow, LastFmFlow::Busy(_)))
                    .on_click(cx.listener(|this, _, _, cx| this.lastfm_connect(cx))),
            )
            .when(!s.lastfm.api_key.is_empty(), |this| {
                this.child(
                    Button::new("lastfm-clear")
                        .small()
                        .ghost()
                        .label("Forget key")
                        .on_click(cx.listener(|this, _, window, cx| this.lastfm_clear(window, cx))),
                )
            }),
    };
    let lastfm = v_flex()
        .gap_2()
        .child(
            h_flex()
                .justify_between()
                .child(section_title("Last.fm"))
                .child(lastfm_buttons),
        )
        .child(field("API key", Input::new(&panel.lastfm_key).small(), cx))
        .child(field(
            "Shared secret",
            Input::new(&panel.lastfm_secret).small().mask_toggle(),
            cx,
        ))
        .child(status_line(lastfm_status.0, lastfm_status.1));

    // ---- ListenBrainz
    let lb_status: (SharedString, gpui::Hsla) = match (&s.listenbrainz.user, &panel.lb) {
        (_, Validation::Busy) => ("Checking the token…".into(), muted),
        (_, Validation::Failed(e)) => (e.clone().into(), danger),
        (Some(user), Validation::Idle) => (format!("Connected as {user}").into(), success),
        (None, Validation::Idle) => ("Not connected".into(), muted),
    };
    let lb_button = if s.listenbrainz.user.is_some() {
        Button::new("lb-disconnect")
            .small()
            .outline()
            .label("Disconnect")
            .on_click(cx.listener(|this, _, window, cx| this.lb_disconnect(window, cx)))
    } else {
        Button::new("lb-validate")
            .small()
            .primary()
            .label("Validate")
            .loading(matches!(panel.lb, Validation::Busy))
            .on_click(cx.listener(|this, _, _, cx| this.lb_validate(cx)))
    };
    let listenbrainz = v_flex()
        .gap_2()
        .child(
            h_flex()
                .justify_between()
                .child(section_title("ListenBrainz"))
                .child(lb_button),
        )
        .child(field(
            "User token",
            Input::new(&panel.lb_token).small().mask_toggle(),
            cx,
        ))
        .child(status_line(lb_status.0, lb_status.1));

    // ---- Discord
    let enabled = s.discord.enabled;
    let discord_status: (SharedString, gpui::Hsla) = match (&panel.discord_status, enabled) {
        (_, false) => ("Off".into(), muted),
        (_, true) if s.discord.client_id.is_empty() => {
            ("Enter your Discord application ID.".into(), muted)
        }
        (DiscordStatus::Connected, true) => ("Connected to Discord".into(), success),
        (DiscordStatus::Disconnected { error }, true) => {
            (format!("Discord not reachable: {error}").into(), danger)
        }
        (DiscordStatus::Idle, true) => ("Waiting for playback…".into(), muted),
    };
    let discord = v_flex()
        .gap_2()
        .child(
            h_flex()
                .justify_between()
                .child(section_title("Discord Rich Presence"))
                .child(
                    Switch::new("discord-enabled")
                        .checked(enabled)
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.set_discord_enabled(*checked, cx)
                        })),
                ),
        )
        .child(field(
            "Application ID",
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
        .child(section_title("Activity"))
        .child(row("Type", None, kind, cx))
        .child(field("Name", input(ActivityField::Name), cx))
        .child(field("Details", input(ActivityField::Details), cx))
        .child(field("State", input(ActivityField::State), cx))
        .child(note(
            "Placeholders: {title}, {artist}, {album}. An empty name uses the Discord \
             application's name.",
            cx,
        ))
        .child(row("Large image", None, large, cx));
    if act.large_image == ImageSource::Custom {
        activity = activity.child(field("Large image", input(ActivityField::LargeImage), cx));
    }
    activity = activity.child(row("Small image", None, small, cx));
    if act.small_image == ImageSource::Custom {
        activity = activity.child(field("Small image", input(ActivityField::SmallImage), cx));
    }
    for (index, target) in [(0, button0), (1, button1)] {
        activity = activity.child(row(
            if index == 0 { "Button 1" } else { "Button 2" },
            None,
            target,
            cx,
        ));
        if act.buttons[index].target != ButtonTarget::Off {
            activity = activity.child(field("Label", input(ActivityField::ButtonLabel(index)), cx));
        }
        if act.buttons[index].target == ButtonTarget::Custom {
            activity = activity.child(field("URL", input(ActivityField::ButtonUrl(index)), cx));
        }
    }
    let activity = activity
        .child(row(
            "Show while paused",
            Some("Keeps the activity (without the running time) when playback pauses."),
            paused,
            cx,
        ))
        .child(note(
            "Custom images are asset keys of your Discord application or image URLs. \
             Buttons are visible to others, not to yourself.",
            cx,
        ));
    let discord = discord.child(activity);

    // ---- scrobble rules
    let r = s.scrobble_rules;
    let rules = v_flex()
        .gap_2()
        .child(section_title("When a song counts"))
        .child(slider_row(
            "Played",
            format!("{} %", r.percent),
            &panel.prefs.scrobble_percent,
            cx,
        ))
        .child(slider_row(
            "…or at most",
            format_secs(r.max_delay_secs),
            &panel.prefs.scrobble_delay,
            cx,
        ))
        .child(slider_row(
            "Ignore songs under",
            format_secs(r.min_track_secs),
            &panel.prefs.scrobble_min,
            cx,
        ))
        .child(note(
            "Last.fm's rule is 50 % or 4 minutes, songs over 30 s. Applies to Last.fm and \
             ListenBrainz.",
            cx,
        ));

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
                            .child(format!("Scrobble errors ({})", panel.scrobble_errors.len())),
                    )
                    .child(
                        Button::new("errors-clear")
                            .xsmall()
                            .ghost()
                            .label("Clear")
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
        format!("Secrets: {}", panel.secret_store)
    } else {
        "Secrets: loading…".into()
    };
    let path_line = match &panel.path {
        Some(p) => format!("Settings: {}", p.display()),
        None => "Settings are not saved (no config directory)".into(),
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
        format!("{} min", secs / 60)
    } else if secs >= 60 {
        format!("{}:{:02} min", secs / 60, secs % 60)
    } else {
        format!("{secs} s")
    }
}
