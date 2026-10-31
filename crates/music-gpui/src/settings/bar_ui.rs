//! Appearance → Player bar: the progress bar style, the background that reacts
//! to play / pause, times, volume scrolling, mini player and Now Playing
//! visualizer switches (`bar_prefs.rs`), plus the panel methods that change them.

use gpui::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _,
    prelude::FluentBuilder as _,
};
use gpui_component::{
    Disableable as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    switch::Switch,
    v_flex,
};

use super::bar_prefs::{
    BarArtworkStyle, BarBackground, GRADIENT_INTENSITIES, GradientDirection, NpVisualizerStyle,
    PlayerBarConfig, ProgressStyle,
};
use super::panel::{SettingsEvent, SettingsPanel};
use super::prefs_ui::choice;
use super::widgets::{group_at, note, row_at};

impl SettingsPanel {
    fn update_player_bar(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut PlayerBarConfig)) {
        let before = self.settings.player_bar;
        f(&mut self.settings.player_bar);
        if self.settings.player_bar != before {
            // The app re-renders the bar (and the mini player) on this event.
            cx.emit(SettingsEvent::NowPlaying);
            self.changed(false, cx);
        }
    }

    pub fn set_progress_style(&mut self, style: ProgressStyle, cx: &mut Context<Self>) {
        self.update_player_bar(cx, |b| b.progress_style = style);
    }

    pub fn set_bar_background(
        &mut self,
        on: bool,
        style: Option<BarBackground>,
        cx: &mut Context<Self>,
    ) {
        self.update_player_bar(cx, |b| {
            b.background_effect = on;
            if let Some(style) = style {
                b.background = style;
            }
        });
    }

    pub fn set_dim_when_paused(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_player_bar(cx, |b| b.dim_when_paused = on);
    }

    pub fn set_gradient_direction(&mut self, direction: GradientDirection, cx: &mut Context<Self>) {
        self.update_player_bar(cx, |b| b.gradient_direction = direction);
    }

    pub fn set_gradient_intensity(&mut self, intensity: f32, cx: &mut Context<Self>) {
        self.update_player_bar(cx, |b| {
            b.gradient_intensity = intensity;
            b.normalize();
        });
    }

    pub fn set_animate_gradient(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_player_bar(cx, |b| b.animate_gradient = on);
    }

    pub fn set_peak_caps(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_player_bar(cx, |b| b.peak_caps = on);
    }

    pub fn set_show_remaining(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_player_bar(cx, |b| b.show_remaining = on);
    }

    pub fn set_scroll_volume(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_player_bar(cx, |b| b.scroll_volume = on);
    }

    pub fn set_mini_follows_bar(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_player_bar(cx, |b| b.mini_follows_bar = on);
    }

    pub fn set_np_visualizer(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_player_bar(cx, |b| b.np_visualizer = on);
    }

    pub fn set_np_visualizer_style(&mut self, style: NpVisualizerStyle, cx: &mut Context<Self>) {
        self.update_player_bar(cx, |b| b.np_visualizer_style = style);
    }

    pub fn set_bar_artwork_style(&mut self, style: BarArtworkStyle, cx: &mut Context<Self>) {
        self.update_player_bar(cx, |b| b.artwork_style = style);
    }
}

/// The "Player bar" group of the Appearance section.
pub(super) fn render(panel: &mut SettingsPanel, cx: &mut Context<SettingsPanel>) -> AnyElement {
    let bar = panel.settings.player_bar;
    let switch = |id: &'static str,
                  on: bool,
                  f: fn(&mut SettingsPanel, bool, &mut Context<SettingsPanel>),
                  cx: &Context<SettingsPanel>| {
        Switch::new(id)
            .checked(on)
            .on_click(cx.listener(move |this, checked: &bool, _, cx| f(this, *checked, cx)))
    };
    let styles = choice(
        "bar-progress-style",
        &ProgressStyle::ALL,
        bar.progress_style,
        ProgressStyle::title,
        |this, s, cx| this.set_progress_style(s, cx),
        cx,
    );
    let artwork = choice(
        "bar-artwork-style",
        &BarArtworkStyle::ALL,
        bar.artwork_style,
        BarArtworkStyle::title,
        |this, a, cx| this.set_bar_artwork_style(a, cx),
        cx,
    );
    let backgrounds = choice(
        "bar-background-style",
        &BarBackground::ALL,
        bar.background,
        BarBackground::title,
        |this, b, cx| this.set_bar_background(true, Some(b), cx),
        cx,
    );
    let effect = Switch::new("bar-background-effect")
        .checked(bar.background_effect)
        .on_click(
            cx.listener(|this, checked: &bool, _, cx| this.set_bar_background(*checked, None, cx)),
        );
    let peaks = switch(
        "bar-peak-caps",
        bar.peak_caps,
        SettingsPanel::set_peak_caps,
        cx,
    )
    .disabled(bar.progress_style != ProgressStyle::Equalizer);
    let dim = switch(
        "bar-dim-paused",
        bar.dim_when_paused,
        SettingsPanel::set_dim_when_paused,
        cx,
    )
    .disabled(!bar.background_effect);
    let gradient = bar.background_effect && bar.background == BarBackground::Gradient;
    let directions = choice(
        "bar-gradient-direction",
        &GradientDirection::ALL,
        bar.gradient_direction,
        GradientDirection::title,
        |this, d, cx| this.set_gradient_direction(d, cx),
        cx,
    );
    let intensities =
        h_flex()
            .gap_1()
            .flex_wrap()
            .children(GRADIENT_INTENSITIES.iter().enumerate().map(|(i, &value)| {
                let selected = (bar.gradient_intensity - value).abs() < 0.01;
                Button::new(SharedString::from(format!("bar-gradient-intensity-{i}")))
                    .small()
                    .map(|b| if selected { b.primary() } else { b.outline() })
                    .selected(selected)
                    .label(t!(
                        "settings.bar.intensity_value",
                        percent = (value * 100.).round()
                    ))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.set_gradient_intensity(value, cx)),
                    )
            }));
    let animate = switch(
        "bar-animate-gradient",
        bar.animate_gradient,
        SettingsPanel::set_animate_gradient,
        cx,
    );
    let remaining = switch(
        "bar-show-remaining",
        bar.show_remaining,
        SettingsPanel::set_show_remaining,
        cx,
    );
    let scroll = switch(
        "bar-scroll-volume",
        bar.scroll_volume,
        SettingsPanel::set_scroll_volume,
        cx,
    );
    let mini = switch(
        "bar-mini-follows",
        bar.mini_follows_bar,
        SettingsPanel::set_mini_follows_bar,
        cx,
    );
    let np = switch(
        "bar-np-visualizer",
        bar.np_visualizer,
        SettingsPanel::set_np_visualizer,
        cx,
    );
    let np_style = choice(
        "bar-np-visualizer-style",
        &NpVisualizerStyle::ALL,
        bar.np_visualizer_style,
        NpVisualizerStyle::title,
        |this, s, cx| this.set_np_visualizer_style(s, cx),
        cx,
    );
    group_at("appearance.bar", t!("settings.bar.group"), cx)
        .child(
            v_flex()
                .gap_2()
                .child(row_at(
                    "appearance.bar.progress_style",
                    t!("settings.bar.progress_style"),
                    (bar.progress_style == ProgressStyle::Waveform)
                        .then(|| t!("settings.bar.waveform_note")),
                    styles,
                    cx,
                ))
                .child(row_at(
                    "appearance.bar.artwork",
                    t!("settings.bar.artwork_style"),
                    Some(t!("settings.bar.artwork_style_note")),
                    artwork,
                    cx,
                ))
                .child(row_at(
                    "appearance.bar.peak_caps",
                    t!("settings.bar.peak_caps"),
                    Some(t!("settings.bar.peak_caps_note")),
                    peaks,
                    cx,
                ))
                .child(row_at(
                    "appearance.bar.background_effect",
                    t!("settings.bar.background_effect"),
                    Some(t!("settings.bar.background_effect_note")),
                    effect,
                    cx,
                ))
                .child(row_at(
                    "appearance.bar.background_style",
                    t!("settings.bar.background_style"),
                    None,
                    backgrounds,
                    cx,
                ))
                .child(row_at(
                    "appearance.bar.dim",
                    t!("settings.bar.dim_when_paused"),
                    None,
                    dim,
                    cx,
                ))
                .when(gradient, |this| {
                    this.child(row_at(
                        "appearance.bar.gradient_direction",
                        t!("settings.bar.gradient_direction"),
                        None,
                        directions,
                        cx,
                    ))
                    .child(row_at(
                        "appearance.bar.gradient_intensity",
                        t!("settings.bar.gradient_intensity"),
                        None,
                        intensities,
                        cx,
                    ))
                    .child(row_at(
                        "appearance.bar.animate_gradient",
                        t!("settings.bar.animate_gradient"),
                        Some(t!("settings.bar.animate_gradient_note")),
                        animate,
                        cx,
                    ))
                })
                .child(row_at(
                    "appearance.bar.show_remaining",
                    t!("settings.bar.show_remaining"),
                    None,
                    remaining,
                    cx,
                ))
                .child(row_at(
                    "appearance.bar.scroll_volume",
                    t!("settings.bar.scroll_volume"),
                    None,
                    scroll,
                    cx,
                ))
                .child(row_at(
                    "appearance.bar.mini_follows",
                    t!("settings.bar.mini_follows"),
                    None,
                    mini,
                    cx,
                ))
                .child(row_at(
                    "appearance.bar.np_visualizer",
                    t!("settings.bar.np_visualizer"),
                    Some(t!("settings.bar.np_visualizer_note")),
                    np,
                    cx,
                ))
                .when(bar.np_visualizer, |this| {
                    this.child(row_at(
                        "appearance.bar.np_visualizer_style",
                        t!("settings.bar.np_visualizer_style"),
                        None,
                        np_style,
                        cx,
                    ))
                }),
        )
        .child(note(t!("settings.bar.cpu_note"), cx))
        .child(super::bar_items_ui::render(panel, cx))
        .into_any_element()
}
