//! Moods & genres — stub until InnerTube moods exist.

use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, ScrollHandle, Styled as _,
    Window,
};

use super::PageView;
use crate::app::MusicApp;
use crate::components::{empty_state, page_scroll, page_title};
use crate::icons::Lucide;

pub struct MoodsPage {
    scroll: ScrollHandle,
}

impl MoodsPage {
    pub fn build(cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|_| Self {
            scroll: scroll.clone(),
        });
        PageView::new(page, Some(scroll))
    }
}

impl Render for MoodsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        page_scroll(
            "moods-page",
            &self.scroll,
            gpui_component::v_flex()
                .gap_6()
                .child(page_title("Moods & genres", None, cx))
                .child(empty_state(Lucide::Shapes, "Moods & genres are on their way", "Mood and genre playlists (Chill, Focus, Workout, …) arrive with the InnerTube browse layer in music-core.", cx)),
        )
    }
}
