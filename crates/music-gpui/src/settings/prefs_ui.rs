//! Settings-panel state and methods for the player / lyrics / Now Playing /
//! Discord activity / scrobble-rule options (model in [`super::prefs`]). The
//! entities (sliders, inputs) live in [`PrefsUi`], a field of `SettingsPanel`;
//! the sections (`playback.rs`, `lyrics.rs`, `appearance.rs`,
//! `integrations.rs`) render them.

use std::time::Duration;

use gpui::{AppContext as _, Context, Entity, Subscription, Window};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::slider::{SliderEvent, SliderState};
use music_core::lyrics::romanize::Script;
use music_core::lyrics::translate::{AiTranslator, GoogleTranslator, LyricsTranslator};
use music_core::lyrics::{LyricsServiceConfig, ProviderId, ProviderKeys};
use music_core::{OutputDevice, Player, Quality};

use super::file::Settings;
use super::panel::{SettingsEvent, SettingsPanel};
use super::prefs::{
    ActivityType, ButtonTarget, CROSSFADE_MAX, ImageSource, LYRICS_SIZE_RANGE,
    LYRICS_SPACING_RANGE, LyricsAnimation, MiniPlayerStyle, NowPlayingBackground, NowPlayingLayout,
    PITCH_MAX, SPEED_MAX, SPEED_MIN, TextAlign, TranslatorKind,
};
use crate::i18n::Key;
use crate::secrets::Secret;
use crate::util::off_thread;

/// Output devices as last listed (the list call can stall, so it runs off-thread).
pub(super) enum Devices {
    NotLoaded,
    Loading,
    Ready(Vec<OutputDevice>),
}

/// A Discord activity text field.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ActivityField {
    Name,
    Details,
    State,
    LargeImage,
    SmallImage,
    ButtonLabel(usize),
    ButtonUrl(usize),
}

pub(super) struct PrefsUi {
    pub crossfade: Entity<SliderState>,
    pub speed: Entity<SliderState>,
    pub pitch: Entity<SliderState>,
    pub sleep_custom: Entity<SliderState>,
    pub devices: Devices,

    pub lyrics_size: Entity<SliderState>,
    pub lyrics_spacing: Entity<SliderState>,
    pub paxsenix_key: Entity<InputState>,
    pub betterlyrics_key: Entity<InputState>,
    pub target_language: Entity<InputState>,
    pub ai_endpoint: Entity<InputState>,
    pub ai_model: Entity<InputState>,
    pub ai_key: Entity<InputState>,

    pub activity: Vec<(ActivityField, Entity<InputState>)>,

    pub scrobble_percent: Entity<SliderState>,
    pub scrobble_min: Entity<SliderState>,
    pub scrobble_delay: Entity<SliderState>,
}

fn slider_value(event: &SliderEvent) -> f32 {
    match event {
        SliderEvent::Change(v) | SliderEvent::Release(v) => v.start(),
    }
}

impl PrefsUi {
    pub fn new(
        s: &Settings,
        window: &mut Window,
        cx: &mut Context<SettingsPanel>,
    ) -> (Self, Vec<Subscription>) {
        let slider =
            |min: f32, max: f32, step: f32, value: f32, cx: &mut Context<SettingsPanel>| {
                cx.new(|_| {
                    SliderState::new()
                        .min(min)
                        .max(max)
                        .step(step)
                        .default_value(value)
                })
            };
        let input = |placeholder: &str,
                     value: &str,
                     masked: bool,
                     window: &mut Window,
                     cx: &mut Context<SettingsPanel>| {
            let (placeholder, value) = (placeholder.to_owned(), value.to_owned());
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .masked(masked)
                    .default_value(value)
            })
        };
        // An input whose placeholder is translated (and follows language switches).
        let tr_input = |placeholder: Key,
                        value: &str,
                        masked: bool,
                        window: &mut Window,
                        cx: &mut Context<SettingsPanel>| {
            let entity = input("", value, masked, window, cx);
            crate::i18n::track_placeholder(&entity, placeholder, window, cx);
            entity
        };
        let a = &s.audio;
        let l = &s.lyrics;
        let act = &s.discord.activity;
        let r = &s.scrobble_rules;
        let ui = Self {
            crossfade: slider(0., CROSSFADE_MAX, 0.5, a.crossfade_secs, cx),
            speed: slider(SPEED_MIN, SPEED_MAX, 0.05, a.speed, cx),
            pitch: slider(-PITCH_MAX, PITCH_MAX, 1., a.pitch, cx),
            sleep_custom: slider(1., 180., 1., a.sleep_custom_minutes as f32, cx),
            devices: Devices::NotLoaded,
            lyrics_size: slider(
                LYRICS_SIZE_RANGE.0,
                LYRICS_SIZE_RANGE.1,
                1.,
                l.text_size,
                cx,
            ),
            lyrics_spacing: slider(
                LYRICS_SPACING_RANGE.0,
                LYRICS_SPACING_RANGE.1,
                0.02,
                l.line_spacing,
                cx,
            ),
            paxsenix_key: tr_input(k!("prefs.placeholder.paxsenix_key"), "", true, window, cx),
            betterlyrics_key: tr_input(
                k!("prefs.placeholder.betterlyrics_key"),
                "",
                true,
                window,
                cx,
            ),
            target_language: input("en", &l.target_language, false, window, cx),
            ai_endpoint: input(
                "https://api.openai.com/v1",
                &l.ai_endpoint,
                false,
                window,
                cx,
            ),
            ai_model: input("gpt-4o-mini", &l.ai_model, false, window, cx),
            ai_key: tr_input(k!("prefs.placeholder.api_key"), "", true, window, cx),
            activity: vec![
                (
                    ActivityField::Name,
                    tr_input(
                        k!("prefs.placeholder.app_name"),
                        &act.name,
                        false,
                        window,
                        cx,
                    ),
                ),
                (
                    ActivityField::Details,
                    input("{title}", &act.details, false, window, cx),
                ),
                (
                    ActivityField::State,
                    tr_input(
                        k!("prefs.placeholder.discord_state"),
                        &act.state,
                        false,
                        window,
                        cx,
                    ),
                ),
                (
                    ActivityField::LargeImage,
                    tr_input(
                        k!("prefs.placeholder.asset"),
                        &act.large_image_custom,
                        false,
                        window,
                        cx,
                    ),
                ),
                (
                    ActivityField::SmallImage,
                    tr_input(
                        k!("prefs.placeholder.asset"),
                        &act.small_image_custom,
                        false,
                        window,
                        cx,
                    ),
                ),
                (
                    ActivityField::ButtonLabel(0),
                    tr_input(
                        k!("prefs.placeholder.button_label"),
                        &act.buttons[0].label,
                        false,
                        window,
                        cx,
                    ),
                ),
                (
                    ActivityField::ButtonUrl(0),
                    input("https://…", &act.buttons[0].url, false, window, cx),
                ),
                (
                    ActivityField::ButtonLabel(1),
                    tr_input(
                        k!("prefs.placeholder.button_label"),
                        &act.buttons[1].label,
                        false,
                        window,
                        cx,
                    ),
                ),
                (
                    ActivityField::ButtonUrl(1),
                    input("https://…", &act.buttons[1].url, false, window, cx),
                ),
            ],
            scrobble_percent: slider(10., 100., 5., r.percent as f32, cx),
            scrobble_min: slider(0., 120., 5., r.min_track_secs as f32, cx),
            scrobble_delay: slider(30., 600., 10., r.max_delay_secs as f32, cx),
        };

        let mut subs = Vec::new();
        let on_slider =
            |slider: &Entity<SliderState>,
             cx: &mut Context<SettingsPanel>,
             f: fn(&mut SettingsPanel, f32, &mut Context<SettingsPanel>)| {
                cx.subscribe(slider, move |this, _, event: &SliderEvent, cx| {
                    f(this, slider_value(event), cx)
                })
            };
        subs.push(on_slider(&ui.crossfade, cx, |this, v, cx| {
            this.update_audio(cx, |a| a.crossfade_secs = v)
        }));
        subs.push(on_slider(&ui.speed, cx, |this, v, cx| {
            this.update_audio(cx, |a| a.speed = v)
        }));
        subs.push(on_slider(&ui.pitch, cx, |this, v, cx| {
            this.update_audio(cx, |a| a.pitch = v)
        }));
        subs.push(on_slider(&ui.sleep_custom, cx, |this, v, cx| {
            let minutes = v.round() as u32;
            if this.settings.audio.sleep_custom_minutes != minutes {
                this.settings.audio.sleep_custom_minutes = minutes;
                this.changed(false, cx);
            }
        }));
        subs.push(on_slider(&ui.lyrics_size, cx, |this, v, cx| {
            this.update_lyrics_display(cx, |l| l.text_size = v)
        }));
        subs.push(on_slider(&ui.lyrics_spacing, cx, |this, v, cx| {
            this.update_lyrics_display(cx, |l| l.line_spacing = v)
        }));
        subs.push(on_slider(&ui.scrobble_percent, cx, |this, v, cx| {
            this.update_scrobble_rules(cx, |r| r.percent = v.round() as u32)
        }));
        subs.push(on_slider(&ui.scrobble_min, cx, |this, v, cx| {
            this.update_scrobble_rules(cx, |r| r.min_track_secs = v.round() as u32)
        }));
        subs.push(on_slider(&ui.scrobble_delay, cx, |this, v, cx| {
            this.update_scrobble_rules(cx, |r| r.max_delay_secs = v.round() as u32)
        }));

        // API keys are secrets: stored when the field loses focus / Enter.
        for (input, secret) in [
            (&ui.paxsenix_key, Secret::PaxsenixKey),
            (&ui.betterlyrics_key, Secret::BetterLyricsKey),
            (&ui.ai_key, Secret::AiTranslateKey),
        ] {
            subs.push(
                cx.subscribe(input, move |this, input, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        let value = input.read(cx).value().trim().to_owned();
                        this.set_lyrics_secret(secret, value, cx);
                    }
                }),
            );
        }
        for input in [&ui.target_language, &ui.ai_endpoint, &ui.ai_model] {
            subs.push(cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.sync_translation_inputs(cx);
                }
            }));
        }
        for (field, input) in &ui.activity {
            let field = *field;
            subs.push(
                cx.subscribe(input, move |this, input, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        let value = input.read(cx).value().to_string();
                        this.set_activity_text(field, value, cx);
                    }
                }),
            );
        }
        (ui, subs)
    }
}

impl SettingsPanel {
    // ---- what MusicApp reads ----------------------------------------------------

    /// Provider order, switches and API keys for `LyricsService`.
    pub fn lyrics_service_config(&self) -> LyricsServiceConfig {
        let l = &self.settings.lyrics;
        let secret = |s| self.secret(s).filter(|v: &String| !v.is_empty());
        LyricsServiceConfig {
            order: l.order(),
            disabled: ProviderId::ALL
                .into_iter()
                .filter(|id| !l.is_enabled(*id))
                .collect(),
            prefer_word_synced: l.prefer_word_synced,
            keys: ProviderKeys {
                paxsenix: secret(Secret::PaxsenixKey),
                betterlyrics: secret(Secret::BetterLyricsKey),
            },
            ..LyricsServiceConfig::default()
        }
    }

    /// Identifies the translator settings; translations are cached per track under it.
    pub fn translator_key(&self) -> String {
        let l = &self.settings.lyrics;
        match l.translator {
            TranslatorKind::Google => format!("google:{}", l.target_language),
            TranslatorKind::Ai => format!(
                "ai:{}:{}:{}:{}",
                l.ai_endpoint, l.ai_model, l.target_language, l.ai_key_set
            ),
        }
    }

    /// The configured translator; `Err` explains what is missing.
    pub fn translator(&self) -> Result<Box<dyn LyricsTranslator>, String> {
        let l = &self.settings.lyrics;
        match l.translator {
            TranslatorKind::Google => Ok(Box::new(GoogleTranslator::default())),
            TranslatorKind::Ai => {
                let key = self.secret(Secret::AiTranslateKey).unwrap_or_default();
                if l.ai_endpoint.is_empty() || l.ai_model.is_empty() {
                    return Err(t!("settings.lyrics.ai_missing").into());
                }
                Ok(Box::new(AiTranslator::new(
                    &l.ai_endpoint,
                    key,
                    &l.ai_model,
                )))
            }
        }
    }

    /// `Some(config)` while Discord presence is on.
    pub fn discord_config(&self) -> Option<music_core::discord::DiscordConfig> {
        let d = &self.settings.discord;
        (d.enabled && !d.client_id.is_empty()).then(|| d.activity.to_core(&d.client_id))
    }

    // ---- audio -------------------------------------------------------------------------

    fn update_audio(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut super::prefs::AudioConfig),
    ) {
        let before = self.settings.audio.clone();
        f(&mut self.settings.audio);
        self.settings.audio.normalize();
        if self.settings.audio != before {
            cx.emit(SettingsEvent::Audio);
            self.changed(false, cx);
        }
    }

    /// Saved like any audio option; the playing song switches to it too
    /// (`SettingsEvent::Quality`, see `MusicApp::switch_stream_quality`).
    pub fn set_quality(&mut self, quality: Quality, cx: &mut Context<Self>) {
        let before = self.settings.audio.quality;
        self.update_audio(cx, |a| a.quality = quality);
        if self.settings.audio.quality != before {
            cx.emit(SettingsEvent::Quality);
        }
    }

    pub fn set_stream_source(
        &mut self,
        mode: super::prefs::StreamSourceMode,
        cx: &mut Context<Self>,
    ) {
        self.update_audio(cx, |a| a.stream_source = mode);
    }

    pub fn set_skip_silence(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_audio(cx, |a| a.skip_silence = on);
    }

    pub fn set_auto_skip(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_audio(cx, |a| a.auto_skip = on);
    }

    pub fn set_normalization(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_audio(cx, |a| a.normalize = on);
    }

    /// Script / reset hook: moves the slider too.
    pub fn set_crossfade(&mut self, secs: f32, window: &mut Window, cx: &mut Context<Self>) {
        self.update_audio(cx, |a| a.crossfade_secs = secs);
        let v = self.settings.audio.crossfade_secs;
        self.prefs
            .crossfade
            .update(cx, |s, cx| s.set_value(v, window, cx));
    }

    pub fn set_speed_pitch(
        &mut self,
        speed: Option<f32>,
        pitch: Option<f32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_audio(cx, |a| {
            if let Some(speed) = speed {
                a.speed = speed;
            }
            if let Some(pitch) = pitch {
                a.pitch = pitch;
            }
        });
        let (speed, pitch) = (self.settings.audio.speed, self.settings.audio.pitch);
        self.prefs
            .speed
            .update(cx, |s, cx| s.set_value(speed, window, cx));
        self.prefs
            .pitch
            .update(cx, |s, cx| s.set_value(pitch, window, cx));
    }

    pub fn set_output_device(&mut self, device: Option<(String, String)>, cx: &mut Context<Self>) {
        let (id, name) = device.unzip();
        self.update_audio(cx, |a| {
            a.output_device = id;
            a.output_device_name = name;
        });
    }

    /// Lists output devices off the UI thread.
    pub fn refresh_devices(&mut self, cx: &mut Context<Self>) {
        if matches!(self.prefs.devices, Devices::Loading) {
            return;
        }
        self.prefs.devices = Devices::Loading;
        cx.spawn(async move |this, cx| {
            let devices = off_thread(Player::output_devices).await;
            this.update(cx, |this, cx| {
                this.prefs.devices = Devices::Ready(devices);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn sleep_custom(&self) -> Duration {
        Duration::from_secs(u64::from(self.settings.audio.sleep_custom_minutes) * 60)
    }

    // ---- lyrics ------------------------------------------------------------------------

    /// Display-only lyrics options (no refetch).
    fn update_lyrics_display(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut super::prefs::LyricsConfig),
    ) {
        let before = self.settings.lyrics.clone();
        f(&mut self.settings.lyrics);
        self.settings.lyrics.normalize();
        if self.settings.lyrics != before {
            cx.emit(SettingsEvent::NowPlaying);
            self.changed(false, cx);
        }
    }

    /// Provider options: the app rebuilds its `LyricsService`.
    fn update_lyrics_sources(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut super::prefs::LyricsConfig),
    ) {
        let before = self.settings.lyrics.clone();
        f(&mut self.settings.lyrics);
        self.settings.lyrics.normalize();
        if self.settings.lyrics != before {
            cx.emit(SettingsEvent::Lyrics);
            self.changed(false, cx);
        }
    }

    pub fn set_provider_enabled(&mut self, id: ProviderId, on: bool, cx: &mut Context<Self>) {
        self.update_lyrics_sources(cx, |l| {
            l.disabled.retain(|k| k != id.key());
            if !on {
                l.disabled.push(id.key().to_owned());
            }
        });
    }

    pub fn move_provider(&mut self, id: ProviderId, delta: isize, cx: &mut Context<Self>) {
        self.update_lyrics_sources(cx, |l| {
            if let Some(from) = l.provider_order.iter().position(|k| k == id.key()) {
                let to = (from as isize + delta).clamp(0, l.provider_order.len() as isize - 1);
                let key = l.provider_order.remove(from);
                l.provider_order.insert(to as usize, key);
            }
        });
    }

    pub fn reset_provider_order(&mut self, cx: &mut Context<Self>) {
        self.update_lyrics_sources(cx, |l| {
            let default = super::prefs::LyricsConfig::default();
            l.provider_order = default.provider_order;
            l.disabled.clear();
        });
    }

    pub fn set_prefer_word_synced(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_lyrics_sources(cx, |l| l.prefer_word_synced = on);
    }

    fn set_lyrics_secret(&mut self, secret: Secret, value: String, cx: &mut Context<Self>) {
        if self.secret(secret).unwrap_or_default() == value {
            return;
        }
        let set = !value.is_empty();
        self.put_secret(secret, set.then_some(value), cx);
        let l = &mut self.settings.lyrics;
        match secret {
            Secret::PaxsenixKey => l.paxsenix_key_set = set,
            Secret::BetterLyricsKey => l.betterlyrics_key_set = set,
            Secret::AiTranslateKey => l.ai_key_set = set,
            _ => {}
        }
        if secret == Secret::AiTranslateKey {
            cx.emit(SettingsEvent::NowPlaying);
        } else {
            cx.emit(SettingsEvent::Lyrics);
        }
        self.changed(false, cx);
    }

    fn sync_translation_inputs(&mut self, cx: &mut Context<Self>) {
        let lang = self
            .prefs
            .target_language
            .read(cx)
            .value()
            .trim()
            .to_owned();
        let endpoint = self.prefs.ai_endpoint.read(cx).value().trim().to_owned();
        let model = self.prefs.ai_model.read(cx).value().trim().to_owned();
        self.update_lyrics_display(cx, |l| {
            l.target_language = lang;
            l.ai_endpoint = endpoint;
            l.ai_model = model;
        });
    }

    pub fn set_translate(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_lyrics_display(cx, |l| l.translate = on);
    }

    pub fn set_translator(&mut self, kind: TranslatorKind, cx: &mut Context<Self>) {
        self.update_lyrics_display(cx, |l| l.translator = kind);
    }

    pub fn set_romanize(&mut self, script: Script, on: bool, cx: &mut Context<Self>) {
        self.update_lyrics_display(cx, |l| l.romanize.set(script, on));
    }

    pub fn set_lyrics_align(&mut self, align: TextAlign, cx: &mut Context<Self>) {
        self.update_lyrics_display(cx, |l| l.align = align);
    }

    pub fn set_lyrics_animation(&mut self, animation: LyricsAnimation, cx: &mut Context<Self>) {
        self.update_lyrics_display(cx, |l| l.animation = animation);
    }

    pub fn set_background_vocals(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_lyrics_display(cx, |l| l.show_background_vocals = on);
    }

    /// Script hook: text size / spacing (moves the sliders too).
    pub fn set_lyrics_metrics(
        &mut self,
        size: Option<f32>,
        spacing: Option<f32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_lyrics_display(cx, |l| {
            if let Some(size) = size {
                l.text_size = size;
            }
            if let Some(spacing) = spacing {
                l.line_spacing = spacing;
            }
        });
        let (size, spacing) = (
            self.settings.lyrics.text_size,
            self.settings.lyrics.line_spacing,
        );
        self.prefs
            .lyrics_size
            .update(cx, |s, cx| s.set_value(size, window, cx));
        self.prefs
            .lyrics_spacing
            .update(cx, |s, cx| s.set_value(spacing, window, cx));
    }

    // ---- Now Playing / mini player -------------------------------------------------------

    fn update_now_playing(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut super::prefs::NowPlayingConfig),
    ) {
        let before = self.settings.now_playing;
        f(&mut self.settings.now_playing);
        if self.settings.now_playing != before {
            cx.emit(SettingsEvent::NowPlaying);
            self.changed(false, cx);
        }
    }

    pub fn set_np_layout(&mut self, layout: NowPlayingLayout, cx: &mut Context<Self>) {
        self.update_now_playing(cx, |n| n.layout = layout);
    }

    pub fn set_np_background(&mut self, background: NowPlayingBackground, cx: &mut Context<Self>) {
        self.update_now_playing(cx, |n| n.background = background);
    }

    pub fn set_mini_style(&mut self, style: MiniPlayerStyle, cx: &mut Context<Self>) {
        self.update_now_playing(cx, |n| n.mini_player = style);
    }

    // ---- Discord activity / scrobble rules -------------------------------------------------

    fn update_activity(
        &mut self,
        cx: &mut Context<Self>,
        debounce: bool,
        f: impl FnOnce(&mut super::prefs::ActivityConfig),
    ) {
        let before = self.settings.discord.activity.clone();
        f(&mut self.settings.discord.activity);
        if self.settings.discord.activity != before {
            if !debounce {
                cx.emit(SettingsEvent::Integrations);
            }
            self.changed(debounce, cx);
        }
    }

    fn set_activity_text(&mut self, field: ActivityField, value: String, cx: &mut Context<Self>) {
        self.update_activity(cx, true, |a| match field {
            ActivityField::Name => a.name = value,
            ActivityField::Details => a.details = value,
            ActivityField::State => a.state = value,
            ActivityField::LargeImage => a.large_image_custom = value,
            ActivityField::SmallImage => a.small_image_custom = value,
            ActivityField::ButtonLabel(i) => a.buttons[i].label = value,
            ActivityField::ButtonUrl(i) => a.buttons[i].url = value,
        });
    }

    pub fn set_activity_kind(&mut self, kind: ActivityType, cx: &mut Context<Self>) {
        self.update_activity(cx, false, |a| a.kind = kind);
    }

    pub fn set_activity_image(&mut self, small: bool, source: ImageSource, cx: &mut Context<Self>) {
        self.update_activity(cx, false, |a| {
            if small {
                a.small_image = source;
            } else {
                a.large_image = source;
            }
        });
    }

    pub fn set_activity_button(
        &mut self,
        index: usize,
        target: ButtonTarget,
        cx: &mut Context<Self>,
    ) {
        self.update_activity(cx, false, |a| a.buttons[index].target = target);
    }

    pub fn set_show_when_paused(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_activity(cx, false, |a| a.show_when_paused = on);
    }

    fn update_scrobble_rules(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut super::prefs::ScrobbleRulesConfig),
    ) {
        let before = self.settings.scrobble_rules;
        f(&mut self.settings.scrobble_rules);
        self.settings.scrobble_rules.normalize();
        if self.settings.scrobble_rules != before {
            self.changed(true, cx);
        }
    }

    /// Fills the secret inputs once the keychain has been read.
    pub(super) fn fill_secret_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (secret, input) in [
            (Secret::PaxsenixKey, self.prefs.paxsenix_key.clone()),
            (Secret::BetterLyricsKey, self.prefs.betterlyrics_key.clone()),
            (Secret::AiTranslateKey, self.prefs.ai_key.clone()),
        ] {
            if let Some(value) = self.secret(secret) {
                input.update(cx, |input, cx| input.set_value(value, window, cx));
            }
        }
    }
}

// ---- shared widgets for the sections ------------------------------------------------

/// A row of buttons, one per option, the current one highlighted.
pub(super) fn choice<T: Copy + PartialEq + 'static>(
    id: &str,
    options: &[T],
    current: T,
    title: fn(T) -> &'static str,
    on_pick: fn(&mut SettingsPanel, T, &mut Context<SettingsPanel>),
    cx: &Context<SettingsPanel>,
) -> gpui::Div {
    use gpui::{ParentElement as _, Styled as _, prelude::FluentBuilder as _};
    use gpui_component::button::{Button, ButtonVariants as _};
    use gpui_component::{Selectable as _, Sizable as _};
    gpui_component::h_flex()
        .gap_1()
        .flex_wrap()
        .children(options.iter().enumerate().map(|(i, &option)| {
            let selected = option == current;
            Button::new(gpui::SharedString::from(format!("{id}-{i}")))
                .small()
                .map(|b| if selected { b.primary() } else { b.outline() })
                .selected(selected)
                .label(title(option))
                .on_click(cx.listener(move |this, _, _, cx| on_pick(this, option, cx)))
        }))
}

/// Label, slider and the formatted value on one line.
pub(super) fn slider_row(
    label: &'static str,
    value: String,
    slider: &Entity<SliderState>,
    cx: &gpui::App,
) -> gpui::Div {
    use gpui::{ParentElement as _, Styled as _, div, px};
    use gpui_component::ActiveTheme as _;
    gpui_component::h_flex()
        .gap_3()
        .child(div().w(px(150.)).flex_none().text_sm().child(label))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(gpui_component::slider::Slider::new(slider).horizontal()),
        )
        .child(
            div()
                .w(px(76.))
                .flex_none()
                .text_sm()
                .text_right()
                .text_color(cx.theme().muted_foreground)
                .child(value),
        )
}
