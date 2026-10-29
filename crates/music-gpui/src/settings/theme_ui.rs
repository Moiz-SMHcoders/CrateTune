//! Appearance → "Customize theme": presets, accent (swatches, hex field, hue /
//! saturation sliders), background darkness and tint, contrast, corners,
//! density, font family, dynamic colours, a live preview and "Reset to
//! default". Every change is applied at once (`SettingsEvent::Appearance` →
//! `MusicApp::apply_theme`); the widgets' entities live in [`ThemeUi`], a field
//! of the settings panel, and follow the settings when they change elsewhere
//! (presets, imports, scripts).

use gpui::{
    AnyElement, AppContext as _, Context, Entity, Focusable as _, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Rgba, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, hsla,
    linear_color_stop, linear_gradient, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IndexPath, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState},
    slider::{SliderEvent, SliderState},
    switch::Switch,
    v_flex,
};

use super::SettingsPanel;
use super::panel::SettingsEvent;
use super::portable::{PRESETS, ThemePreset};
use super::prefs_ui::{choice, slider_row_at};
use super::theme_prefs::{
    ACCENT_PRESETS, Contrast, Corners, CustomTheme, Darkness, Density, DynamicStyle, parse_hex,
    to_hex,
};
use super::widgets::{group_at, note, row_at};
use crate::icons::Lucide;
use crate::theme::{self, AppPalette, Look, ThemeChoice};

#[derive(Clone)]
pub struct FontChoice {
    /// Empty = the system UI font.
    family: SharedString,
    title: SharedString,
}

impl SelectItem for FontChoice {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.family
    }

    fn matches(&self, query: &str) -> bool {
        self.title.to_lowercase().contains(&query.to_lowercase())
    }
}

type FontPicker = Entity<SelectState<SearchableVec<FontChoice>>>;

/// Entities of the Customize theme group.
pub struct ThemeUi {
    hex: Entity<InputState>,
    hue: Entity<SliderState>,
    saturation: Entity<SliderState>,
    tint: Entity<SliderState>,
    strength: Entity<SliderState>,
    /// Built on first show (listing system fonts touches the font system).
    fonts: Option<(FontPicker, Subscription)>,
    /// The hex field holds text that isn't a colour.
    hex_invalid: bool,
    /// The import shown in the preview dialog (`portable_ui.rs`).
    pending_import: Option<super::portable::ImportPlan>,
}

fn slider(
    min: f32,
    max: f32,
    step: f32,
    value: f32,
    cx: &mut Context<SettingsPanel>,
) -> Entity<SliderState> {
    cx.new(|_| {
        SliderState::new()
            .min(min)
            .max(max)
            .step(step)
            .default_value(value)
    })
}

fn slider_value(event: &SliderEvent) -> f32 {
    match event {
        SliderEvent::Change(v) | SliderEvent::Release(v) => v.start(),
    }
}

fn rgb_hsla([r, g, b]: [u8; 3]) -> Hsla {
    Rgba {
        r: r as f32 / 255.,
        g: g as f32 / 255.,
        b: b as f32 / 255.,
        a: 1.,
    }
    .into()
}

fn hsla_rgb(c: Hsla) -> [u8; 3] {
    let rgb: Rgba = c.into();
    [rgb.r, rgb.g, rgb.b].map(|v| (v.clamp(0., 1.) * 255.).round() as u8)
}

/// Hue (degrees) and saturation (%) the sliders show for an accent.
fn hue_saturation(accent: &str) -> (f32, f32) {
    match parse_hex(accent) {
        Some(rgb) => {
            let c = rgb_hsla(rgb);
            (c.h * 360., c.s * 100.)
        }
        None => (220., 70.),
    }
}

impl ThemeUi {
    pub fn new(
        custom: &CustomTheme,
        window: &mut Window,
        cx: &mut Context<SettingsPanel>,
    ) -> (Self, Vec<Subscription>) {
        let accent = custom.accent.clone();
        let hex = cx.new(|cx| InputState::new(window, cx).default_value(accent));
        crate::i18n::track_placeholder(&hex, k!("theme.custom.hex_placeholder"), window, cx);
        let (h, s) = hue_saturation(&custom.accent);
        let hue = slider(0., 359., 1., h, cx);
        let saturation = slider(0., 100., 1., s, cx);
        let tint = slider(0., 100., 1., custom.surface_tint * 100., cx);
        let strength = slider(0., 100., 1., custom.dynamic.strength * 100., cx);
        let mut subs =
            vec![
                cx.subscribe_in(&hex, window, |this, input, event: &InputEvent, _, cx| {
                    if !matches!(event, InputEvent::Change) {
                        return;
                    }
                    let text = input.read(cx).value().trim().to_owned();
                    let parsed = parse_hex(&text);
                    this.theme_ui.hex_invalid = !text.is_empty() && parsed.is_none();
                    match parsed {
                        Some(rgb) => this.update_custom_theme(|c| c.accent = to_hex(rgb), cx),
                        None if text.is_empty() => {
                            this.update_custom_theme(|c| c.accent.clear(), cx)
                        }
                        None => cx.notify(),
                    }
                }),
            ];
        for (entity, which) in [(&hue, 0u8), (&saturation, 1)] {
            subs.push(
                cx.subscribe(entity, move |this, _, event: &SliderEvent, cx| {
                    let value = slider_value(event);
                    let (mut h, mut s) = hue_saturation(&this.settings.custom_theme.accent);
                    if which == 0 {
                        h = value;
                    } else {
                        s = value;
                    }
                    // Keep the current lightness (a mid lightness for a fresh accent).
                    let l = parse_hex(&this.settings.custom_theme.accent)
                        .map_or(0.62, |rgb| rgb_hsla(rgb).l.clamp(0.35, 0.85));
                    let hex = to_hex(hsla_rgb(hsla(h / 360., s / 100., l, 1.)));
                    this.update_custom_theme(|c| c.accent = hex, cx);
                }),
            );
        }
        subs.push(cx.subscribe(&tint, |this, _, event: &SliderEvent, cx| {
            let v = slider_value(event) / 100.;
            this.update_custom_theme(|c| c.surface_tint = v, cx);
        }));
        subs.push(cx.subscribe(&strength, |this, _, event: &SliderEvent, cx| {
            let v = slider_value(event) / 100.;
            this.update_custom_theme(|c| c.dynamic.strength = v, cx);
        }));
        (
            Self {
                hex,
                hue,
                saturation,
                tint,
                strength,
                fonts: None,
                hex_invalid: false,
                pending_import: None,
            },
            subs,
        )
    }

    /// Moves the widgets to the saved values (after presets, imports, scripts).
    fn sync(&mut self, custom: &CustomTheme, window: &mut Window, cx: &mut Context<SettingsPanel>) {
        let focused = self.hex.read(cx).focus_handle(cx).is_focused(window);
        let typed = self.hex.read(cx).value().trim().to_owned();
        if !focused && parse_hex(&typed).map(to_hex).unwrap_or_default() != custom.accent {
            let accent = custom.accent.clone();
            self.hex
                .update(cx, |input, cx| input.set_value(accent, window, cx));
            self.hex_invalid = false;
        }
        let (h, s) = hue_saturation(&custom.accent);
        for (entity, value, tolerance) in [
            (&self.hue, h, 1.5),
            (&self.saturation, s, 1.5),
            (&self.tint, custom.surface_tint * 100., 0.5),
            (&self.strength, custom.dynamic.strength * 100., 0.5),
        ] {
            if (entity.read(cx).value().start() - value).abs() > tolerance {
                entity.update(cx, |state, cx| state.set_value(value, window, cx));
            }
        }
        if let Some((picker, _)) = &self.fonts {
            let family = SharedString::from(custom.font_family.clone());
            picker.update(cx, |state, cx| {
                if state.selected_value() != Some(&family) {
                    state.set_selected_value(&family, window, cx);
                }
            });
        }
    }

    fn ensure_fonts(
        &mut self,
        current: &str,
        window: &mut Window,
        cx: &mut Context<SettingsPanel>,
    ) {
        if self.fonts.is_some() {
            return;
        }
        let mut names = cx.text_system().all_font_names();
        names.retain(|n| !n.starts_with('.'));
        names.sort_by_key(|n| n.to_lowercase());
        names.dedup();
        let mut items = vec![FontChoice {
            family: SharedString::default(),
            title: t!("theme.custom.font_system").into(),
        }];
        items.extend(names.into_iter().map(|n| FontChoice {
            family: n.clone().into(),
            title: n.into(),
        }));
        let selected = items
            .iter()
            .position(|f| f.family.as_ref() == current)
            .map(IndexPath::new);
        let picker = cx.new(|cx| {
            SelectState::new(SearchableVec::new(items), selected, window, cx).searchable(true)
        });
        let sub = cx.subscribe(
            &picker,
            |this, _, event: &SelectEvent<SearchableVec<FontChoice>>, cx| {
                if let SelectEvent::Confirm(Some(family)) = event {
                    let family = family.to_string();
                    this.update_custom_theme(|c| c.font_family = family, cx);
                }
            },
        );
        self.fonts = Some((picker, sub));
    }
}

impl SettingsPanel {
    /// Sets the import waiting for confirmation, returning the previous one.
    pub(super) fn replace_pending_import(
        &mut self,
        plan: Option<super::portable::ImportPlan>,
    ) -> Option<super::portable::ImportPlan> {
        std::mem::replace(&mut self.theme_ui.pending_import, plan)
    }

    /// Changes `settings.custom_theme`, applies it live and saves (debounced).
    pub fn update_custom_theme(
        &mut self,
        f: impl FnOnce(&mut CustomTheme),
        cx: &mut Context<Self>,
    ) {
        let before = self.settings.custom_theme.clone();
        f(&mut self.settings.custom_theme);
        self.settings.custom_theme.normalize();
        if self.settings.custom_theme != before {
            cx.emit(SettingsEvent::Appearance);
            self.changed(false, cx);
        }
    }

    /// Back to the stock look (the theme choice stays).
    pub fn reset_custom_theme(&mut self, cx: &mut Context<Self>) {
        self.update_custom_theme(|c| *c = CustomTheme::default(), cx);
    }

    pub fn apply_theme_preset(&mut self, preset: &ThemePreset, cx: &mut Context<Self>) {
        let custom = preset.custom(&self.settings.custom_theme);
        self.set_theme(preset.choice, cx);
        self.update_custom_theme(|c| *c = custom, cx);
    }

    /// Script hook: `field:value` for one Customize-theme option.
    pub fn set_custom_theme_field(
        &mut self,
        field: &str,
        value: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let unit = || {
            value
                .parse::<f32>()
                .ok()
                .map(|v| if v > 1. { v / 100. } else { v })
        };
        match field {
            "accent" => {
                let hex = if value == "none" {
                    Some(String::new())
                } else {
                    parse_hex(value).map(to_hex)
                };
                hex.map(|hex| self.update_custom_theme(|c| c.accent = hex, cx))
                    .is_some()
            }
            "tint" => unit()
                .map(|v| self.update_custom_theme(|c| c.surface_tint = v, cx))
                .is_some(),
            "darkness" => Darkness::parse(value)
                .map(|v| self.update_custom_theme(|c| c.darkness = v, cx))
                .is_some(),
            "contrast" => Contrast::parse(value)
                .map(|v| self.update_custom_theme(|c| c.contrast = v, cx))
                .is_some(),
            "corners" => Corners::parse(value)
                .map(|v| self.update_custom_theme(|c| c.corners = v, cx))
                .is_some(),
            "density" => Density::parse(value)
                .map(|v| self.update_custom_theme(|c| c.density = v, cx))
                .is_some(),
            "font" => {
                let family = if value == "system" {
                    String::new()
                } else {
                    value.to_owned()
                };
                self.update_custom_theme(|c| c.font_family = family, cx);
                true
            }
            "style" => DynamicStyle::parse(value)
                .map(|v| self.update_custom_theme(|c| c.dynamic.style = v, cx))
                .is_some(),
            "strength" => unit()
                .map(|v| self.update_custom_theme(|c| c.dynamic.strength = v, cx))
                .is_some(),
            "transitions" => {
                let on = value != "off";
                self.update_custom_theme(|c| c.dynamic.transitions = on, cx);
                true
            }
            _ => false,
        }
    }
}

/// A small card: background, a raised bar and the accent dot.
fn swatch(bg: Hsla, surface: Hsla, dot: Hsla, h: f32) -> gpui::Div {
    h_flex()
        .h(px(h))
        .w_full()
        .rounded(theme::radius(7.))
        .bg(bg)
        .p_2()
        .gap_2()
        .items_end()
        .child(
            div()
                .h(px(10.))
                .flex_1()
                .rounded(theme::radius(3.))
                .bg(surface),
        )
        .child(div().size(px(12.)).rounded_full().bg(dot))
}

fn look_of(choice: ThemeChoice, custom: CustomTheme) -> Look {
    Look {
        choice,
        font_scale: 1.0,
        custom,
        art: None,
        art_tint: 0.,
    }
}

/// The live preview: a few elements drawn with the applied tokens.
fn preview(cx: &Context<SettingsPanel>) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let palette = AppPalette::get(cx);
    let row = |selected: bool, title: &'static str, meta: &'static str| {
        h_flex()
            .px(theme::space(10.))
            .py(theme::space(6.))
            .gap_2()
            .rounded(theme::radius(6.))
            .when(selected, |this| {
                this.bg(theme.list_active)
                    .border_1()
                    .border_color(theme.list_active_border)
            })
            .child(
                div()
                    .size(px(22.))
                    .rounded(theme::radius(4.))
                    .bg(theme.muted),
            )
            .child(
                v_flex()
                    .child(div().text_sm().text_color(theme.foreground).child(title))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(meta),
                    ),
            )
    };
    v_flex()
        .p(theme::space(12.))
        .gap(theme::space(8.))
        .rounded(theme::radius(10.))
        .bg(theme.background)
        .border_1()
        .border_color(theme.border)
        .child(
            h_flex()
                .justify_between()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(t!("theme.custom.preview_title")),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .child(div().size(px(10.)).rounded_full().bg(palette.accent))
                        .child(div().size(px(10.)).rounded_full().bg(palette.secondary))
                        .child(div().size(px(10.)).rounded_full().bg(palette.tertiary)),
                ),
        )
        .child(row(
            false,
            t!("theme.custom.preview_song"),
            t!("theme.custom.preview_artist"),
        ))
        .child(row(
            true,
            t!("theme.custom.preview_playing"),
            t!("theme.custom.preview_artist"),
        ))
        .child(
            div()
                .h(px(4.))
                .w_full()
                .rounded_full()
                .bg(theme.secondary)
                .child(
                    div()
                        .h_full()
                        .w(gpui::relative(0.4))
                        .rounded_full()
                        .bg(theme.slider_bar),
                ),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("theme-preview-primary")
                        .primary()
                        .small()
                        .label(t!("common.play")),
                )
                .child(
                    Button::new("theme-preview-outline")
                        .outline()
                        .small()
                        .label(t!("common.shuffle")),
                )
                .child(
                    div()
                        .px_2()
                        .py_0p5()
                        .rounded(theme::radius(4.))
                        .bg(theme.selection)
                        .text_xs()
                        .child(t!("theme.custom.preview_selection")),
                ),
        )
}

/// The Customize theme group (rendered by `appearance.rs`).
pub(super) fn render(
    panel: &mut SettingsPanel,
    window: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let custom = panel.settings.custom_theme.clone();
    let current_choice = panel.settings.appearance.theme;
    panel.theme_ui.ensure_fonts(&custom.font_family, window, cx);
    panel.theme_ui.sync(&custom, window, cx);
    let theme = cx.theme();
    let (border, ring, muted, danger) = (
        theme.border,
        theme.ring,
        theme.muted_foreground,
        theme.danger,
    );

    // ---- presets
    let presets = h_flex()
        .flex_wrap()
        .gap_2()
        .children(PRESETS.iter().map(|preset| {
            let selected = preset.matches(&panel.settings);
            let (bg, surface, dot) =
                theme::preview(&look_of(preset.choice, preset.custom(&custom)));
            v_flex()
                .id(SharedString::from(format!("theme-preset-{}", preset.id)))
                .w(px(96.))
                .p_1p5()
                .gap_1p5()
                .rounded(theme::radius(9.))
                .border_2()
                .border_color(if selected { ring } else { border })
                .cursor_pointer()
                .child(swatch(bg, surface, dot, 36.))
                .child(
                    div()
                        .text_xs()
                        .truncate()
                        .when(selected, |this| this.font_weight(FontWeight::SEMIBOLD))
                        .child(crate::i18n::text(preset.title)),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.apply_theme_preset(preset, cx)))
        }));
    let file_buttons = h_flex()
        .gap_1()
        .flex_wrap()
        .child(
            Button::new("theme-reset")
                .outline()
                .small()
                .icon(Lucide::RotateCcw)
                .label(t!("theme.custom.reset"))
                .disabled(custom.is_default())
                .on_click(cx.listener(|this, _, _, cx| this.reset_custom_theme(cx))),
        )
        .child(
            Button::new("theme-export")
                .outline()
                .small()
                .icon(Lucide::Download)
                .label(t!("theme.file.export"))
                .on_click(cx.listener(|this, _, window, cx| this.export_theme_file(window, cx))),
        )
        .child(
            Button::new("theme-import")
                .outline()
                .small()
                .icon(Lucide::Upload)
                .label(t!("theme.file.import"))
                .on_click(cx.listener(|this, _, window, cx| this.import_theme_file(window, cx))),
        );

    // ---- accent
    let current_accent = custom.accent.clone();
    let swatches =
        h_flex()
            .flex_wrap()
            .gap_1p5()
            .child(
                div()
                    .id("accent-none")
                    .size(px(26.))
                    .rounded_full()
                    .border_2()
                    .border_color(if current_accent.is_empty() {
                        ring
                    } else {
                        border
                    })
                    .bg(gpui::linear_gradient(
                        135.,
                        linear_color_stop(hsla(0., 0., 0.98, 1.), 0.5),
                        linear_color_stop(hsla(0., 0., 0.2, 1.), 0.5),
                    ))
                    .cursor_pointer()
                    .tooltip(|window, cx| {
                        gpui_component::tooltip::Tooltip::new(t!("theme.custom.accent_default"))
                            .build(window, cx)
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.update_custom_theme(|c| c.accent.clear(), cx)
                    })),
            )
            .children(ACCENT_PRESETS.iter().map(|hex| {
                let selected = current_accent.eq_ignore_ascii_case(hex);
                let color = parse_hex(hex).map(rgb_hsla).unwrap_or_default();
                div()
                    .id(SharedString::from(format!("accent-{hex}")))
                    .size(px(26.))
                    .rounded_full()
                    .border_2()
                    .border_color(if selected {
                        ring
                    } else {
                        gpui::transparent_black()
                    })
                    .p(px(2.))
                    .cursor_pointer()
                    .child(div().size_full().rounded_full().bg(color))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.update_custom_theme(|c| c.accent = (*hex).to_owned(), cx)
                    }))
            }));
    let applied_accent = AppPalette::get(cx).accent;
    let hex_row = h_flex()
        .gap_2()
        .child(
            div()
                .size(px(22.))
                .rounded(theme::radius(5.))
                .bg(applied_accent)
                .border_1()
                .border_color(border),
        )
        .child(
            div()
                .w(px(130.))
                .child(Input::new(&panel.theme_ui.hex).small()),
        )
        .when(panel.theme_ui.hex_invalid, |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(danger)
                    .child(t!("theme.custom.hex_invalid")),
            )
        });
    let hue_strip = h_flex()
        .h(px(6.))
        .w_full()
        .rounded_full()
        .overflow_hidden()
        .children((0..12).map(|i| {
            let (a, b) = (i as f32 / 12., (i + 1) as f32 / 12.);
            div().flex_1().h_full().bg(linear_gradient(
                90.,
                linear_color_stop(hsla(a, 0.85, 0.6, 1.), 0.),
                linear_color_stop(hsla(b, 0.85, 0.6, 1.), 1.),
            ))
        }));
    let (hue_now, sat_now) = hue_saturation(&custom.accent);
    let accent_body = v_flex()
        .gap_2()
        .child(swatches)
        .child(hex_row)
        .child(
            v_flex()
                .gap_1()
                .child(slider_row_at(
                    "appearance.accent.hue",
                    t!("theme.custom.hue"),
                    format!("{}°", hue_now.round()),
                    &panel.theme_ui.hue,
                    cx,
                ))
                .child(
                    h_flex()
                        .child(div().w(px(150.)).flex_none())
                        .child(div().flex_1().child(hue_strip))
                        .child(div().w(px(76.)).flex_none()),
                ),
        )
        .child(slider_row_at(
            "appearance.accent.saturation",
            t!("theme.custom.saturation"),
            format!("{} %", sat_now.round()),
            &panel.theme_ui.saturation,
            cx,
        ))
        .child(note(t!("theme.custom.accent_note"), cx));

    // ---- surfaces, shape, type
    let darkness = choice(
        "theme-darkness",
        Darkness::ALL,
        custom.darkness,
        Darkness::title,
        |this, v, cx| this.update_custom_theme(|c| c.darkness = v, cx),
        cx,
    );
    let contrast = choice(
        "theme-contrast",
        Contrast::ALL,
        custom.contrast,
        Contrast::title,
        |this, v, cx| this.update_custom_theme(|c| c.contrast = v, cx),
        cx,
    );
    let corners = choice(
        "theme-corners",
        Corners::ALL,
        custom.corners,
        Corners::title,
        |this, v, cx| this.update_custom_theme(|c| c.corners = v, cx),
        cx,
    );
    let density = choice(
        "theme-density",
        Density::ALL,
        custom.density,
        Density::title,
        |this, v, cx| this.update_custom_theme(|c| c.density = v, cx),
        cx,
    );
    let font = panel.theme_ui.fonts.as_ref().map(|(picker, _)| {
        div().w(px(240.)).child(
            Select::new(picker)
                .small()
                .search_placeholder(t!("common.search_placeholder"))
                .menu_max_h(px(320.)),
        )
    });

    // ---- dynamic colours
    let dynamic = &custom.dynamic;
    let styles = choice(
        "theme-dynamic-style",
        &DynamicStyle::ALL,
        dynamic.style,
        DynamicStyle::title,
        |this, v, cx| this.update_custom_theme(|c| c.dynamic.style = v, cx),
        cx,
    );
    let transitions = Switch::new("theme-transitions")
        .checked(dynamic.transitions)
        .on_click(cx.listener(|this, checked: &bool, _, cx| {
            let on = *checked;
            this.update_custom_theme(|c| c.dynamic.transitions = on, cx);
        }));
    let dynamic_active = current_choice == ThemeChoice::Dynamic;
    let use_dynamic = (!dynamic_active).then(|| {
        Button::new("theme-use-dynamic")
            .outline()
            .small()
            .label(t!("theme.custom.use_dynamic"))
            .on_click(cx.listener(|this, _, _, cx| this.set_theme(ThemeChoice::Dynamic, cx)))
    });

    v_flex()
        .gap_5()
        .child(
            group_at("appearance.custom_theme", t!("theme.custom.group"), cx)
                .child(note(t!("theme.custom.intro"), cx))
                .child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child(t!("theme.custom.presets")),
                )
                .child(presets)
                .child(file_buttons)
                .child(preview(cx)),
        )
        .child(group_at("appearance.accent", t!("theme.custom.accent"), cx).child(accent_body))
        .child(
            group_at("appearance.surfaces", t!("theme.custom.surfaces"), cx)
                .child(row_at(
                    "appearance.surfaces.darkness",
                    t!("theme.custom.darkness"),
                    Some(t!("theme.custom.darkness_detail")),
                    darkness,
                    cx,
                ))
                .child(slider_row_at(
                    "appearance.surfaces.tint",
                    t!("theme.custom.tint"),
                    format!("{} %", (custom.surface_tint * 100.).round()),
                    &panel.theme_ui.tint,
                    cx,
                ))
                .child(note(t!("theme.custom.tint_note"), cx))
                .child(row_at(
                    "appearance.surfaces.contrast",
                    t!("theme.custom.contrast"),
                    Some(t!("theme.custom.contrast_detail")),
                    contrast,
                    cx,
                ))
                .child(row_at(
                    "appearance.surfaces.corners",
                    t!("theme.custom.corners"),
                    None,
                    corners,
                    cx,
                ))
                .child(row_at(
                    "appearance.surfaces.density",
                    t!("theme.custom.density"),
                    Some(t!("theme.custom.density_detail")),
                    density,
                    cx,
                ))
                .children(font.map(|font| {
                    row_at(
                        "appearance.surfaces.font",
                        t!("theme.custom.font"),
                        Some(t!("theme.custom.font_detail")),
                        font,
                        cx,
                    )
                })),
        )
        .child(
            group_at("appearance.dynamic", t!("theme.dynamic.group"), cx)
                .child(note(
                    if dynamic_active {
                        t!("theme.dynamic.active_note")
                    } else {
                        t!("theme.dynamic.inactive_note")
                    },
                    cx,
                ))
                .children(use_dynamic.map(|b| h_flex().child(b)))
                .child(row_at(
                    "appearance.dynamic.style",
                    t!("theme.dynamic.style"),
                    None,
                    styles,
                    cx,
                ))
                .child(slider_row_at(
                    "appearance.dynamic.strength",
                    t!("theme.dynamic.strength"),
                    format!("{} %", (dynamic.strength * 100.).round()),
                    &panel.theme_ui.strength,
                    cx,
                ))
                .child(row_at(
                    "appearance.dynamic.transitions",
                    t!("theme.dynamic.transitions"),
                    Some(t!("theme.dynamic.transitions_detail")),
                    transitions,
                    cx,
                )),
        )
        .into_any_element()
}
