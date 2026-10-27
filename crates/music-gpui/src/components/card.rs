//! Album / playlist / artist / song tiles for shelves and grids (ArchiveTune's
//! `ItemGrid` style: square art, title, one muted subtitle line; artists round).

use std::rc::Rc;

use gpui::{
    App, ClickEvent, ElementId, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{ActiveTheme as _, menu::ContextMenuExt as _, menu::PopupMenu, v_flex};

use super::artwork::artwork;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CardKind {
    Album,
    Playlist,
    Artist,
    Song,
}

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
type MenuBuilder = Rc<dyn Fn(PopupMenu, &mut Window, &mut gpui::Context<PopupMenu>) -> PopupMenu>;

#[derive(IntoElement)]
pub struct Card {
    id: ElementId,
    kind: CardKind,
    title: SharedString,
    subtitle: Option<SharedString>,
    artwork_url: Option<String>,
    width: f32,
    on_click: Option<ClickHandler>,
    menu: Option<MenuBuilder>,
}

impl Card {
    pub fn new(id: impl Into<ElementId>, kind: CardKind, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            kind,
            title: title.into(),
            subtitle: None,
            artwork_url: None,
            width: 160.,
            on_click: None,
            menu: None,
        }
    }

    pub fn subtitle(mut self, subtitle: impl Into<SharedString>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    pub fn artwork(mut self, url: Option<String>) -> Self {
        self.artwork_url = url;
        self
    }

    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(f));
        self
    }

    /// Right-click menu (e.g. [`super::track_row::track_menu`] for song cards).
    pub fn context_menu(
        mut self,
        f: impl Fn(PopupMenu, &mut Window, &mut gpui::Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> Self {
        self.menu = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for Card {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let side = px(self.width);
        let round = self.kind == CardKind::Artist;
        let art = artwork(
            self.artwork_url,
            side,
            if round { side / 2. } else { px(8.) },
            cx,
        );
        let theme = cx.theme();
        let hover = theme.list_hover;
        let muted = theme.muted_foreground;
        let card = v_flex()
            .id(self.id)
            .w(side + px(16.))
            .flex_none()
            .p_2()
            .gap_2()
            .rounded(px(10.))
            .when(round, |this| this.items_center())
            .when_some(self.on_click, |this, f| {
                this.cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(move |e, w, cx| f(e, w, cx))
            })
            .child(art)
            .child(
                v_flex()
                    .w_full()
                    .gap_0p5()
                    .when(round, |this| this.items_center())
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .when(round, |this| this.text_center())
                            .child(self.title),
                    )
                    .children(self.subtitle.map(|s| {
                        div()
                            .w_full()
                            .truncate()
                            .text_xs()
                            .text_color(muted)
                            .when(round, |this| this.text_center())
                            .child(s)
                    })),
            );
        match self.menu {
            Some(menu) => card
                .context_menu(move |m, w, cx| menu(m, w, cx))
                .into_any_element(),
            None => card.into_any_element(),
        }
    }
}
