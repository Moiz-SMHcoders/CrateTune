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
    window: &mut Window,
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
    let themes = h_flex()
        .flex_wrap()
        .gap_3()
        .children(ThemeChoice::ALL.map(|choice| {
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

    let scales = h_flex()
        .flex_wrap()
        .gap_1()
        .children(FONT_SCALES.map(|value| {
            let selected = (value - scale).abs() < 0.01;
            Button::new(SharedString::from(format!("font-scale-{value}")))
                .small()
                .map(|b| if selected { b.primary() } else { b.outline() })
                .selected(selected)
                .label(t!(
                    "settings.appearance.font_scale",
                    percent = (value * 100.).round()
                ))
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
            // Art across the top, fading out; type and controls at the bottom.
            NowPlayingLayout::Immersive => frame.p_0().overflow_hidden().child(
                v_flex()
                    .size_full()
                    .child(div().h(px(36.)).w_full().bg(gpui::linear_gradient(
                        180.,
                        gpui::linear_color_stop(hsla(0.08, 0.5, 0.6, 0.9), 0.),
                        gpui::linear_color_stop(hsla(0.62, 0.35, 0.22, 1.), 1.),
                    )))
                    .child(
                        v_flex()
                            .px_2()
                            .gap(px(3.))
                            .child(block(60., 5., 0.85))
                            .child(block(40., 3., 0.5))
                            .child(block(110., 2., 0.6)),
                    ),
            ),
            NowPlayingLayout::Card => frame.child(
                h_flex().size_full().justify_center().child(
                    v_flex()
                        .h_full()
                        .w(px(44.))
                        .p(px(3.))
                        .gap(px(3.))
                        .rounded(px(5.))
                        .bg(hsla(0., 0., 1., 0.12))
                        .child(block(38., 30., 0.8))
                        .child(block(30., 3., 0.55))
                        .child(block(38., 2., 0.4)),
                ),
            ),
            NowPlayingLayout::Minimal => frame.p_0().overflow_hidden().child(
                div()
                    .relative()
                    .size_full()
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .h_full()
                            .w(px(50.))
                            .bg(hsla(0., 0., 1., 0.14)),
                    )
                    .child(
                        h_flex()
                            .absolute()
                            .bottom(px(8.))
                            .left(px(8.))
                            .right(px(8.))
                            .items_end()
                            .justify_between()
                            .child(
                                v_flex()
                                    .gap(px(3.))
                                    .child(block(64., 9., 0.85))
                                    .child(block(36., 3., 0.5)),
                            )
                            .child(block(26., 12., 0.7)),
                    ),
            ),
            NowPlayingLayout::Editorial => frame.bg(hsla(0.62, 0.3, 0.12, 1.)).child(
                h_flex()
                    .size_full()
                    .gap_2()
                    .child(block(40., 40., 0.8).rounded_full())
                    .child(
                        v_flex()
                            .gap(px(3.))
                            .child(block(54., 6., 0.85))
                            .child(
                                h_flex()
                                    .gap(px(3.))
                                    .child(
                                        block(40., 10., 0.9)
                                            .rounded_full()
                                            .bg(hsla(0.1, 0.6, 0.74, 1.)),
                                    )
                                    .child(block(10., 10., 0.5).rounded_full()),
                            )
                            .child(block(54., 2., 0.5)),
                    ),
            ),
            NowPlayingLayout::Queue => frame.child(
                h_flex()
                    .size_full()
                    .gap_2()
                    .child(
                        v_flex()
                            .gap(px(3.))
                            .child(block(32., 32., 0.8))
                            .child(block(32., 3., 0.5)),
                    )
                    .child(v_flex().gap(px(4.)).children((0..4).map(|_| {
                        h_flex()
                            .gap(px(3.))
                            .child(block(7., 7., 0.6))
                            .child(block(44., 3., 0.45))
                    }))),
            ),
        }
    };
    let layouts = h_flex()
        .flex_wrap()
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

    let language = super::language::language_row(panel, window, cx);
    let player_bar = super::bar_ui::render(panel, cx);
    v_flex()
        .gap_5()
        .child(group(t!("settings.language.group"), cx).child(language))
        .child(
            group(t!("settings.appearance.theme"), cx)
                .child(themes)
                .child(note(t!("settings.appearance.theme_note"), cx)),
        )
        .child(
            group(t!("settings.appearance.text_size"), cx)
                .child(scales)
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(t!("settings.appearance.text_size_note")),
                ),
        )
        .child(
            group(t!("common.now_playing"), cx)
                .child(layouts)
                .child(row(
                    t!("settings.appearance.background"),
                    None,
                    backgrounds,
                    cx,
                ))
                .child(note(t!("settings.appearance.np_note"), cx)),
        )
        .child(group(t!("settings.appearance.mini_player"), cx).child(row(
            t!("settings.appearance.background"),
            None,
            mini,
            cx,
        )))
        .child(player_bar)
        .into_any_element()
}
