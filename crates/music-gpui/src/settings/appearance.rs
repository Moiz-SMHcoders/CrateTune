//! Appearance section: theme (dark / pure black / dynamic accent), font size,
//! Now Playing layout + background, and the mini player style.

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
use super::prefs::{MiniPlayerStyle, NowPlayingBackground, NowPlayingLayout};
use super::prefs_ui::choice;
use super::widgets::{group, note, row};
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

    // ---- Now Playing: layout cards with a schematic preview
    let np = panel.settings.now_playing;
    let block = |w: f32, h: f32, alpha: f32| {
        div()
            .w(px(w))
            .h(px(h))
            .flex_none()
            .rounded(px(2.))
            .bg(hsla(0., 0., 1., alpha))
    };
    let lines = |n: usize, w: f32| {
        v_flex()
            .gap(px(3.))
            .children((0..n).map(move |i| block(w * (1. - 0.15 * (i % 3) as f32), 3., 0.55)))
    };
    let preview = |layout: NowPlayingLayout| {
        let frame = div()
            .h(px(70.))
            .w_full()
            .rounded(px(6.))
            .bg(hsla(0.62, 0.35, 0.22, 1.))
            .p_2();
        match layout {
            NowPlayingLayout::Split => frame.child(
                h_flex()
                    .size_full()
                    .gap_2()
                    .child(
                        v_flex()
                            .gap(px(3.))
                            .child(block(30., 30., 0.8))
                            .child(block(30., 3., 0.5))
                            .child(block(30., 3., 0.35)),
                    )
                    .child(lines(5, 50.)),
            ),
            NowPlayingLayout::Centered => frame.child(
                v_flex()
                    .size_full()
                    .items_center()
                    .gap(px(3.))
                    .child(block(36., 36., 0.8))
                    .child(block(44., 3., 0.5))
                    .child(block(30., 3., 0.35)),
            ),
            NowPlayingLayout::Lyrics => frame.child(
                v_flex()
                    .size_full()
                    .gap(px(4.))
                    .child(div().flex_1().child(lines(4, 90.)))
                    .child(
                        h_flex()
                            .gap(px(4.))
                            .child(block(10., 10., 0.8))
                            .child(block(60., 3., 0.5)),
                    ),
            ),
        }
    };
    let layouts = h_flex()
        .gap_3()
        .children(NowPlayingLayout::ALL.map(|layout| {
            let selected = np.layout == layout;
            v_flex()
                .id(SharedString::from(format!("np-layout-{}", layout.key())))
                .w(px(150.))
                .p_2()
                .gap_2()
                .rounded(px(10.))
                .border_2()
                .border_color(if selected { ring } else { border })
                .cursor_pointer()
                .child(preview(layout))
                .child(
                    div()
                        .text_sm()
                        .when(selected, |this| this.font_weight(FontWeight::SEMIBOLD))
                        .child(layout.title()),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_np_layout(layout, cx)))
        }));
    let backgrounds = choice(
        "np-background",
        &NowPlayingBackground::ALL,
        np.background,
        NowPlayingBackground::title,
        |this, b, cx| this.set_np_background(b, cx),
        cx,
    );
    let mini = choice(
        "mini-style",
        &MiniPlayerStyle::ALL,
        np.mini_player,
        MiniPlayerStyle::title,
        |this, m, cx| this.set_mini_style(m, cx),
        cx,
    );

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
                    .child("Scales all interface text; lyrics keep their own size (Lyrics)."),
            ),
        )
        .child(
            group("Now Playing", cx)
                .child(layouts)
                .child(row("Background", None, backgrounds, cx))
                .child(note(
                    "Gradient, solid and glow take their colours from the artwork. The glow \
                     only moves while music plays. Layout and background are also in the \
                     Now Playing screen's layout menu.",
                    cx,
                )),
        )
        .child(group("Mini player", cx).child(row("Background", None, mini, cx)))
        .into_any_element()
}
