//! Playlist page (`Route::Playlist { id, name }`) — stub until music-core can browse it.

use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, ScrollHandle, Styled as _,
    Window,
};
use gpui_component::v_flex;

use super::PageView;
use crate::app::MusicApp;
use crate::components::{empty_state, page_scroll, page_title};
use crate::icons::Lucide;

pub struct PlaylistPage {
    id: String,
    name: String,
    scroll: ScrollHandle,
}

impl PlaylistPage {
    pub fn build(id: String, name: String, cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|_| Self {
            id,
            name,
            scroll: scroll.clone(),
        });
        PageView::new(page, Some(scroll))
    }
}

impl Render for PlaylistPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        page_scroll(
            "playlist-page",
            &self.scroll,
            v_flex()
                .gap_6()
                .child(page_title(
                    self.name.clone(),
                    Some(format!("Playlist · {}", self.id).into()),
                    cx,
                ))
                .child(empty_state(
                    Lucide::ListMusic,
                    "Coming soon",
                    "Playlist pages arrive with the InnerTube playlist browse in music-core.",
                    cx,
                )),
        )
    }
}
