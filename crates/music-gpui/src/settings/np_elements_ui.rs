//! Appearance → Now Playing / Mini player → "Elements": a design (layout) picker
//! and a check box per element that view offers (`np_elements.rs`), plus the panel
//! methods Now Playing's "Customize this view" panel, the mini player's
//! right-click menu and the scripts use.

use gpui::{AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, px};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex, v_flex,
};

use super::np_elements::{ElementView, Elements, NpElement};
use super::panel::SettingsPanel;
use super::prefs::{MiniPlayerSkin, NowPlayingLayout};
use super::prefs_ui::choice;
use super::widgets::{Anchor as _, note};

impl SettingsPanel {
    /// What `view` shows.
    pub fn np_elements(&self, view: ElementView) -> Elements {
        self.settings.now_playing.shown(view)
    }

    pub fn set_np_element(
        &mut self,
        view: ElementView,
        element: NpElement,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        // The app re-renders Now Playing (and the mini player with it) on the event.
        self.update_now_playing(cx, |n| n.set_element(view, element, on));
    }

    pub fn set_np_elements(&mut self, view: ElementView, shown: Elements, cx: &mut Context<Self>) {
        self.update_now_playing(cx, |n| n.set_elements(view, shown));
    }

    pub fn reset_np_elements(&mut self, view: ElementView, cx: &mut Context<Self>) {
        self.update_now_playing(cx, |n| n.reset_elements(view));
    }
}

/// The "Elements" rows of the Now Playing group.
pub(super) fn render_np(panel: &mut SettingsPanel, cx: &mut Context<SettingsPanel>) -> AnyElement {
    let layout = panel
        .prefs
        .elements_pick
        .0
        .unwrap_or(panel.settings.now_playing.layout);
    let picker = choice(
        "np-elements-design",
        &NowPlayingLayout::ALL,
        layout,
        NowPlayingLayout::title,
        |this, l, cx| {
            this.prefs.elements_pick.0 = Some(l);
            cx.notify();
        },
        cx,
    );
    render(
        panel,
        ElementView::Np(layout),
        picker,
        t!("np_elements.settings.np_note"),
        cx,
    )
    .anchor("appearance.now_playing.elements")
    .into_any_element()
}

/// The "Elements" rows of the Mini player group.
pub(super) fn render_mini(
    panel: &mut SettingsPanel,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let skin = panel
        .prefs
        .elements_pick
        .1
        .unwrap_or(panel.settings.now_playing.mini_skin);
    let picker = choice(
        "mini-elements-layout",
        &MiniPlayerSkin::ALL,
        skin,
        MiniPlayerSkin::title,
        |this, k, cx| {
            this.prefs.elements_pick.1 = Some(k);
            cx.notify();
        },
        cx,
    );
    render(
        panel,
        ElementView::Mini(skin),
        picker,
        t!("np_elements.settings.mini_note"),
        cx,
    )
    .anchor("appearance.mini_player.elements")
    .into_any_element()
}

fn render(
    panel: &mut SettingsPanel,
    view: ElementView,
    picker: gpui::Div,
    note_text: &'static str,
    cx: &mut Context<SettingsPanel>,
) -> gpui::Div {
    let shown = panel.np_elements(view);
    let prefix = match view {
        ElementView::Np(_) => "np",
        ElementView::Mini(_) => "mini",
    };
    let boxes = view.offered().iter().map(|e| {
        h_flex().w(px(200.)).child(
            Checkbox::new(SharedString::from(format!("{prefix}-element-{}", e.key())))
                .checked(shown.has(e))
                .label(e.title())
                .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                    this.set_np_element(view, e, *checked, cx)
                })),
        )
    });
    v_flex()
        .gap_2()
        .pt_3()
        .border_t_1()
        .border_color(cx.theme().border)
        .child(
            h_flex()
                .justify_between()
                .gap_4()
                .child(
                    gpui::div()
                        .text_sm()
                        .child(t!("np_elements.settings.title")),
                )
                .child(
                    Button::new(SharedString::from(format!("{prefix}-elements-reset")))
                        .ghost()
                        .small()
                        .label(t!("np_elements.reset"))
                        .disabled(panel.settings.now_playing.elements_default(view))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.reset_np_elements(view, cx)),
                        ),
                ),
        )
        .child(picker)
        .child(h_flex().flex_wrap().gap_x_4().gap_y_2().children(boxes))
        .child(note(note_text, cx))
}
