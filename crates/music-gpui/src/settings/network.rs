//! Network section: an HTTP(S) / SOCKS proxy and DNS-over-HTTPS (Cloudflare,
//! Google, Quad9 or a custom URL, optional fallback to the system resolver) for
//! every request (InnerTube, streams, artwork, lyrics, scrobbling) through
//! `music_core::http::configure`.
//!
//! The fields are a draft until "Apply": it validates them, rebuilds the shared
//! HTTP agent (an invalid proxy keeps the previous one), saves the settings and
//! stores the password as a secret (keychain, else the 0600 settings file).
//! The proxy is applied at launch before any page loads; with a password it is
//! applied again once the secret has been read. Applying emits
//! [`SettingsEvent::Network`] so the app reloads its pages.

use std::time::Instant;

use gpui::{
    AnyElement, AppContext as _, Context, Entity, IntoElement, ParentElement as _, SharedString,
    Styled as _, Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{
    ActiveTheme as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    switch::Switch,
    v_flex,
};
use music_core::HttpConfig;

use super::SettingsPanel;
use super::file::{DnsConfig, DnsMode, NetworkConfig, ProxyKind, Settings};
use super::panel::SettingsEvent;
use super::widgets::{field, group, note, status_line};
use crate::secrets::Secret;
use crate::util::off_thread;

/// Fetched by "Test connection" (an empty 204 response).
const TEST_URL: &str = "https://www.youtube.com/generate_204";

pub(super) enum NetStatus {
    Idle,
    Applied(String),
    Failed(String),
    Testing,
    TestOk(u128),
    TestFailed(String),
    DnsOk(String),
}

/// The section's draft fields, owned by the panel.
pub struct NetworkState {
    kind: ProxyKind,
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    dns_mode: DnsMode,
    dns_url: Entity<InputState>,
    dns_fallback: bool,
    pub(super) status: NetStatus,
}

impl NetworkState {
    pub fn new(settings: &Settings, window: &mut Window, cx: &mut Context<SettingsPanel>) -> Self {
        let n = &settings.network;
        let mut input = |placeholder: &str, value: String, masked: bool| {
            let placeholder = placeholder.to_owned();
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .masked(masked)
                    .default_value(value)
            })
        };
        Self {
            kind: n.proxy,
            host: input("proxy.example.com or 127.0.0.1", n.host.clone(), false),
            port: input(
                "1080",
                if n.port == 0 {
                    String::new()
                } else {
                    n.port.to_string()
                },
                false,
            ),
            user: input("optional", n.username.clone(), false),
            password: input("optional", String::new(), true),
            dns_mode: n.dns.mode,
            dns_url: input(
                "https://dns.example/dns-query",
                n.dns.custom_url.clone(),
                false,
            ),
            dns_fallback: n.dns.fallback,
            status: NetStatus::Idle,
        }
    }

    /// Fills the password field once the secret has been read.
    pub(super) fn show_password(&self, value: String, window: &mut Window, cx: &mut gpui::App) {
        self.password
            .update(cx, |input, cx| input.set_value(value, window, cx));
    }
}

impl SettingsPanel {
    /// Rebuilds the shared HTTP agent from the saved settings (+ password secret).
    /// The error never contains the password.
    pub(super) fn apply_proxy(&mut self) -> Result<String, String> {
        let network = &self.settings.network;
        let password = self.secrets.get(&Secret::ProxyPassword).cloned();
        let url = network.proxy_url(password.as_deref())?;
        let config = HttpConfig {
            proxy: url,
            doh: network.dns.doh(),
            ..music_core::http::http_config()
        };
        music_core::http::configure(config).map_err(|e| {
            if e.to_string().contains("DNS-over-HTTPS") {
                "The DNS-over-HTTPS URL isn't valid.".to_owned()
            } else {
                "The proxy address isn't valid for this proxy type.".to_owned()
            }
        })?;
        Ok(status_text(network))
    }

    /// "Apply": validate the draft, save, apply, store the password.
    pub fn network_apply(&mut self, cx: &mut Context<Self>) {
        let state = &self.network;
        let read = |input: &Entity<InputState>, cx: &Context<Self>| {
            input.read(cx).value().trim().to_owned()
        };
        let port_text = read(&state.port, cx);
        let port = match port_text.parse::<u16>() {
            Ok(p) => p,
            Err(_) if port_text.is_empty() => 0,
            Err(_) => {
                self.network.status =
                    NetStatus::Failed("The port must be a number (1–65535).".into());
                cx.notify();
                return;
            }
        };
        // SOCKS4 has no authentication; its credential fields are hidden.
        let socks4 = state.kind == ProxyKind::Socks4;
        let password = if socks4 {
            String::new()
        } else {
            state.password.read(cx).value().to_string()
        };
        let draft = NetworkConfig {
            proxy: state.kind,
            host: read(&state.host, cx),
            port,
            username: if socks4 {
                String::new()
            } else {
                read(&state.user, cx)
            },
            has_password: !password.is_empty(),
            dns: DnsConfig {
                mode: state.dns_mode,
                custom_url: read(&state.dns_url, cx),
                fallback: state.dns_fallback,
            },
        };
        if let Err(e) = draft.validate() {
            self.network.status = NetStatus::Failed(e);
            cx.notify();
            return;
        }
        let previous = self.settings.network.clone();
        let previous_password = self.secrets.get(&Secret::ProxyPassword).cloned();
        self.settings.network = draft;
        let new_password = (!password.is_empty()).then_some(password);
        match &new_password {
            Some(p) => self.secrets.insert(Secret::ProxyPassword, p.clone()),
            None => self.secrets.remove(&Secret::ProxyPassword),
        };
        match self.apply_proxy() {
            Ok(message) => {
                if new_password != previous_password {
                    self.put_secret(Secret::ProxyPassword, new_password, cx);
                }
                self.network.status = NetStatus::Applied(message);
                cx.emit(SettingsEvent::Network);
                self.changed(false, cx);
            }
            Err(e) => {
                // Keep what was in effect.
                self.settings.network = previous;
                match previous_password {
                    Some(p) => self.secrets.insert(Secret::ProxyPassword, p),
                    None => self.secrets.remove(&Secret::ProxyPassword),
                };
                self.network.status = NetStatus::Failed(e);
            }
        }
        cx.notify();
    }

    /// Fetches [`TEST_URL`] through the current agent and reports the time.
    pub fn network_test(&mut self, cx: &mut Context<Self>) {
        self.network.status = NetStatus::Testing;
        cx.spawn(async move |this, cx| {
            let started = Instant::now();
            let result = off_thread(|| music_core::fetch_bytes(TEST_URL)).await;
            let ms = started.elapsed().as_millis();
            this.update(cx, |this, cx| {
                this.network.status = match result {
                    Ok(_) => NetStatus::TestOk(ms),
                    Err(e) => NetStatus::TestFailed(e.to_string()),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Resolves music.youtube.com the way requests do now and shows how.
    pub fn network_test_dns(&mut self, cx: &mut Context<Self>) {
        self.network.status = NetStatus::Testing;
        cx.spawn(async move |this, cx| {
            let started = Instant::now();
            let result = off_thread(|| music_core::http::lookup("music.youtube.com")).await;
            let ms = started.elapsed().as_millis();
            this.update(cx, |this, cx| {
                this.network.status = match result {
                    Ok(lookup) => {
                        use music_core::http::LookupSource;
                        let via = match lookup.source {
                            LookupSource::Doh => "DNS-over-HTTPS",
                            LookupSource::Cached => "DNS-over-HTTPS (cached)",
                            LookupSource::System => "the system resolver",
                        };
                        let first: Vec<String> = lookup
                            .addrs
                            .iter()
                            .take(2)
                            .map(ToString::to_string)
                            .collect();
                        NetStatus::DnsOk(format!(
                            "music.youtube.com → {} via {via} in {ms} ms",
                            first.join(", ")
                        ))
                    }
                    Err(e) => NetStatus::TestFailed(e),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Script hook: `system|cloudflare|google|quad9|https://…[;nofallback]`, applied.
    pub fn set_dns_draft(&mut self, spec: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (mode, rest) = spec.split_once(';').unwrap_or((spec, ""));
        self.network.dns_fallback = rest != "nofallback";
        self.network.dns_mode = DnsMode::ALL
            .into_iter()
            .find(|m| m.title().eq_ignore_ascii_case(mode))
            .unwrap_or(DnsMode::Custom);
        if self.network.dns_mode == DnsMode::Custom {
            let url = mode.to_owned();
            self.network
                .dns_url
                .update(cx, |input, cx| input.set_value(url, window, cx));
        }
        self.network_apply(cx);
    }

    /// Script hook: `KIND:HOST:PORT` (or `off`), applied like the Apply button.
    pub fn set_proxy_draft(&mut self, spec: &str, window: &mut Window, cx: &mut Context<Self>) {
        let mut parts = spec.splitn(3, ':');
        let kind = parts.next().unwrap_or("off");
        self.network.kind = ProxyKind::ALL
            .into_iter()
            .find(|k| k.title().eq_ignore_ascii_case(kind))
            .unwrap_or(ProxyKind::Off);
        for (input, value) in [
            (self.network.host.clone(), parts.next().unwrap_or("")),
            (self.network.port.clone(), parts.next().unwrap_or("")),
        ] {
            let value = value.to_owned();
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        self.network_apply(cx);
    }
}

/// "Using …" line for the applied settings.
fn status_text(network: &NetworkConfig) -> String {
    let proxy = match network.display_url() {
        Some(url) => format!("Using {url}"),
        None => "Direct connections (no proxy)".into(),
    };
    match network.dns.mode {
        DnsMode::System => proxy,
        DnsMode::Custom => format!(
            "{proxy} · DNS-over-HTTPS ({})",
            network.dns.custom_url.trim()
        ),
        mode => format!("{proxy} · DNS-over-HTTPS ({})", mode.title()),
    }
}

pub(super) fn render(
    panel: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let theme = cx.theme();
    let (muted, danger, success) = (theme.muted_foreground, theme.danger, theme.success);
    let state = &panel.network;
    let kind = state.kind;
    let kinds = h_flex()
        .gap_1()
        .flex_wrap()
        .children(ProxyKind::ALL.map(|k| {
            let selected = k == kind;
            Button::new(SharedString::from(format!("proxy-kind-{}", k.title())))
                .small()
                .map(|b| if selected { b.primary() } else { b.outline() })
                .selected(selected)
                .label(k.title())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.network.kind = k;
                    cx.notify();
                }))
        }));
    let (status, color): (SharedString, gpui::Hsla) = match &state.status {
        NetStatus::Idle => (status_text(&panel.settings.network).into(), muted),
        NetStatus::DnsOk(m) => (m.clone().into(), success),
        NetStatus::Applied(m) => (m.clone().into(), success),
        NetStatus::Failed(e) => (e.clone().into(), danger),
        NetStatus::Testing => ("Testing…".into(), muted),
        NetStatus::TestOk(ms) => (format!("Connected to YouTube in {ms} ms").into(), success),
        NetStatus::TestFailed(e) => (format!("Connection failed: {e}").into(), danger),
    };
    let off = kind == ProxyKind::Off;
    let dns_mode = state.dns_mode;
    let dns_modes = h_flex().gap_1().flex_wrap().children(DnsMode::ALL.map(|m| {
        let selected = m == dns_mode;
        Button::new(SharedString::from(format!("dns-mode-{}", m.title())))
            .small()
            .map(|b| if selected { b.primary() } else { b.outline() })
            .selected(selected)
            .label(m.title())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.network.dns_mode = m;
                cx.notify();
            }))
    }));
    v_flex()
        .gap_5()
        .child(
            group("Proxy", cx)
                .child(kinds)
                .when(!off, |this| {
                    this.child(field("Host", Input::new(&state.host).small(), cx))
                        .child(field("Port", Input::new(&state.port).small(), cx))
                        .when(kind != ProxyKind::Socks4, |this| {
                            this.child(field("User name", Input::new(&state.user).small(), cx))
                                .child(field(
                                    "Password",
                                    Input::new(&state.password).small().mask_toggle(),
                                    cx,
                                ))
                        })
                })
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("proxy-apply")
                                .small()
                                .primary()
                                .label("Apply")
                                .on_click(cx.listener(|this, _, _, cx| this.network_apply(cx))),
                        )
                        .child(
                            Button::new("proxy-test")
                                .small()
                                .outline()
                                .label("Test connection")
                                .loading(matches!(state.status, NetStatus::Testing))
                                .on_click(cx.listener(|this, _, _, cx| this.network_test(cx))),
                        ),
                )
                .child(status_line(status, color))
                .child(note(
                    "Applies to everything: YouTube Music, audio streams, artwork, lyrics and \
                     scrobbling. SOCKS5h resolves host names through the proxy. The password \
                     is kept in the keychain.",
                    cx,
                )),
        )
        .child(
            group("DNS", cx)
                .child(dns_modes)
                .when(dns_mode == DnsMode::Custom, |this| {
                    this.child(field("DoH URL", Input::new(&state.dns_url).small(), cx))
                })
                .when(dns_mode != DnsMode::System, |this| {
                    this.child(
                        h_flex()
                            .gap_2()
                            .child(
                                Switch::new("dns-fallback")
                                    .checked(state.dns_fallback)
                                    .small()
                                    .on_click(cx.listener(|this, on: &bool, _, cx| {
                                        this.network.dns_fallback = *on;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .child("Fall back to the system resolver when DoH fails"),
                            ),
                    )
                })
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("dns-apply")
                                .small()
                                .primary()
                                .label("Apply")
                                .on_click(cx.listener(|this, _, _, cx| this.network_apply(cx))),
                        )
                        .child(
                            Button::new("dns-test")
                                .small()
                                .outline()
                                .label("Test DNS")
                                .loading(matches!(state.status, NetStatus::Testing))
                                .on_click(cx.listener(|this, _, _, cx| this.network_test_dns(cx))),
                        ),
                )
                .child(note(
                    "DNS-over-HTTPS (RFC 8484) looks up host names over an encrypted HTTPS \
                     connection instead of plain DNS. Answers are cached for their TTL. With \
                     a SOCKS5h proxy the proxy resolves names itself. Quad9 is reached on its \
                     HTTP/1.1 port 5053, which some networks block.",
                    cx,
                )),
        )
        .into_any_element()
}
