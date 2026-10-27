//! Building blocks shared by the settings sections.

use gpui::{
    App, FontWeight, Hsla, IntoElement, ParentElement as _, SharedString, Styled as _, div, px,
};
use gpui_component::{ActiveTheme as _, h_flex, input::Input, v_flex};

/// A titled group inside a section.
pub fn group(title: impl Into<SharedString>, cx: &App) -> gpui::Div {
    v_flex()
        .gap_2()
        .p_4()
        .rounded(px(10.))
        .border_1()
        .border_color(cx.theme().border)
        .child(section_title(title))
}

pub fn section_title(title: impl Into<SharedString>) -> impl IntoElement {
    div().font_weight(FontWeight::SEMIBOLD).child(title.into())
}

pub fn note(text: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .line_clamp(3)
        .child(text.into())
}

pub fn status_line(text: SharedString, color: Hsla) -> impl IntoElement {
    div().text_xs().text_color(color).line_clamp(3).child(text)
}

/// A labelled text input row.
pub fn field(label: &'static str, input: Input, cx: &App) -> impl IntoElement {
    h_flex()
        .gap_3()
        .child(
            div()
                .w(px(96.))
                .flex_none()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(div().flex_1().min_w_0().child(input))
}

/// A label + description on the left, a control on the right.
pub fn row(
    label: impl Into<SharedString>,
    description: Option<&'static str>,
    control: impl IntoElement,
    cx: &App,
) -> impl IntoElement {
    h_flex()
        .gap_4()
        .justify_between()
        .child(
            v_flex()
                .min_w_0()
                .gap_0p5()
                .child(div().text_sm().child(label.into()))
                .children(description.map(|d| {
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(d)
                })),
        )
        .child(div().flex_none().child(control))
}

pub fn band_label(hz: f64) -> String {
    if hz >= 1000. {
        format!("{}k", hz / 1000.)
    } else {
        format!("{hz}")
    }
}

#[cfg(test)]
mod tests {
    use super::band_label;

    #[test]
    fn band_labels() {
        assert_eq!(band_label(31.), "31");
        assert_eq!(band_label(1000.), "1k");
        assert_eq!(band_label(16000.), "16k");
    }
}
