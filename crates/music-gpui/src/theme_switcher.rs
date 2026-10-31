//! The top bar's quick theme switcher (after Deviload's swatch popover): a
//! palette button opening light / dark / system and a swatch per built-in
//! preset and theme file, each previewed in the current mode. Picking one
//! applies it like Settings → Appearance does (and saves it). The popover is
//! built only while open; the bar re-renders once per open / close.

use gpui::{
    AnyElement, App, Context, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    popover::Popover,
    v_flex,
};

use crate::app::{MusicApp, read_app, update_app};
use crate::icons::Lucide;
use crate::settings::theme_library::SwitchEntry;
use crate::settings::theme_prefs::ColorMode;

impl MusicApp {
    /// The palette button and its popover.
    pub(crate) fn theme_switcher(&self, _cx: &mut Context<Self>) -> AnyElement {
        let trigger = Button::new("titlebar-themes")
            .ghost()
            .icon(IconName::Palette)
            .selected(self.theme_switcher_open)
            .tooltip(t!("themes.switcher"));
        Popover::new("theme-switcher")
            .anchor(gpui::Anchor::TopRight)
            .open(self.theme_switcher_open)
            .on_open_change(|open, _, cx| {
                let open = *open;
                update_app(cx, |app, cx| {
                    app.theme_switcher_open = open;
                    cx.notify();
                });
            })
            .trigger(trigger)
            .content(|_, _, cx| switcher_menu(cx))
            .into_any_element()
    }

    /// `@theme-switcher[=off]`: opens or closes the popover (screenshots, checks).
    pub(crate) fn set_theme_switcher(&mut self, open: bool, cx: &mut Context<Self>) {
        self.theme_switcher_open = open;
        cx.notify();
    }
}

fn swatch(bg: Hsla, surface: Hsla, dot: Hsla, edge: Hsla) -> gpui::Div {
    h_flex()
        .h(px(30.))
        .w_full()
        .rounded(crate::theme::radius(6.))
        .bg(bg)
        .border_1()
        .border_color(edge)
        .px(px(5.))
        .gap(px(4.))
        .items_end()
        .pb(px(5.))
        .child(
            div()
                .h(px(7.))
                .flex_1()
                .rounded(crate::theme::radius(2.))
                .bg(surface),
        )
        .child(div().size(px(9.)).rounded_full().bg(dot))
}

fn switcher_menu(cx: &mut App) -> AnyElement {
    let Some((entries, mode)) = read_app(cx, |app, cx| {
        let panel = app.settings.read(cx);
        (
            panel.theme_switcher_entries(),
            panel.settings().appearance.mode,
        )
    }) else {
        return div().into_any_element();
    };
    let theme = cx.theme();
    let (border, ring, muted) = (theme.border, theme.ring, theme.muted_foreground);
    let modes = h_flex().gap_1().children(ColorMode::ALL.iter().map(|&m| {
        let icon = match m {
            ColorMode::Light => Icon::new(IconName::Sun),
            ColorMode::Dark => Icon::new(IconName::Moon),
            ColorMode::System => Icon::new(Lucide::SunMoon),
        };
        let selected = m == mode;
        Button::new(SharedString::from(format!("switcher-mode-{}", m.key())))
            .small()
            .map(|b| if selected { b.primary() } else { b.outline() })
            .selected(selected)
            .icon(icon)
            .label(m.title())
            .on_click(move |_, _, cx| {
                update_app(cx, |app, cx| {
                    app.settings.update(cx, |s, cx| s.set_color_mode(m, cx));
                });
            })
    }));
    let cards = h_flex()
        .flex_wrap()
        .gap_2()
        .children(entries.into_iter().enumerate().map(
            |(
                i,
                SwitchEntry {
                    target,
                    name,
                    preview,
                    selected,
                },
            )| {
                let (bg, surface, dot) = preview;
                v_flex()
                    .id(("switcher-theme", i))
                    .w(px(78.))
                    .p_1()
                    .gap_1()
                    .rounded(crate::theme::radius(8.))
                    .border_2()
                    .border_color(if selected { ring } else { border })
                    .cursor_pointer()
                    .child(swatch(bg, surface, dot, border))
                    .child(
                        div()
                            .text_xs()
                            .truncate()
                            .when(selected, |this| this.font_weight(FontWeight::SEMIBOLD))
                            .child(name),
                    )
                    .on_click(move |_, _, cx| {
                        let target = target.clone();
                        update_app(cx, |app, cx| {
                            app.settings
                                .update(cx, |s, cx| s.apply_switch_target(&target, cx));
                        });
                    })
            },
        ));
    v_flex()
        .w(px(360.))
        .gap_3()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(t!("themes.switcher")),
        )
        .child(modes)
        .child(cards)
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child(t!("themes.switcher_note")),
        )
        .into_any_element()
}
