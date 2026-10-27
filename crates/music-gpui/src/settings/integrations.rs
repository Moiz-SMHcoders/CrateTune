//! Scrobbling & Discord section: Last.fm (desktop auth flow), ListenBrainz
//! (token validation), Discord Rich Presence, recent scrobble errors.

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
use super::widgets::{field, note, section_title, status_line};

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
        .child(discord)
        .child(footer)
        .into_any_element()
}
