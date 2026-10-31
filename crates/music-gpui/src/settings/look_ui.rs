//! Appearance → "App-wide look" and "Motion & effects" (`look_prefs.rs`), plus
//! the panel methods that change them.

use std::path::PathBuf;

use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    switch::Switch,
    v_flex,
};

use super::bar_prefs::GradientDirection;
use super::look_prefs::{
    AppLookConfig, AppLookStyle, ArtCorners, CardSize, EffectsConfig, LOOK_INTENSITIES, MotionPref,
    WINDOW_OPACITIES, WindowMaterial,
};
use super::panel::{SettingsEvent, SettingsPanel};
use super::prefs_ui::choice;
use super::widgets::{Anchor as _, group_at, note, row_at};
use crate::app_icon::AppIconChoice;

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

    pub fn set_app_look_react(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_app_look(cx, |l| l.react = on);
    }

    pub fn set_window_material(&mut self, m: WindowMaterial, cx: &mut Context<Self>) {
        self.update_effects(cx, |e| e.window_material = m);
    }

    pub fn set_window_opacity(&mut self, v: f32, cx: &mut Context<Self>) {
        self.update_effects(cx, |e| {
            e.window_opacity = v;
            e.normalize();
        });
    }

    pub fn set_app_icon(&mut self, choice: AppIconChoice, cx: &mut Context<Self>) {
        if self.settings.appearance.app_icon != choice {
            self.settings.appearance.app_icon = choice;
            cx.emit(SettingsEvent::Appearance);
            self.changed(false, cx);
        }
    }

    /// Stores `path` as the custom app icon and picks it (also `@app-icon=PATH`).
    pub fn import_app_icon(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dest) = crate::app_icon::custom_path() else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let result =
                crate::util::off_thread(move || crate::app_icon::import_custom(&path, &dest)).await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(()) => {
                    this.app_icon_generation += 1;
                    this.set_app_icon(AppIconChoice::Custom, cx);
                    cx.emit(SettingsEvent::Appearance);
                    cx.notify();
                }
                Err(e) => crate::app::notify(window, cx, t!("app_icon.error", error = e)),
            })
            .ok();
        })
        .detach();
    }

    fn pick_app_icon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(t!("settings.app_icon.choose").into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = picked.await else {
                return;
            };
            let Some(path) = paths.pop() else { return };
            this.update_in(cx, |this, window, cx| {
                this.import_app_icon(path, window, cx)
            })
            .ok();
        })
        .detach();
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
    let react = switch(
        "app-look-react",
        look.react,
        SettingsPanel::set_app_look_react,
        cx,
    )
    .disabled(crate::effects::motion() != crate::effects::Motion::Full);
    let own = on && !following;
    group_at("appearance.look", t!("settings.look.group"), cx)
        .child(
            v_flex()
                .gap_2()
                .child(row_at(
                    "appearance.look.style",
                    t!("settings.look.style"),
                    Some(t!("settings.look.style_note")),
                    styles,
                    cx,
                ))
                .when(on, |this| {
                    this.child(row_at(
                        "appearance.look.follow_bar",
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
                    this.child(row_at(
                        "appearance.look.direction",
                        t!("settings.look.direction"),
                        None,
                        directions,
                        cx,
                    ))
                })
                .when(own, |this| {
                    this.child(row_at(
                        "appearance.look.intensity",
                        t!("settings.look.intensity"),
                        None,
                        intensities,
                        cx,
                    ))
                    .child(row_at(
                        "appearance.look.dim",
                        t!("settings.look.dim_when_paused"),
                        None,
                        dim,
                        cx,
                    ))
                })
                .when(on && effective.draws_backdrop(), |this| {
                    this.child(row_at(
                        "appearance.look.tint_surfaces",
                        t!("settings.look.tint_surfaces"),
                        Some(t!("settings.look.tint_surfaces_note")),
                        tint,
                        cx,
                    ))
                })
                .when(own && look.style == AppLookStyle::Gradient, |this| {
                    this.child(row_at(
                        "appearance.look.animate",
                        t!("settings.look.animate"),
                        Some(t!("settings.look.animate_note")),
                        animate,
                        cx,
                    ))
                })
                .when(on && effective.draws_backdrop(), |this| {
                    this.child(row_at(
                        "appearance.look.react",
                        t!("settings.look.react"),
                        Some(t!("settings.look.react_note")),
                        react,
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
    group_at("appearance.motion", t!("settings.motion.group"), cx)
        .child(
            v_flex()
                .gap_2()
                .child(row_at(
                    "appearance.motion.animations",
                    t!("settings.motion.animations"),
                    Some(t!("settings.motion.animations_note")),
                    levels,
                    cx,
                ))
                .children(system.map(|text| note(text, cx)))
                .child(row_at(
                    "appearance.motion.page_transitions",
                    t!("settings.motion.page_transitions"),
                    Some(t!("settings.motion.page_transitions_note")),
                    transitions,
                    cx,
                ))
                .child(row_at(
                    "appearance.motion.card_hover",
                    t!("settings.motion.card_hover"),
                    Some(t!("settings.motion.card_hover_note")),
                    hover,
                    cx,
                ))
                .child(row_at(
                    "appearance.motion.artwork_headers",
                    t!("settings.motion.artwork_headers"),
                    Some(t!("settings.motion.artwork_headers_note")),
                    headers,
                    cx,
                ))
                .child(row_at(
                    "appearance.motion.artwork_corners",
                    t!("settings.motion.artwork_corners"),
                    None,
                    corners,
                    cx,
                ))
                .child(row_at(
                    "appearance.motion.artwork_shadow",
                    t!("settings.motion.artwork_shadow"),
                    None,
                    shadow,
                    cx,
                ))
                .child(row_at(
                    "appearance.motion.card_size",
                    t!("settings.motion.card_size"),
                    None,
                    sizes,
                    cx,
                ))
                .when(cfg!(target_os = "macos"), |this| {
                    this.child(row_at(
                        "appearance.motion.translucent_sidebar",
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

/// Appearance → Window: the window background (material, opacity) and the app
/// icon, below the title bar switch (`appearance.rs`).
pub(super) fn render_window(
    panel: &mut SettingsPanel,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let fx = panel.settings.effects;
    let materials: Vec<WindowMaterial> = WindowMaterial::ALL
        .iter()
        .copied()
        .filter(|m| m.available())
        .collect();
    let shown = fx.material();
    let picker = choice(
        "window-material",
        &materials,
        shown,
        WindowMaterial::title,
        |this, m, cx| this.set_window_material(m, cx),
        cx,
    );
    let opacities = h_flex()
        .gap_1()
        .flex_wrap()
        .children(WINDOW_OPACITIES.iter().enumerate().map(|(i, &value)| {
            let selected = (fx.window_opacity - value).abs() < 0.01;
            Button::new(SharedString::from(format!("window-opacity-{i}")))
                .small()
                .map(|b| if selected { b.primary() } else { b.outline() })
                .selected(selected)
                .label(t!(
                    "settings.window.opacity_value",
                    percent = (value * 100.).round()
                ))
                .on_click(cx.listener(move |this, _, _, cx| this.set_window_opacity(value, cx)))
        }));
    let used = crate::theme::window_alpha();
    let raised = shown != WindowMaterial::Off && used > fx.window_opacity + 0.005;
    let platform_note = if cfg!(target_os = "macos") {
        t!("settings.window.note_macos")
    } else if cfg!(windows) {
        t!("settings.window.note_windows")
    } else {
        t!("settings.window.note_linux")
    };

    // ---- app icon
    let current = panel.settings.appearance.app_icon;
    let (border, ring) = (cx.theme().border, cx.theme().ring);
    let has_custom = crate::app_icon::custom_path().is_some_and(|p| p.is_file());
    let icons = h_flex().flex_wrap().gap_2().children(
        AppIconChoice::ALL
            .iter()
            .copied()
            .filter(|c| *c != AppIconChoice::Custom || has_custom)
            .map(|choice| {
                let selected = current == choice;
                let image = std::sync::Arc::new(gpui::Image::from_bytes(
                    gpui::ImageFormat::Png,
                    crate::app_icon::png(choice).into_owned(),
                ));
                v_flex()
                    .id(SharedString::from(format!("app-icon-{}", choice.key())))
                    .w(px(76.))
                    .p_1()
                    .gap_1()
                    .items_center()
                    .rounded(crate::theme::radius(9.))
                    .border_2()
                    .border_color(if selected { ring } else { border })
                    .cursor_pointer()
                    .child(gpui::img(image).size(px(52.)))
                    .child(
                        div()
                            .text_xs()
                            .truncate()
                            .when(selected, |this| this.font_weight(FontWeight::SEMIBOLD))
                            .child(choice.title()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.set_app_icon(choice, cx)))
            }),
    );
    let choose = Button::new("app-icon-choose")
        .outline()
        .small()
        .icon(crate::icons::Lucide::ImageUp)
        .label(t!("settings.app_icon.choose"))
        .disabled(crate::app_icon::custom_path().is_none())
        .on_click(cx.listener(|this, _, window, cx| this.pick_app_icon(window, cx)));

    v_flex()
        .gap_2()
        .child(row_at(
            "appearance.window.material",
            t!("settings.window.material"),
            Some(t!("settings.window.material_note")),
            picker,
            cx,
        ))
        .when(shown != WindowMaterial::Off, |this| {
            this.child(row_at(
                "appearance.window.opacity",
                t!("settings.window.opacity"),
                None,
                opacities,
                cx,
            ))
            .when(raised, |this| {
                this.child(note(
                    t!(
                        "settings.window.opacity_raised",
                        percent = (used * 100.).round()
                    ),
                    cx,
                ))
            })
        })
        .when(crate::effects::os_reduce_transparency(), |this| {
            this.child(note(t!("settings.window.reduced_by_os"), cx))
        })
        .child(note(platform_note, cx))
        .child(note(t!("settings.window.cpu_note"), cx))
        .child(
            v_flex()
                .gap_2()
                .pt_2()
                .anchor("appearance.window.app_icon")
                .child(div().text_sm().child(t!("settings.app_icon")))
                .child(note(t!("settings.app_icon_note"), cx))
                .child(icons)
                .child(h_flex().child(choose))
                .when(
                    cfg!(any(target_os = "linux", target_os = "freebsd")),
                    |this| this.child(note(t!("settings.app_icon.linux_note"), cx)),
                ),
        )
        .into_any_element()
}
