//! Appearance → Window, under the window background (`glass_prefs.rs`): presets,
//! macOS's blur strength and vibrancy material, each surface's opacity, the tint,
//! and which surfaces the readable floor raised; plus the panel methods and the
//! scripts (`@glass-preset`, `@surface-opacity`, `@glass-tint`, `@glass-blur`,
//! `@glass-vibrancy`).
//!
//! The floor shown here is computed from the theme and the settings alone (not
//! from the app, which may be the one updating the settings).

use gpui::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _,
    prelude::FluentBuilder as _,
};
use gpui_component::{
    ActiveTheme as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

use super::glass_prefs::{
    BlurStrength, GlassConfig, GlassPreset, GlassTint, SURFACE_OPACITIES, Surface, TINT_STRENGTHS,
    Vibrancy,
};
use super::look_prefs::WindowMaterial;
use super::panel::{SettingsEvent, SettingsPanel};
use super::prefs_ui::choice;
use super::widgets::{note, stack_at};
use crate::glass::{Behind, Colors, resolve};

impl SettingsPanel {
    /// Edits the surfaces' translucency; the window follows at once.
    pub fn set_glass(&mut self, edit: impl FnOnce(&mut GlassConfig), cx: &mut Context<Self>) {
        let before = self.settings.effects.glass;
        edit(&mut self.settings.effects.glass);
        self.settings.effects.glass.normalize();
        if self.settings.effects.glass != before {
            cx.emit(SettingsEvent::Appearance);
            cx.emit(SettingsEvent::NowPlaying);
            self.changed(false, cx);
            cx.notify();
        }
    }

    /// A preset: the blurred window background with its opacity and surfaces.
    pub fn apply_glass_preset(&mut self, preset: GlassPreset, cx: &mut Context<Self>) {
        let values = preset.values();
        let fx = &mut self.settings.effects;
        let before = (fx.window_material, fx.window_opacity, fx.glass);
        fx.window_material = WindowMaterial::Blur;
        fx.window_opacity = values.window_opacity;
        fx.glass = values.glass;
        fx.normalize();
        if (fx.window_material, fx.window_opacity, fx.glass) != before {
            cx.emit(SettingsEvent::Appearance);
            cx.emit(SettingsEvent::NowPlaying);
            self.changed(false, cx);
            cx.notify();
        }
    }
}

/// What a surface's opacity button says.
fn percent(value: f32) -> SharedString {
    t!(
        "settings.window.opacity_value",
        percent = (value * 100.).round()
    )
    .into()
}

/// The rows under the window background (`look_ui::render_window`). `shown` is
/// the material drawn; `sidebar_only`: the translucent sidebar is on instead.
pub(super) fn render(
    panel: &mut SettingsPanel,
    shown: WindowMaterial,
    sidebar_only: bool,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let fx = panel.settings.effects;
    let glass = fx.glass;
    let presets = h_flex()
        .gap_1()
        .flex_wrap()
        .children(GlassPreset::ALL.iter().map(|&preset| {
            let selected = shown == WindowMaterial::Blur
                && GlassPreset::matching(fx.window_opacity, &glass) == Some(preset);
            Button::new(SharedString::from(format!("glass-preset-{}", preset.key())))
                .small()
                .map(|b| if selected { b.primary() } else { b.outline() })
                .selected(selected)
                .label(preset.title())
                .on_click(cx.listener(move |this, _, _, cx| this.apply_glass_preset(preset, cx)))
        }));
    let presets_available = WindowMaterial::Blur.available();
    let on = shown != WindowMaterial::Off || sidebar_only;

    // ---- per surface
    let surfaces = Surface::ALL
        .into_iter()
        // With only the translucent sidebar, the rest stays solid.
        .filter(|s| !sidebar_only || matches!(s, Surface::Sidebar | Surface::TopBar))
        .map(|surface| {
            let current = glass.opacity(surface);
            let auto = Button::new(SharedString::from(format!(
                "glass-surface-{}-auto",
                surface.key()
            )))
            .small()
            .map(|b| {
                if current.is_none() {
                    b.primary()
                } else {
                    b.outline()
                }
            })
            .selected(current.is_none())
            .label(t!("glass.auto"))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set_glass(|g| g.set_opacity(surface, None), cx)
            }));
            let levels = SURFACE_OPACITIES.iter().map(move |&value| {
                let selected = current.is_some_and(|c| (c - value).abs() < 0.005);
                Button::new(SharedString::from(format!(
                    "glass-surface-{}-{}",
                    surface.key(),
                    (value * 100.).round()
                )))
                .small()
                .map(|b| if selected { b.primary() } else { b.outline() })
                .selected(selected)
                .label(if value >= 1. {
                    SharedString::from(t!("glass.solid"))
                } else {
                    percent(value)
                })
            });
            let levels: Vec<_> = levels
                .zip(SURFACE_OPACITIES)
                .map(|(button, value)| {
                    button.on_click(cx.listener(move |this, _, _, cx| {
                        this.set_glass(|g| g.set_opacity(surface, Some(value)), cx)
                    }))
                })
                .collect();
            v_flex()
                .gap_1()
                .child(gpui::div().text_xs().child(surface.title()))
                .child(h_flex().gap_1().flex_wrap().child(auto).children(levels))
        })
        .collect::<Vec<_>>();

    // ---- what the floor did
    let theme = cx.theme();
    let colors = Colors {
        background: theme.background,
        foreground: theme.foreground,
        muted: theme.muted_foreground,
        sidebar: theme.sidebar,
        sidebar_foreground: theme.sidebar_foreground,
        secondary: theme.secondary,
        accent: crate::theme::AppColors::get(cx).accent,
    };
    let behind = if sidebar_only {
        Behind::Sidebar
    } else {
        Behind::Material {
            window_alpha: crate::theme::window_alpha(),
        }
    };
    let resolved = resolve(behind, &glass, &colors);
    let raised: Vec<String> = Surface::ALL
        .into_iter()
        .filter_map(|s| {
            let chosen = glass.opacity(s)?;
            let used = resolved.surface(s).a;
            (used > chosen + 0.005).then(|| {
                t!(
                    "glass.raised_item",
                    surface = s.title(),
                    percent = (used * 100.).round()
                )
            })
        })
        .collect();

    let tints = choice(
        "glass-tint",
        GlassTint::ALL,
        glass.tint,
        GlassTint::title,
        |this, tint, cx| this.set_glass(|g| g.tint = tint, cx),
        cx,
    );
    let strengths = h_flex()
        .gap_1()
        .flex_wrap()
        .children(TINT_STRENGTHS.iter().map(|&value| {
            let selected = (glass.tint_strength - value).abs() < 0.005;
            Button::new(SharedString::from(format!(
                "glass-tint-strength-{}",
                (value * 100.).round()
            )))
            .small()
            .map(|b| if selected { b.primary() } else { b.outline() })
            .selected(selected)
            .label(percent(value))
            .on_click(
                cx.listener(move |this, _, _, cx| this.set_glass(|g| g.tint_strength = value, cx)),
            )
        }));
    let blur = choice(
        "glass-blur",
        BlurStrength::ALL,
        glass.blur,
        BlurStrength::title,
        |this, b, cx| this.set_glass(|g| g.blur = b, cx),
        cx,
    );
    let vibrancy = choice(
        "glass-vibrancy",
        Vibrancy::ALL,
        glass.vibrancy,
        Vibrancy::title,
        |this, v, cx| this.set_glass(|g| g.vibrancy = v, cx),
        cx,
    );

    v_flex()
        .gap_2()
        .when(presets_available, |this| {
            this.child(stack_at(
                "appearance.window.presets",
                t!("glass.presets"),
                Some(t!("glass.presets_note")),
                presets,
                cx,
            ))
        })
        .when(
            shown == WindowMaterial::Blur && cfg!(target_os = "macos"),
            |this| {
                this.child(stack_at(
                    "appearance.window.blur",
                    t!("glass.blur"),
                    Some(t!("glass.blur_note")),
                    blur,
                    cx,
                ))
            },
        )
        .when(shown == WindowMaterial::Vibrant, |this| {
            this.child(stack_at(
                "appearance.window.vibrancy",
                t!("glass.vibrancy"),
                Some(t!("glass.vibrancy_note")),
                vibrancy,
                cx,
            ))
        })
        .when(on, |this| {
            this.child(
                v_flex()
                    .gap_2()
                    .child(stack_at(
                        "appearance.window.surfaces",
                        t!("glass.surfaces"),
                        Some(t!("glass.surfaces_note")),
                        gpui::div(),
                        cx,
                    ))
                    .children(surfaces)
                    .when(!raised.is_empty(), |this| {
                        this.child(note(t!("glass.raised", surfaces = raised.join(", ")), cx))
                    }),
            )
            .child(stack_at(
                "appearance.window.tint",
                t!("glass.tint"),
                Some(t!("glass.tint_note")),
                tints,
                cx,
            ))
            .when(glass.tint != GlassTint::Off, |this| {
                this.child(stack_at(
                    "appearance.window.tint_strength",
                    t!("glass.tint_strength"),
                    None,
                    strengths,
                    cx,
                ))
            })
        })
        .into_any_element()
}

impl crate::app::MusicApp {
    /// `@glass-preset=subtle|glass|frosted`, `@surface-opacity=sidebar:0.7|auto`
    /// (`top_bar`, `player_bar`, `queue`), `@glass-tint=accent[:0.25]|off`,
    /// `@glass-blur=system|light|medium|strong`, `@glass-vibrancy=sidebar|window|menu|popover|hud|full-screen`
    /// (saved like a click).
    pub(crate) fn glass_script_action(
        &mut self,
        name: &str,
        arg: Option<&str>,
        cx: &mut Context<crate::app::MusicApp>,
    ) -> bool {
        let arg = arg.unwrap_or_default();
        let panel = self.settings.clone();
        match name {
            "glass-preset" => match GlassPreset::parse(arg) {
                Some(p) => panel.update(cx, |s, cx| s.apply_glass_preset(p, cx)),
                None => eprintln!("music-gpui: @glass-preset wants subtle|glass|frosted"),
            },
            "surface-opacity" => {
                let (key, value) = arg.split_once(':').unwrap_or((arg, ""));
                let value = match value {
                    "auto" => Some(None),
                    v => v.parse::<f32>().ok().map(Some),
                };
                match (Surface::parse(key), value) {
                    (Some(surface), Some(value)) => panel.update(cx, |s, cx| {
                        s.set_glass(|g| g.set_opacity(surface, value), cx)
                    }),
                    _ => eprintln!(
                        "music-gpui: @surface-opacity wants sidebar|top_bar|player_bar|queue:0.3..1|auto"
                    ),
                }
            }
            "glass-tint" => {
                let (key, strength) = arg.split_once(':').unwrap_or((arg, ""));
                match GlassTint::parse(key) {
                    Some(tint) => {
                        let strength = strength.parse::<f32>().ok();
                        panel.update(cx, |s, cx| {
                            s.set_glass(
                                |g| {
                                    g.tint = tint;
                                    if let Some(k) = strength {
                                        g.tint_strength = k;
                                    }
                                },
                                cx,
                            )
                        })
                    }
                    None => eprintln!(
                        "music-gpui: @glass-tint wants off|accent|frost|smoke|ice|rose|amber[:STRENGTH]"
                    ),
                }
            }
            "glass-blur" => match BlurStrength::parse(arg) {
                Some(b) => panel.update(cx, |s, cx| s.set_glass(|g| g.blur = b, cx)),
                None => eprintln!("music-gpui: @glass-blur wants system|light|medium|strong"),
            },
            "glass-vibrancy" => match Vibrancy::parse(arg) {
                Some(v) => panel.update(cx, |s, cx| s.set_glass(|g| g.vibrancy = v, cx)),
                None => eprintln!(
                    "music-gpui: @glass-vibrancy wants sidebar|window|menu|popover|hud|full-screen"
                ),
            },
            _ => return false,
        }
        cx.notify();
        true
    }
}
