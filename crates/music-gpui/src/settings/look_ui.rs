//! Appearance → "App-wide look" and "Motion & effects" (`look_prefs.rs`), plus
//! the panel methods that change them.

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

use super::bar_prefs::GradientDirection;
use super::look_prefs::{
    AppLookConfig, AppLookStyle, ArtCorners, CardSize, EffectsConfig, LOOK_INTENSITIES, MotionPref,
};
use super::panel::{SettingsEvent, SettingsPanel};
use super::prefs_ui::choice;
use super::widgets::{group, note, row};

impl SettingsPanel {
    fn update_app_look(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut AppLookConfig)) {
        let before = self.settings.app_look;
        f(&mut self.settings.app_look);
        self.settings.app_look.normalize();
        if self.settings.app_look != before {
            // The theme (artwork tint) and the backdrop follow at once.
            cx.emit(SettingsEvent::Appearance);
            cx.emit(SettingsEvent::NowPlaying);
            self.changed(false, cx);
        }
    }

    fn update_effects(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut EffectsConfig)) {
        let before = self.settings.effects;
        f(&mut self.settings.effects);
        if self.settings.effects != before {
            cx.emit(SettingsEvent::Appearance);
            cx.emit(SettingsEvent::NowPlaying);
            self.changed(false, cx);
        }
    }

    pub fn set_app_look_style(&mut self, style: AppLookStyle, cx: &mut Context<Self>) {
        self.update_app_look(cx, |l| l.style = style);
    }

    pub fn set_app_look_follow(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_app_look(cx, |l| l.follow_bar = on);
    }

    pub fn set_app_look_direction(&mut self, d: GradientDirection, cx: &mut Context<Self>) {
        self.update_app_look(cx, |l| l.direction = d);
    }

    pub fn set_app_look_intensity(&mut self, v: f32, cx: &mut Context<Self>) {
        self.update_app_look(cx, |l| l.intensity = v);
    }

    pub fn set_app_look_dim(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_app_look(cx, |l| l.dim_when_paused = on);
    }

    pub fn set_app_look_tint_surfaces(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_app_look(cx, |l| l.tint_surfaces = on);
    }

    pub fn set_app_look_animate(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_app_look(cx, |l| l.animate = on);
    }

    pub fn set_motion(&mut self, m: MotionPref, cx: &mut Context<Self>) {
        self.update_effects(cx, |e| e.motion = m);
    }

    pub fn set_page_transitions(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_effects(cx, |e| e.page_transitions = on);
    }

    pub fn set_card_hover(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_effects(cx, |e| e.card_hover = on);
    }

    pub fn set_artwork_headers(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_effects(cx, |e| e.artwork_headers = on);
    }

    pub fn set_artwork_corners(&mut self, c: ArtCorners, cx: &mut Context<Self>) {
        self.update_effects(cx, |e| e.artwork_corners = c);
    }

    pub fn set_artwork_shadow(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_effects(cx, |e| e.artwork_shadow = on);
    }

    pub fn set_card_size(&mut self, size: CardSize, cx: &mut Context<Self>) {
        self.update_effects(cx, |e| e.card_size = size);
    }

    pub fn set_translucent_sidebar(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_effects(cx, |e| e.translucent_sidebar = on);
    }
}

type Setter = fn(&mut SettingsPanel, bool, &mut Context<SettingsPanel>);

fn switch(id: &'static str, on: bool, f: Setter, cx: &Context<SettingsPanel>) -> Switch {
    Switch::new(id)
        .checked(on)
        .on_click(cx.listener(move |this, checked: &bool, _, cx| f(this, *checked, cx)))
}

/// The "App-wide look" group.
pub(super) fn render_app_look(
    panel: &mut SettingsPanel,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let look = panel.settings.app_look;
    let bar = panel.settings.player_bar;
    let on = look.style != AppLookStyle::Off;
    let following = on && look.follow_bar && bar.background_effect;
    let effective = look.effective(&bar);
    let styles = choice(
        "app-look-style",
        AppLookStyle::ALL,
        look.style,
        AppLookStyle::title,
        |this, s, cx| this.set_app_look_style(s, cx),
        cx,
    );
    let directions = choice(
        "app-look-direction",
        &GradientDirection::ALL,
        look.direction,
        GradientDirection::title,
        |this, d, cx| this.set_app_look_direction(d, cx),
        cx,
    );
    let intensities =
        h_flex()
            .gap_1()
            .flex_wrap()
            .children(LOOK_INTENSITIES.iter().enumerate().map(|(i, &value)| {
                let selected = (look.intensity - value).abs() < 0.01;
                Button::new(SharedString::from(format!("app-look-intensity-{i}")))
                    .small()
                    .map(|b| if selected { b.primary() } else { b.outline() })
                    .selected(selected)
                    .label(t!(
                        "settings.bar.intensity_value",
                        percent = (value * 100.).round()
                    ))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.set_app_look_intensity(value, cx)),
                    )
            }));
    let follow = switch(
        "app-look-follow",
        look.follow_bar,
        SettingsPanel::set_app_look_follow,
        cx,
    )
    .disabled(!on);
    let dim = switch(
        "app-look-dim",
        look.dim_when_paused,
        SettingsPanel::set_app_look_dim,
        cx,
    );
    let tint = switch(
        "app-look-tint-surfaces",
        look.tint_surfaces,
        SettingsPanel::set_app_look_tint_surfaces,
        cx,
    );
    let animate = switch(
        "app-look-animate",
        look.animate,
        SettingsPanel::set_app_look_animate,
        cx,
    );
    let own = on && !following;
    group(t!("settings.look.group"), cx)
        .child(
            v_flex()
                .gap_2()
                .child(row(
                    t!("settings.look.style"),
                    Some(t!("settings.look.style_note")),
                    styles,
                    cx,
                ))
                .when(on, |this| {
                    this.child(row(
                        t!("settings.look.follow_bar"),
                        Some(t!("settings.look.follow_bar_note")),
                        follow,
                        cx,
                    ))
                })
                .when(following, |this| {
                    this.child(note(
                        t!("settings.look.following", style = bar.background.title()),
                        cx,
                    ))
                })
                .when(own && look.style == AppLookStyle::Gradient, |this| {
                    this.child(row(t!("settings.look.direction"), None, directions, cx))
                })
                .when(own, |this| {
                    this.child(row(t!("settings.look.intensity"), None, intensities, cx))
                        .child(row(t!("settings.look.dim_when_paused"), None, dim, cx))
                })
                .when(on && effective.draws_backdrop(), |this| {
                    this.child(row(
                        t!("settings.look.tint_surfaces"),
                        Some(t!("settings.look.tint_surfaces_note")),
                        tint,
                        cx,
                    ))
                })
                .when(own && look.style == AppLookStyle::Gradient, |this| {
                    this.child(row(
                        t!("settings.look.animate"),
                        Some(t!("settings.look.animate_note")),
                        animate,
                        cx,
                    ))
                }),
        )
        .child(note(t!("settings.look.cpu_note"), cx))
        .into_any_element()
}

/// The "Motion & effects" group.
pub(super) fn render_effects(
    panel: &mut SettingsPanel,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let fx = panel.settings.effects;
    let levels = choice(
        "motion-level",
        MotionPref::ALL,
        fx.motion,
        MotionPref::title,
        |this, m, cx| this.set_motion(m, cx),
        cx,
    );
    let system = (fx.motion == MotionPref::System).then(|| {
        if crate::effects::os_reduce_motion() {
            t!("settings.motion.system_reduced")
        } else {
            t!("settings.motion.system_full")
        }
    });
    let corners = choice(
        "art-corners",
        ArtCorners::ALL,
        fx.artwork_corners,
        ArtCorners::title,
        |this, c, cx| this.set_artwork_corners(c, cx),
        cx,
    );
    let sizes = choice(
        "card-size",
        CardSize::ALL,
        fx.card_size,
        CardSize::title,
        |this, s, cx| this.set_card_size(s, cx),
        cx,
    );
    let transitions = switch(
        "motion-page-transitions",
        fx.page_transitions,
        SettingsPanel::set_page_transitions,
        cx,
    )
    .disabled(fx.motion == MotionPref::Off);
    let hover = switch(
        "motion-card-hover",
        fx.card_hover,
        SettingsPanel::set_card_hover,
        cx,
    );
    let headers = switch(
        "motion-artwork-headers",
        fx.artwork_headers,
        SettingsPanel::set_artwork_headers,
        cx,
    );
    let shadow = switch(
        "motion-artwork-shadow",
        fx.artwork_shadow,
        SettingsPanel::set_artwork_shadow,
        cx,
    );
    let translucent = switch(
        "motion-translucent-sidebar",
        fx.translucent_sidebar,
        SettingsPanel::set_translucent_sidebar,
        cx,
    );
    group(t!("settings.motion.group"), cx)
        .child(
            v_flex()
                .gap_2()
                .child(row(
                    t!("settings.motion.animations"),
                    Some(t!("settings.motion.animations_note")),
                    levels,
                    cx,
                ))
                .children(system.map(|text| note(text, cx)))
                .child(row(
                    t!("settings.motion.page_transitions"),
                    Some(t!("settings.motion.page_transitions_note")),
                    transitions,
                    cx,
                ))
                .child(row(
                    t!("settings.motion.card_hover"),
                    Some(t!("settings.motion.card_hover_note")),
                    hover,
                    cx,
                ))
                .child(row(
                    t!("settings.motion.artwork_headers"),
                    Some(t!("settings.motion.artwork_headers_note")),
                    headers,
                    cx,
                ))
                .child(row(
                    t!("settings.motion.artwork_corners"),
                    None,
                    corners,
                    cx,
                ))
                .child(row(t!("settings.motion.artwork_shadow"), None, shadow, cx))
                .child(row(t!("settings.motion.card_size"), None, sizes, cx))
                .when(cfg!(target_os = "macos"), |this| {
                    this.child(row(
                        t!("settings.motion.translucent_sidebar"),
                        Some(t!("settings.motion.translucent_sidebar_note")),
                        translucent,
                        cx,
                    ))
                }),
        )
        .child(note(t!("settings.motion.cpu_note"), cx))
        .into_any_element()
}
