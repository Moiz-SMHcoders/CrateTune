//! Settings sections for SponsorBlock, Music Together and Spotify import, the
//! inputs they need ([`ExtrasUi`]) and the `SettingsPanel` methods behind them.
//! The app reads the saved values every poll / when it needs them, so these
//! sections emit no events.

use std::time::Duration;

use gpui::{
    AnyElement, AppContext as _, Context, Entity, IntoElement, ParentElement as _, SharedString,
    Styled as _, Subscription, Window, div,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    switch::Switch,
    v_flex,
};
use music_core::sponsorblock::{Category, DEFAULT_API_URL, normalize_api_url};
use music_core::spotify;

use super::SettingsPanel;
use super::file::Settings;
use super::prefs_ui::choice;
use super::widgets::{field, group, note, row, status_line};
use crate::app::update_app;
use crate::icons::Lucide;
use crate::integrations::config::SkipMode;
use crate::router::Route;
use crate::secrets::Secret;
use crate::util::off_thread;

/// Spotify sign-in progress.
pub enum SpotifyFlow {
    Idle,
    /// The browser is open on Spotify's consent page; the loopback listener waits.
    Waiting,
    Failed(String),
}

pub struct ExtrasUi {
    sb_url: Entity<InputState>,
    together_name: Entity<InputState>,
    together_port: Entity<InputState>,
    together_relay: Entity<InputState>,
    spotify_client: Entity<InputState>,
    spotify_port: Entity<InputState>,
    sb_url_error: Option<String>,
    port_error: Option<String>,
    relay_error: Option<String>,
    pub(super) spotify: SpotifyFlow,
}

impl ExtrasUi {
    pub fn new(
        settings: &Settings,
        window: &mut Window,
        cx: &mut Context<SettingsPanel>,
    ) -> (Self, Vec<Subscription>) {
        let mut input = |placeholder: &str, value: String| {
            let placeholder = placeholder.to_owned();
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        let this = Self {
            sb_url: input(DEFAULT_API_URL, settings.sponsorblock.api_url.clone()),
            together_name: input("Your name", settings.together.display_name.clone()),
            together_port: input("42117", settings.together.port.to_string()),
            together_relay: input(
                "wss://relay.example.com (empty: LAN only)",
                settings.together.relay_url.clone(),
            ),
            spotify_client: input("Client ID", settings.spotify.client_id.clone()),
            spotify_port: input("43819", settings.spotify.redirect_port.to_string()),
            sb_url_error: None,
            port_error: None,
            relay_error: None,
            spotify: SpotifyFlow::Idle,
        };
        let subscriptions = [
            &this.sb_url,
            &this.together_name,
            &this.together_port,
            &this.together_relay,
            &this.spotify_client,
            &this.spotify_port,
        ]
        .into_iter()
        .map(|input| {
            cx.subscribe(
                input,
                |panel: &mut SettingsPanel, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        panel.sync_extras(cx);
                    }
                },
            )
        })
        .collect();
        (this, subscriptions)
    }
}

impl SettingsPanel {
    fn sync_extras(&mut self, cx: &mut Context<Self>) {
        let read = |input: &Entity<InputState>, cx: &Context<Self>| {
            input.read(cx).value().trim().to_owned()
        };
        let url = read(&self.extras.sb_url, cx);
        let name = read(&self.extras.together_name, cx);
        let port = read(&self.extras.together_port, cx);
        let relay = read(&self.extras.together_relay, cx);
        let client = read(&self.extras.spotify_client, cx);
        let redirect = read(&self.extras.spotify_port, cx);
        let mut changed = false;
        match normalize_api_url(&url) {
            Some(url) => {
                self.extras.sb_url_error = None;
                if url != self.settings.sponsorblock.api_url {
                    self.settings.sponsorblock.api_url = url;
                    changed = true;
                }
            }
            None => {
                self.extras.sb_url_error =
                    Some("Needs an https:// address without credentials or query".into())
            }
        }
        if name != self.settings.together.display_name {
            self.settings.together.display_name = name;
            changed = true;
        }
        self.extras.relay_error = (!relay.is_empty()
            && music_core::together::relay_url(&relay).is_none())
        .then(|| "Needs a ws:// or wss:// address (no user name or query)".into());
        if self.extras.relay_error.is_none() && relay != self.settings.together.relay_url {
            self.settings.together.relay_url = relay;
            changed = true;
        }
        let parse_port = |p: &str| p.parse::<u16>().ok().filter(|p| *p >= 1024);
        match (parse_port(&port), parse_port(&redirect)) {
            (Some(port), Some(redirect)) => {
                self.extras.port_error = None;
                if port != self.settings.together.port
                    || redirect != self.settings.spotify.redirect_port
                {
                    self.settings.together.port = port;
                    self.settings.spotify.redirect_port = redirect;
                    changed = true;
                }
            }
            _ => self.extras.port_error = Some("Ports must be between 1024 and 65535".into()),
        }
        if client != self.settings.spotify.client_id {
            self.settings.spotify.client_id = client;
            changed = true;
        }
        if changed {
            self.changed(false, cx);
        }
        cx.notify();
    }

    /// Script hook: types into one of these fields.
    pub fn set_extras_field(
        &mut self,
        field: &str,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let input = match field {
            "sb-url" => &self.extras.sb_url,
            "together-name" => &self.extras.together_name,
            "together-port" => &self.extras.together_port,
            "together-relay" => &self.extras.together_relay,
            "spotify-client" => &self.extras.spotify_client,
            "spotify-port" => &self.extras.spotify_port,
            _ => return false,
        };
        let value = value.to_owned();
        input.update(cx, |input, cx| input.set_value(value, window, cx));
        // `set_value` emits no Change event.
        self.sync_extras(cx);
        true
    }

    // ---- SponsorBlock -----------------------------------------------------------------

    pub fn set_sponsorblock(
        &mut self,
        f: impl FnOnce(&mut crate::integrations::config::SponsorBlockPrefs),
        cx: &mut Context<Self>,
    ) {
        let before = self.settings.sponsorblock.clone();
        f(&mut self.settings.sponsorblock);
        if self.settings.sponsorblock != before {
            self.changed(false, cx);
        }
    }

    pub fn set_sponsorblock_category(
        &mut self,
        category: Category,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        self.set_sponsorblock(
            |sb| {
                sb.categories.retain(|c| *c != category);
                if on {
                    sb.categories.push(category);
                    sb.categories.sort();
                }
            },
            cx,
        );
    }

    // ---- Together -----------------------------------------------------------------------

    pub fn set_together(
        &mut self,
        f: impl FnOnce(&mut crate::integrations::config::TogetherPrefs),
        cx: &mut Context<Self>,
    ) {
        let before = self.settings.together.clone();
        f(&mut self.settings.together);
        if self.settings.together != before {
            self.changed(false, cx);
        }
    }

    // ---- Spotify ------------------------------------------------------------------------

    /// Client ID + refresh token, once signed in and the keychain has been read.
    pub fn spotify_credentials(&self) -> Option<(String, String)> {
        let s = &self.settings.spotify;
        s.user.as_ref()?;
        let client = s.client_id.trim();
        (!client.is_empty())
            .then(|| self.secret(Secret::SpotifyRefreshToken))
            .flatten()
            .map(|token| (client.to_owned(), token))
    }

    /// Spotify rotated the refresh token.
    pub fn set_spotify_refresh_token(&mut self, token: String, cx: &mut Context<Self>) {
        if self.secret(Secret::SpotifyRefreshToken).as_deref() != Some(token.as_str()) {
            self.put_secret(Secret::SpotifyRefreshToken, Some(token), cx);
        }
    }

    /// Opens Spotify's consent page and waits (up to 5 min) for the redirect on the
    /// loopback port, then stores the refresh token in the keychain.
    pub fn spotify_connect(&mut self, cx: &mut Context<Self>) {
        let client_id = self.settings.spotify.client_id.trim().to_owned();
        if client_id.is_empty() {
            self.extras.spotify = SpotifyFlow::Failed("Enter your app's Client ID first.".into());
            cx.notify();
            return;
        }
        let redirect = match spotify::LoopbackRedirect::bind(self.settings.spotify.redirect_port) {
            Ok(r) => r,
            Err(e) => {
                self.extras.spotify =
                    SpotifyFlow::Failed(format!("Can't listen for the redirect: {e}"));
                cx.notify();
                return;
            }
        };
        let pkce = spotify::Pkce::new();
        let state = spotify::new_state();
        let uri = redirect.redirect_uri();
        cx.open_url(&spotify::authorize_url(&client_id, &uri, &pkce, &state));
        self.extras.spotify = SpotifyFlow::Waiting;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = off_thread(move || {
                let code = redirect.wait(&state, Duration::from_secs(300))?;
                let token = spotify::exchange_code(&client_id, &code, &uri, &pkce.verifier)?;
                let user = spotify::SpotifyClient::new(token.access_token.clone()).me()?;
                Ok::<_, music_core::Error>((token, user))
            })
            .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok((token, user)) => {
                        this.extras.spotify = SpotifyFlow::Idle;
                        this.settings.spotify.user = Some(user.display_name);
                        if let Some(refresh) = token.refresh_token {
                            this.put_secret(Secret::SpotifyRefreshToken, Some(refresh), cx);
                        }
                        this.changed(false, cx);
                    }
                    Err(e) => this.extras.spotify = SpotifyFlow::Failed(e.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn spotify_disconnect(&mut self, cx: &mut Context<Self>) {
        self.settings.spotify.user = None;
        self.extras.spotify = SpotifyFlow::Idle;
        self.put_secret(Secret::SpotifyRefreshToken, None, cx);
        self.changed(false, cx);
    }
}

fn switch(
    id: impl Into<gpui::ElementId>,
    on: bool,
    set: impl Fn(&mut SettingsPanel, bool, &mut Context<SettingsPanel>) + 'static,
    cx: &Context<SettingsPanel>,
) -> Switch {
    Switch::new(id)
        .checked(on)
        .on_click(cx.listener(move |this, checked: &bool, _, cx| set(this, *checked, cx)))
}

pub(super) fn render_sponsorblock(
    panel: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let sb = panel.settings.sponsorblock.clone();
    let mut categories = group("Categories", cx);
    for category in Category::ALL {
        let on = sb.categories.contains(&category);
        categories = categories.child(row(
            category.label(),
            Some(category_description(category)),
            switch(
                SharedString::from(format!("sb-cat-{}", category.api_value())),
                on,
                move |p, on, cx| p.set_sponsorblock_category(category, on, cx),
                cx,
            ),
            cx,
        ));
    }
    let url_status: (SharedString, gpui::Hsla) = match &panel.extras.sb_url_error {
        Some(e) => (e.clone().into(), cx.theme().danger),
        None => (
            format!("Using {}", panel.settings.sponsorblock.api_url).into(),
            cx.theme().muted_foreground,
        ),
    };
    v_flex()
        .gap_5()
        .child(
            group("SponsorBlock", cx)
                .child(row(
                    "Skip segments",
                    Some("Look up community-marked segments of each song's video and skip them."),
                    switch(
                        "sb-enabled",
                        sb.enabled,
                        |p, on, cx| p.set_sponsorblock(|s| s.enabled = on, cx),
                        cx,
                    ),
                    cx,
                ))
                .child(row(
                    "When a segment starts",
                    Some("Skip right away, or show a Skip button in the player bar."),
                    choice(
                        "sb-mode",
                        &[SkipMode::Auto, SkipMode::Ask],
                        sb.mode,
                        |m| match m {
                            SkipMode::Auto => "Skip automatically",
                            SkipMode::Ask => "Ask",
                        },
                        |p, m, cx| p.set_sponsorblock(|s| s.mode = m, cx),
                        cx,
                    ),
                    cx,
                ))
                .child(row(
                    "Notify after skipping",
                    None,
                    switch(
                        "sb-notify",
                        sb.notify,
                        |p, on, cx| p.set_sponsorblock(|s| s.notify = on, cx),
                        cx,
                    ),
                    cx,
                )),
        )
        .child(categories)
        .child(
            group("Server", cx)
                .child(field(
                    "API URL",
                    Input::new(&panel.extras.sb_url).small(),
                    cx,
                ))
                .child(status_line(url_status.0, url_status.1))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("sb-url-reset")
                                .small()
                                .outline()
                                .label("Use the default server")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.set_extras_field("sb-url", DEFAULT_API_URL, window, cx);
                                })),
                        )
                        .child(
                            Button::new("sb-cache")
                                .small()
                                .ghost()
                                .label("Forget looked-up segments")
                                .on_click(|_, _, cx| {
                                    update_app(cx, |app, _| {
                                        app.integrations.sponsorblock.clear_cache()
                                    });
                                }),
                        ),
                )
                .child(note(
                    "Privacy: only the first 4 characters of the SHA-256 hash of a video id are \
                     sent, so the server can't tell which song plays. Segments come from \
                     sponsor.ajay.app (SponsorBlock, CC BY-NC-SA) unless you set another server.",
                    cx,
                )),
        )
        .into_any_element()
}

fn category_description(category: Category) -> &'static str {
    match category {
        Category::MusicOfftopic => "Talking, skits and other non-music parts of music videos.",
        Category::Sponsor => "Paid promotion.",
        Category::Intro => "Intermissions and intro animations without content.",
        Category::Outro => "Credits and end cards.",
        Category::SelfPromo => "Unpaid promotion: merch, donations, shout-outs.",
        Category::Interaction => "Reminders to like, subscribe or follow.",
        Category::Preview => "Recaps and previews of other content.",
        Category::Filler => "Tangents that aren't the main content.",
        Category::Hook => "Teasers and greetings at the start.",
    }
}

pub(super) fn render_together(
    panel: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let t = panel.settings.together.clone();
    let port_error = panel.extras.port_error.clone();
    let relay_error = panel.extras.relay_error.clone();
    v_flex()
        .gap_5()
        .child(
            group("You", cx)
                .child(field(
                    "Display name",
                    Input::new(&panel.extras.together_name).small(),
                    cx,
                ))
                .child(note("Shown to the others in a session.", cx)),
        )
        .child(
            group("Hosting", cx)
                .child(field(
                    "Port",
                    Input::new(&panel.extras.together_port).small(),
                    cx,
                ))
                .children(port_error.map(|e| status_line(e.into(), cx.theme().danger)))
                .child(row(
                    "Guests can add songs",
                    Some("Their suggestions go into your queue."),
                    switch(
                        "together-add",
                        t.allow_guests_to_add_tracks,
                        |p, on, cx| p.set_together(|t| t.allow_guests_to_add_tracks = on, cx),
                        cx,
                    ),
                    cx,
                ))
                .child(row(
                    "Guests can control playback",
                    Some("Play, pause, seek and skip for everyone."),
                    switch(
                        "together-control",
                        t.allow_guests_to_control_playback,
                        |p, on, cx| p.set_together(|t| t.allow_guests_to_control_playback = on, cx),
                        cx,
                    ),
                    cx,
                ))
                .child(row(
                    "Approve guests before they join",
                    None,
                    switch(
                        "together-approval",
                        t.require_host_approval,
                        |p, on, cx| p.set_together(|t| t.require_host_approval = on, cx),
                        cx,
                    ),
                    cx,
                ))
                .child(note(
                    "Sessions run on your local network: guests connect straight to this \
                     computer (allow incoming connections if the firewall asks). The link \
                     contains a random key; only people with the link can join. Changes apply \
                     to a running session right away. On the Listen together page, “Make host” \
                     hands the session to a guest: everyone follows the new host.",
                    cx,
                )),
        )
        .child(
            group("Across networks", cx)
                .child(field(
                    "Relay server",
                    Input::new(&panel.extras.together_relay).small(),
                    cx,
                ))
                .children(relay_error.map(|e| status_line(e.into(), cx.theme().danger)))
                .child(note(
                    "“Host via relay” runs the session through a music-relay server you host \
                     yourself (see the README: one small binary, put it behind a TLS reverse \
                     proxy for wss://). Guests then join from anywhere with the link. The relay \
                     forwards the session's messages (song ids, titles, names) but never \
                     decides anything; empty = LAN only.",
                    cx,
                )),
        )
        .child(
            h_flex().child(
                Button::new("together-open")
                    .small()
                    .outline()
                    .icon(Lucide::UsersRound)
                    .label("Open Listen together")
                    .on_click(|_, window, cx| {
                        update_app(cx, |app, cx| app.navigate(Route::Together, window, cx));
                    }),
            ),
        )
        .into_any_element()
}

pub(super) fn render_spotify(
    panel: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let s = panel.settings.spotify.clone();
    let theme = cx.theme();
    let (status, color): (SharedString, gpui::Hsla) = match (&s.user, &panel.extras.spotify) {
        (_, SpotifyFlow::Waiting) => (
            "Waiting for you to approve the app in the browser…".into(),
            theme.muted_foreground,
        ),
        (_, SpotifyFlow::Failed(e)) => (e.clone().into(), theme.danger),
        (Some(user), SpotifyFlow::Idle) => (format!("Connected as {user}").into(), theme.success),
        (None, SpotifyFlow::Idle) => ("Not connected".into(), theme.muted_foreground),
    };
    let waiting = matches!(panel.extras.spotify, SpotifyFlow::Waiting);
    let button = if s.user.is_some() {
        Button::new("spotify-disconnect")
            .small()
            .outline()
            .label("Disconnect")
            .on_click(cx.listener(|this, _, _, cx| this.spotify_disconnect(cx)))
    } else {
        Button::new("spotify-connect")
            .small()
            .primary()
            .label("Connect")
            .loading(waiting)
            .disabled(waiting || s.client_id.trim().is_empty())
            .on_click(cx.listener(|this, _, _, cx| this.spotify_connect(cx)))
    };
    let redirect = spotify::redirect_uri(s.redirect_port);
    let copy_redirect = redirect.clone();
    v_flex()
        .gap_5()
        .child(
            group("Spotify account", cx)
                .child(
                    h_flex()
                        .justify_between()
                        .child(status_line(status, color))
                        .child(button),
                )
                .child(field(
                    "Client ID",
                    Input::new(&panel.extras.spotify_client).small(),
                    cx,
                ))
                .child(field(
                    "Redirect port",
                    Input::new(&panel.extras.spotify_port).small(),
                    cx,
                ))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("Redirect URI: {redirect}")),
                        )
                        .child(
                            Button::new("spotify-copy-redirect")
                                .xsmall()
                                .ghost()
                                .label("Copy")
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                        copy_redirect.clone(),
                                    ));
                                }),
                        ),
                )
                .child(note(
                    "1. Create an app at developer.spotify.com/dashboard (Web API). \
                     2. Add the redirect URI above to it. 3. Paste its Client ID here and press \
                     Connect. Access is read-only (your playlists and liked songs); the refresh \
                     token is kept in the keychain.",
                    cx,
                )),
        )
        .child(
            group("Without an account", cx)
                .child(note(
                    "Export a playlist with Exportify (exportify.app) and import the CSV file \
                     in Library → Import & export. Signing in with the web player's sp_dc cookie \
                     (as ArchiveTune does) is not supported: it depends on a secret extracted \
                     from Spotify's web player.",
                    cx,
                ))
                .child(
                    h_flex().child(
                        Button::new("spotify-open-import")
                            .small()
                            .outline()
                            .icon(Lucide::Upload)
                            .label("Open Import & export")
                            .on_click(|_, window, cx| {
                                update_app(cx, |app, cx| app.navigate(Route::Import, window, cx));
                            }),
                    ),
                ),
        )
        .into_any_element()
}
