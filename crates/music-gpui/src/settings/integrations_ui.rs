//! Settings sections for SponsorBlock, Music Together, Spotify import and the AI
//! content filter, the inputs they need ([`ExtrasUi`]) and the `SettingsPanel`
//! methods behind them.
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
use music_core::ai_filter::AiCategory;
use music_core::sponsorblock::{Category, DEFAULT_API_URL, normalize_api_url};
use music_core::spotify;

use super::SettingsPanel;
use super::file::Settings;
use super::prefs_ui::choice;
use super::widgets::{field, group, note, row, status_line};
use crate::app::update_app;
use crate::icons::Lucide;
use crate::integrations::config::{SkipMode, ai_category_label, sponsorblock_label};
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
    ai_endpoint: Entity<InputState>,
    ai_model: Entity<InputState>,
    ai_custom: Entity<InputState>,
    ai_key: Entity<InputState>,
    /// The key field shows the stored key once the keychain was read.
    ai_key_filled: bool,
    /// The SponsorBlock URL / a port / the relay URL typed is invalid.
    sb_url_invalid: bool,
    port_invalid: bool,
    relay_invalid: bool,
    pub(super) spotify: SpotifyFlow,
}

impl ExtrasUi {
    pub fn new(
        settings: &Settings,
        window: &mut Window,
        cx: &mut Context<SettingsPanel>,
    ) -> (Self, Vec<Subscription>) {
        // `placeholder`: technical text that stays as it is (addresses, numbers);
        // translated placeholders are set with `track_placeholder` below.
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
            together_name: input("", settings.together.display_name.clone()),
            together_port: input("42117", settings.together.port.to_string()),
            together_relay: input("", settings.together.relay_url.clone()),
            spotify_client: input("", settings.spotify.client_id.clone()),
            spotify_port: input("43819", settings.spotify.redirect_port.to_string()),
            ai_endpoint: input("", settings.ai_filter.endpoint.clone()),
            ai_model: input("gpt-4o-mini, llama3.1, …", settings.ai_filter.model.clone()),
            ai_custom: input("", settings.ai_filter.custom.clone()),
            ai_key: cx.new(|cx| InputState::new(window, cx).masked(true)),
            ai_key_filled: false,
            sb_url_invalid: false,
            port_invalid: false,
            relay_invalid: false,
            spotify: SpotifyFlow::Idle,
        };
        for (input, key) in [
            (
                &this.together_name,
                k!("settings.together.name_placeholder"),
            ),
            (
                &this.together_relay,
                k!("settings.together.relay_placeholder"),
            ),
            (&this.spotify_client, k!("settings.spotify.client_id")),
            (
                &this.ai_endpoint,
                k!("settings.ai_filter.endpoint_placeholder"),
            ),
            (&this.ai_custom, k!("settings.ai_filter.custom_placeholder")),
            (&this.ai_key, k!("settings.ai_filter.key_placeholder")),
        ] {
            crate::i18n::track_placeholder(input, key, window, cx);
        }
        let mut subscriptions: Vec<Subscription> = Vec::new();
        subscriptions.push(cx.subscribe(
            &this.ai_key,
            |panel: &mut SettingsPanel, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    let value = input.read(cx).value().trim().to_owned();
                    panel.set_ai_filter_key(value, cx);
                }
            },
        ));
        let rest = [
            &this.sb_url,
            &this.together_name,
            &this.together_port,
            &this.together_relay,
            &this.spotify_client,
            &this.spotify_port,
            &this.ai_endpoint,
            &this.ai_model,
            &this.ai_custom,
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
        });
        subscriptions.extend(rest);
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
        let ai = (
            read(&self.extras.ai_endpoint, cx),
            read(&self.extras.ai_model, cx),
            read(&self.extras.ai_custom, cx),
        );
        let mut changed = false;
        let a = &mut self.settings.ai_filter;
        if (&a.endpoint, &a.model, &a.custom) != (&ai.0, &ai.1, &ai.2) {
            (a.endpoint, a.model, a.custom) = ai;
            changed = true;
        }
        match normalize_api_url(&url) {
            Some(url) => {
                self.extras.sb_url_invalid = false;
                if url != self.settings.sponsorblock.api_url {
                    self.settings.sponsorblock.api_url = url;
                    changed = true;
                }
            }
            None => self.extras.sb_url_invalid = true,
        }
        if name != self.settings.together.display_name {
            self.settings.together.display_name = name;
            changed = true;
        }
        self.extras.relay_invalid =
            !relay.is_empty() && music_core::together::relay_url(&relay).is_none();
        if !self.extras.relay_invalid && relay != self.settings.together.relay_url {
            self.settings.together.relay_url = relay;
            changed = true;
        }
        let parse_port = |p: &str| p.parse::<u16>().ok().filter(|p| *p >= 1024);
        match (parse_port(&port), parse_port(&redirect)) {
            (Some(port), Some(redirect)) => {
                self.extras.port_invalid = false;
                if port != self.settings.together.port
                    || redirect != self.settings.spotify.redirect_port
                {
                    self.settings.together.port = port;
                    self.settings.spotify.redirect_port = redirect;
                    changed = true;
                }
            }
            _ => self.extras.port_invalid = true,
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
            "ai-endpoint" => &self.extras.ai_endpoint,
            "ai-model" => &self.extras.ai_model,
            "ai-custom" => &self.extras.ai_custom,
            "ai-key" => {
                self.set_ai_filter_key(value.trim().to_owned(), cx);
                return true;
            }
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

    // ---- Chromecast ------------------------------------------------------------------------

    pub fn set_cast(
        &mut self,
        f: impl FnOnce(&mut crate::integrations::config::CastPrefs),
        cx: &mut Context<Self>,
    ) {
        let before = self.settings.cast.clone();
        f(&mut self.settings.cast);
        if self.settings.cast != before {
            self.changed(false, cx);
        }
        cx.notify();
    }

    // ---- AI content filter ---------------------------------------------------------------

    pub fn set_ai_filter(
        &mut self,
        f: impl FnOnce(&mut crate::integrations::config::AiFilterPrefs),
        cx: &mut Context<Self>,
    ) {
        let before = self.settings.ai_filter.clone();
        f(&mut self.settings.ai_filter);
        if self.settings.ai_filter != before {
            self.changed(false, cx);
        }
        cx.notify();
    }

    fn set_ai_filter_key(&mut self, value: String, cx: &mut Context<Self>) {
        if self.secret(Secret::AiFilterKey).unwrap_or_default() == value {
            return;
        }
        let set = !value.is_empty();
        self.put_secret(Secret::AiFilterKey, set.then_some(value), cx);
        self.settings.ai_filter.key_set = set;
        self.changed(false, cx);
        cx.notify();
    }

    /// The API key for the filter: `Some("")` without one, `None` while the
    /// keychain hasn't been read yet.
    pub fn ai_filter_key(&self) -> Option<String> {
        if !self.settings.ai_filter.key_set {
            return Some(String::new());
        }
        if !self.secrets_loaded {
            return None;
        }
        Some(self.secret(Secret::AiFilterKey).unwrap_or_default())
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
            self.extras.spotify =
                SpotifyFlow::Failed(t!("settings.spotify.error.no_client").into());
            cx.notify();
            return;
        }
        let redirect = match spotify::LoopbackRedirect::bind(self.settings.spotify.redirect_port) {
            Ok(r) => r,
            Err(e) => {
                self.extras.spotify =
                    SpotifyFlow::Failed(t!("settings.spotify.error.redirect", error = e));
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
    let mut categories = group(t!("settings.sponsorblock.categories"), cx);
    for category in Category::ALL {
        let on = sb.categories.contains(&category);
        categories = categories.child(row(
            sponsorblock_label(category),
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
    let url_status: (SharedString, gpui::Hsla) = if panel.extras.sb_url_invalid {
        (
            t!("settings.sponsorblock.url_error").into(),
            cx.theme().danger,
        )
    } else {
        (
            t!(
                "settings.sponsorblock.using",
                url = panel.settings.sponsorblock.api_url
            )
            .into(),
            cx.theme().muted_foreground,
        )
    };
    v_flex()
        .gap_5()
        .child(
            group("SponsorBlock", cx) // i18n-ignore
                .child(row(
                    t!("settings.sponsorblock.skip_segments"),
                    Some(t!("settings.sponsorblock.skip_segments_desc")),
                    switch(
                        "sb-enabled",
                        sb.enabled,
                        |p, on, cx| p.set_sponsorblock(|s| s.enabled = on, cx),
                        cx,
                    ),
                    cx,
                ))
                .child(row(
                    t!("settings.sponsorblock.when"),
                    Some(t!("settings.sponsorblock.when_desc")),
                    choice(
                        "sb-mode",
                        &[SkipMode::Auto, SkipMode::Ask],
                        sb.mode,
                        |m| match m {
                            SkipMode::Auto => t!("settings.sponsorblock.mode_auto"),
                            SkipMode::Ask => t!("settings.sponsorblock.mode_ask"),
                        },
                        |p, m, cx| p.set_sponsorblock(|s| s.mode = m, cx),
                        cx,
                    ),
                    cx,
                ))
                .child(row(
                    t!("settings.sponsorblock.notify"),
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
            group(t!("settings.sponsorblock.server"), cx)
                .child(field(
                    t!("settings.sponsorblock.api_url"),
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
                                .label(t!("settings.sponsorblock.use_default"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.set_extras_field("sb-url", DEFAULT_API_URL, window, cx);
                                })),
                        )
                        .child(
                            Button::new("sb-cache")
                                .small()
                                .ghost()
                                .label(t!("settings.sponsorblock.forget"))
                                .on_click(|_, _, cx| {
                                    update_app(cx, |app, _| {
                                        app.integrations.sponsorblock.clear_cache()
                                    });
                                }),
                        ),
                )
                .child(note(t!("settings.sponsorblock.privacy"), cx)),
        )
        .into_any_element()
}

fn category_description(category: Category) -> &'static str {
    match category {
        Category::MusicOfftopic => t!("settings.sponsorblock.category_desc.music_offtopic"),
        Category::Sponsor => t!("settings.sponsorblock.category_desc.sponsor"),
        Category::Intro => t!("settings.sponsorblock.category_desc.intro"),
        Category::Outro => t!("settings.sponsorblock.category_desc.outro"),
        Category::SelfPromo => t!("settings.sponsorblock.category_desc.selfpromo"),
        Category::Interaction => t!("settings.sponsorblock.category_desc.interaction"),
        Category::Preview => t!("settings.sponsorblock.category_desc.preview"),
        Category::Filler => t!("settings.sponsorblock.category_desc.filler"),
        Category::Hook => t!("settings.sponsorblock.category_desc.hook"),
    }
}

pub(super) fn render_together(
    panel: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let t = panel.settings.together.clone();
    let port_error = panel
        .extras
        .port_invalid
        .then(|| t!("settings.together.port_error"));
    let relay_error = panel
        .extras
        .relay_invalid
        .then(|| t!("settings.together.relay_error"));
    v_flex()
        .gap_5()
        .child(
            group(t!("settings.together.you"), cx)
                .child(field(
                    t!("settings.together.display_name"),
                    Input::new(&panel.extras.together_name).small(),
                    cx,
                ))
                .child(note(t!("settings.together.display_name_note"), cx)),
        )
        .child(
            group(t!("settings.together.hosting"), cx)
                .child(field(
                    t!("settings.together.port"),
                    Input::new(&panel.extras.together_port).small(),
                    cx,
                ))
                .children(port_error.map(|e| status_line(e.into(), cx.theme().danger)))
                .child(row(
                    t!("together.room.allow_add"),
                    Some(t!("settings.together.allow_add_desc")),
                    switch(
                        "together-add",
                        t.allow_guests_to_add_tracks,
                        |p, on, cx| p.set_together(|t| t.allow_guests_to_add_tracks = on, cx),
                        cx,
                    ),
                    cx,
                ))
                .child(row(
                    t!("together.room.allow_control"),
                    Some(t!("settings.together.allow_control_desc")),
                    switch(
                        "together-control",
                        t.allow_guests_to_control_playback,
                        |p, on, cx| p.set_together(|t| t.allow_guests_to_control_playback = on, cx),
                        cx,
                    ),
                    cx,
                ))
                .child(row(
                    t!("settings.together.approval"),
                    None,
                    switch(
                        "together-approval",
                        t.require_host_approval,
                        |p, on, cx| p.set_together(|t| t.require_host_approval = on, cx),
                        cx,
                    ),
                    cx,
                ))
                .child(note(t!("settings.together.hosting_note"), cx)),
        )
        .child(
            group(t!("settings.together.across_networks"), cx)
                .child(field(
                    t!("settings.together.relay"),
                    Input::new(&panel.extras.together_relay).small(),
                    cx,
                ))
                .children(relay_error.map(|e| status_line(e.into(), cx.theme().danger)))
                .child(note(t!("settings.together.relay_note"), cx)),
        )
        .child(
            h_flex().child(
                Button::new("together-open")
                    .small()
                    .outline()
                    .icon(Lucide::UsersRound)
                    .label(t!("settings.together.open"))
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
            t!("settings.spotify.waiting").into(),
            theme.muted_foreground,
        ),
        (_, SpotifyFlow::Failed(e)) => (e.clone().into(), theme.danger),
        (Some(user), SpotifyFlow::Idle) => (
            t!("settings.integrations.connected_as", user = user).into(),
            theme.success,
        ),
        (None, SpotifyFlow::Idle) => (t!("common.not_connected").into(), theme.muted_foreground),
    };
    let waiting = matches!(panel.extras.spotify, SpotifyFlow::Waiting);
    let button = if s.user.is_some() {
        Button::new("spotify-disconnect")
            .small()
            .outline()
            .label(t!("common.disconnect"))
            .on_click(cx.listener(|this, _, _, cx| this.spotify_disconnect(cx)))
    } else {
        Button::new("spotify-connect")
            .small()
            .primary()
            .label(t!("common.connect"))
            .loading(waiting)
            .disabled(waiting || s.client_id.trim().is_empty())
            .on_click(cx.listener(|this, _, _, cx| this.spotify_connect(cx)))
    };
    let redirect = spotify::redirect_uri(s.redirect_port);
    let copy_redirect = redirect.clone();
    v_flex()
        .gap_5()
        .child(
            group(t!("settings.spotify.account"), cx)
                .child(
                    h_flex()
                        .justify_between()
                        .child(status_line(status, color))
                        .child(button),
                )
                .child(field(
                    t!("settings.spotify.client_id"),
                    Input::new(&panel.extras.spotify_client).small(),
                    cx,
                ))
                .child(field(
                    t!("settings.spotify.redirect_port"),
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
                                .child(t!("settings.spotify.redirect_uri", uri = redirect)),
                        )
                        .child(
                            Button::new("spotify-copy-redirect")
                                .xsmall()
                                .ghost()
                                .label(t!("common.copy"))
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                        copy_redirect.clone(),
                                    ));
                                }),
                        ),
                )
                .child(note(t!("settings.spotify.steps"), cx)),
        )
        .child(
            group(t!("settings.spotify.without_account"), cx)
                .child(note(
                    t!(
                        "settings.spotify.without_account_note",
                        other_app = "ArchiveTune"
                    ),
                    cx,
                ))
                .child(
                    h_flex().child(
                        Button::new("spotify-open-import")
                            .small()
                            .outline()
                            .icon(Lucide::Upload)
                            .label(t!("settings.spotify.open_import"))
                            .on_click(|_, window, cx| {
                                update_app(cx, |app, cx| app.navigate(Route::Import, window, cx));
                            }),
                    ),
                ),
        )
        .into_any_element()
}

pub(super) fn render_ai_filter(
    panel: &mut SettingsPanel,
    window: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    if !panel.extras.ai_key_filled && panel.secrets_loaded {
        panel.extras.ai_key_filled = true;
        if let Some(key) = panel.secret(Secret::AiFilterKey) {
            panel
                .extras
                .ai_key
                .update(cx, |input, cx| input.set_value(key, window, cx));
        }
    }
    let a = panel.settings.ai_filter.clone();
    let usable = a.core("x".into()).is_usable();
    let (classified, error) =
        crate::app::read_app(cx, |app, _| app.ai_filter_status()).unwrap_or_default();
    let theme = cx.theme();
    let (status, color): (SharedString, gpui::Hsla) = match (a.enabled, usable, &error) {
        (false, _, _) => (t!("settings.ai_filter.off").into(), theme.muted_foreground),
        (true, false, _) => (t!("settings.ai_filter.incomplete").into(), theme.warning),
        (true, true, Some(e)) => (
            t!("settings.ai_filter.failed", error = e).into(),
            theme.danger,
        ),
        (true, true, None) => (
            tp!("settings.ai_filter.on", classified).into(),
            theme.success,
        ),
    };
    let mut categories = group(t!("settings.ai_filter.hide"), cx);
    for category in AiCategory::ALL {
        let on = a.categories.contains(&category);
        categories = categories.child(row(
            ai_category_label(category),
            None,
            switch(
                SharedString::from(format!("ai-cat-{}", category.key())),
                on,
                move |p, on, cx| {
                    p.set_ai_filter(
                        |a| {
                            a.categories.retain(|c| *c != category);
                            if on {
                                a.categories.push(category);
                            }
                        },
                        cx,
                    )
                },
                cx,
            ),
            cx,
        ));
    }
    categories = categories
        .child(field(
            t!("settings.ai_filter.also_hide"),
            Input::new(&panel.extras.ai_custom).small(),
            cx,
        ))
        .child(note(t!("settings.ai_filter.custom_note"), cx));
    v_flex()
        .gap_5()
        .child(
            group(t!("settings.ai_filter.title"), cx)
                .child(row(
                    t!("settings.ai_filter.enable"),
                    Some(t!("settings.ai_filter.enable_desc")),
                    switch(
                        "ai-filter-enabled",
                        a.enabled,
                        |p, on, cx| p.set_ai_filter(|a| a.enabled = on, cx),
                        cx,
                    ),
                    cx,
                ))
                .child(status_line(status, color))
                .child(
                    h_flex().child(
                        Button::new("ai-filter-clear")
                            .small()
                            .ghost()
                            .label(t!("settings.ai_filter.forget"))
                            .disabled(classified == 0)
                            .on_click(|_, window, cx| {
                                update_app(cx, |app, cx| app.ai_filter_clear_cache(window, cx));
                            }),
                    ),
                ),
        )
        .child(
            group(t!("settings.ai_filter.model"), cx)
                .child(field(
                    t!("settings.ai_filter.endpoint"),
                    Input::new(&panel.extras.ai_endpoint).small(),
                    cx,
                ))
                .child(field(
                    t!("settings.ai_filter.model"),
                    Input::new(&panel.extras.ai_model).small(),
                    cx,
                ))
                .child(field(
                    t!("settings.ai_filter.api_key"),
                    Input::new(&panel.extras.ai_key).small().mask_toggle(),
                    cx,
                ))
                .child(note(t!("settings.ai_filter.api_note"), cx)),
        )
        .child(categories)
        .child(
            group(t!("settings.ai_filter.privacy"), cx)
                .child(note(t!("settings.ai_filter.privacy_note"), cx)),
        )
        .into_any_element()
}

pub(super) fn render_cast(
    panel: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let prefs = panel.settings.cast.clone();
    let (devices, scanning, connecting, error, casting) = crate::app::read_app(cx, |app, _| {
        let c = &app.integrations.cast;
        (
            c.devices.clone(),
            c.scanning,
            c.connecting.clone(),
            c.error.clone(),
            app.cast_device().map(|d| d.name.clone()),
        )
    })
    .unwrap_or_default();
    let theme = cx.theme();
    let (status, color): (SharedString, gpui::Hsla) = match (&casting, &connecting, &error) {
        (Some(name), _, _) => (t!("cast.casting_to", device = name).into(), theme.success),
        (None, Some(name), _) => (
            t!("cast.connecting_to", device = name).into(),
            theme.muted_foreground,
        ),
        (None, None, Some(e)) => (e.clone().into(), theme.danger),
        (None, None, None) if scanning => {
            (t!("settings.cast.looking").into(), theme.muted_foreground)
        }
        (None, None, None) => (
            tp!("settings.cast.found", devices.len()).into(),
            theme.muted_foreground,
        ),
    };
    let mut list = group(t!("settings.cast.devices"), cx).child(
        h_flex()
            .justify_between()
            .child(status_line(status, color))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("cast-scan")
                            .small()
                            .outline()
                            .label(t!("settings.cast.scan"))
                            .loading(scanning)
                            .disabled(scanning)
                            .on_click(|_, window, cx| {
                                update_app(cx, |app, cx| app.cast_scan(window, cx));
                            }),
                    )
                    .children(casting.is_some().then(|| {
                        Button::new("cast-stop")
                            .small()
                            .danger()
                            .label(t!("cast.stop"))
                            .on_click(|_, _, cx| {
                                update_app(cx, |app, cx| app.cast_stop(true, cx));
                            })
                    })),
            ),
    );
    for (ix, device) in devices.into_iter().enumerate() {
        let current = casting.as_deref() == Some(device.name.as_str());
        let detail = format!(
            "{}{}",
            device
                .model
                .as_deref()
                .map(|m| format!("{m} · "))
                .unwrap_or_default(),
            device.addr.ip()
        );
        let target = device.clone();
        list = list.child(
            h_flex()
                .gap_3()
                .justify_between()
                .child(
                    v_flex()
                        .min_w_0()
                        .child(div().text_sm().child(device.name.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(detail),
                        ),
                )
                .child(
                    Button::new(("cast-to", ix))
                        .small()
                        .when_primary(!current)
                        .label(if current {
                            t!("settings.cast.casting")
                        } else {
                            t!("settings.cast.cast")
                        })
                        .disabled(current || connecting.is_some())
                        .on_click(move |_, window, cx| {
                            let device = target.clone();
                            update_app(cx, |app, cx| app.cast_to(device, window, cx));
                        }),
                ),
        );
    }
    v_flex()
        .gap_5()
        .child(list)
        .child(
            group(t!("settings.cast.streaming"), cx)
                .child(row(
                    t!("settings.cast.direct"),
                    Some(t!("settings.cast.direct_desc")),
                    switch(
                        "cast-direct",
                        prefs.direct,
                        |p, on, cx| p.set_cast(|c| c.direct = on, cx),
                        cx,
                    ),
                    cx,
                ))
                .child(note(t!("settings.cast.note"), cx)),
        )
        .into_any_element()
}

trait WhenPrimary {
    fn when_primary(self, on: bool) -> Self;
}

impl WhenPrimary for Button {
    fn when_primary(self, on: bool) -> Self {
        if on { self.primary() } else { self.outline() }
    }
}
