//! Building blocks shared by the settings sections.

use gpui::{
    App, FontWeight, Hsla, IntoElement, ParentElement as _, SharedString, Styled as _, div, px,
};
use gpui_component::{ActiveTheme as _, h_flex, input::Input, v_flex};

pub use super::reveal::Anchor;

/// A titled group inside a section.
pub fn group(title: impl Into<SharedString>, cx: &App) -> gpui::Div {
    v_flex()
        .gap(crate::theme::space(8.))
        .p(crate::theme::space(16.))
        .rounded(crate::theme::radius(10.))
        .border_1()
        .border_color(cx.theme().border)
        .child(section_title(title))
}

/// [`group`] with a scroll anchor (an `anchor` of the search index).
pub fn group_at(id: &'static str, title: impl Into<SharedString>, cx: &App) -> gpui::Div {
    group(title, cx).anchor(id)
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
pub fn field(label: impl Into<SharedString>, input: Input, cx: &App) -> gpui::Div {
    h_flex()
        .gap_3()
        .child(
            div()
                .w(px(96.))
                .flex_none()
                // Long translations wrap onto a second line.
                .line_clamp(2)
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(label.into()),
        )
        .child(div().flex_1().min_w_0().child(input))
}

/// A label + description on the left, a control on the right. The text column
/// takes the free width and wraps; the control keeps its size.
pub fn row(
    label: impl Into<SharedString>,
    description: Option<&'static str>,
    control: impl IntoElement,
    cx: &App,
) -> gpui::Div {
    h_flex()
        .gap_4()
        .justify_between()
        .child(
            v_flex()
                .flex_1()
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

/// [`row`] with a scroll anchor (an `anchor` of the search index).
pub fn row_at(
    id: &'static str,
    label: impl Into<SharedString>,
    description: Option<&'static str>,
    control: impl IntoElement,
    cx: &App,
) -> gpui::Div {
    row(label, description, control, cx).anchor(id)
}

/// [`field`] with a scroll anchor.
pub fn field_at(
    id: &'static str,
    label: impl Into<SharedString>,
    input: Input,
    cx: &App,
) -> gpui::Div {
    field(label, input, cx).anchor(id)
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
