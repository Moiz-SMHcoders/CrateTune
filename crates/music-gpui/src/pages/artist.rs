//! Artist page (`Route::Artist { id, name }`) — stub until music-core can browse it.

use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, ScrollHandle, Styled as _,
    Window,
};
use gpui_component::v_flex;

use super::PageView;
use crate::app::MusicApp;
use crate::components::{empty_state, page_scroll, page_title};
use crate::icons::Lucide;

pub struct ArtistPage {
    id: String,
    name: String,
    scroll: ScrollHandle,
}

impl ArtistPage {
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

impl Render for ArtistPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        page_scroll(
            "artist-page",
            &self.scroll,
            v_flex()
                .gap_6()
                .child(page_title(
                    self.name.clone(),
                    Some(format!("Artist · {}", self.id).into()),
                    cx,
                ))
                .child(empty_state(
                    Lucide::MicVocal,
                    "Coming soon",
                    "Artist pages arrive with the InnerTube artist browse in music-core: top songs, albums, singles, videos and related artists.",
                    cx,
                )),
        )
    }
}
