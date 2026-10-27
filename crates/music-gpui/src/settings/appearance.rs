//! Appearance section: theme (dark / pure black / dynamic accent) and font size.

use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, hsla,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

use super::SettingsPanel;
use super::widgets::{group, note};
use crate::theme::{AppColors, FONT_SCALES, ThemeChoice};

pub(super) fn render(
    panel: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let current = panel.settings.appearance.theme;
    let scale = panel.settings.appearance.font_scale;
    let theme = cx.theme();
    let (border, ring, muted) = (theme.border, theme.ring, theme.muted_foreground);
    let accent = AppColors::get(cx).accent;

    let swatch = |choice: ThemeChoice| {
        // Background / surface / accent preview of each theme.
        let (bg, surface, dot) = match choice {
            ThemeChoice::Dark => (
                hsla(0., 0., 0.04, 1.),
                hsla(0., 0., 0.15, 1.),
                hsla(0., 0., 0.98, 1.),
            ),
            ThemeChoice::Black => (
                hsla(0., 0., 0., 1.),
                hsla(0., 0., 0.06, 1.),
                hsla(0., 0., 0.98, 1.),
            ),
            ThemeChoice::Dynamic => (
                hsla(0., 0., 0.04, 1.),
                hsla(0., 0., 0.15, 1.),
                if current == ThemeChoice::Dynamic {
                    accent
                } else {
                    hsla(0.93, 0.7, 0.62, 1.)
                },
            ),
        };
        h_flex()
            .h(px(56.))
            .w_full()
            .rounded(px(8.))
            .bg(bg)
            .p_2()
            .gap_2()
            .items_end()
            .child(div().h(px(12.)).flex_1().rounded(px(3.)).bg(surface))
            .child(div().size(px(14.)).rounded_full().bg(dot))
    };
    let themes = h_flex().gap_3().children(ThemeChoice::ALL.map(|choice| {
        let selected = current == choice;
        v_flex()
            .id(SharedString::from(format!("theme-{choice:?}")))
            .w(px(150.))
            .p_2()
            .gap_2()
            .rounded(px(10.))
            .border_2()
            .border_color(if selected { ring } else { border })
            .cursor_pointer()
            .child(swatch(choice))
            .child(
                div()
                    .text_sm()
                    .when(selected, |this| this.font_weight(FontWeight::SEMIBOLD))
                    .child(choice.title()),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.set_theme(choice, cx)))
    }));

    let scales = h_flex().gap_1().children(FONT_SCALES.map(|value| {
        let selected = (value - scale).abs() < 0.01;
        Button::new(SharedString::from(format!("font-scale-{value}")))
            .small()
            .map(|b| if selected { b.primary() } else { b.outline() })
            .selected(selected)
            .label(format!("{:.0} %", value * 100.))
            .on_click(cx.listener(move |this, _, _, cx| this.set_font_scale(value, cx)))
    }));

    v_flex()
        .gap_5()
        .child(group("Theme", cx).child(themes).child(note(
            "Dynamic takes its accent from the playing track's artwork (the most \
                     common vibrant colour, adjusted to stay readable on the dark background).",
            cx,
        )))
        .child(
            group("Text size", cx).child(scales).child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("Scales all interface text; lyrics keep their own size."),
            ),
        )
        .into_any_element()
}
