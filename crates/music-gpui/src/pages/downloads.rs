//! Downloads — stub until downloads exist.

use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, ScrollHandle, Styled as _,
    Window,
};

use super::PageView;
use crate::app::MusicApp;
use crate::components::{empty_state, page_scroll, page_title};
use crate::icons::Lucide;

pub struct DownloadsPage {
    scroll: ScrollHandle,
}

impl DownloadsPage {
    pub fn build(cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|_| Self {
            scroll: scroll.clone(),
        });
        PageView::new(page, Some(scroll))
    }
}

impl Render for DownloadsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        page_scroll(
            "downloads-page",
            &self.scroll,
            gpui_component::v_flex()
                .gap_6()
                .child(page_title("Downloads", None, cx))
                .child(empty_state(Lucide::Download, "Nothing downloaded", "Offline downloads arrive with the download manager in music-core. Downloaded songs will be listed here.", cx)),
        )
    }
}
