//! Local files — stub until local files exist.

use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, ScrollHandle, Styled as _,
    Window,
};

use super::PageView;
use crate::app::MusicApp;
use crate::components::{empty_state, page_scroll, page_title};
use crate::icons::Lucide;

pub struct LocalFilesPage {
    scroll: ScrollHandle,
}

impl LocalFilesPage {
    pub fn build(cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|_| Self {
            scroll: scroll.clone(),
        });
        PageView::new(page, Some(scroll))
    }
}

impl Render for LocalFilesPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        page_scroll(
            "local_files-page",
            &self.scroll,
            gpui_component::v_flex()
                .gap_6()
                .child(page_title("Local files", None, cx))
                .child(empty_state(Lucide::FileMusic, "No local music yet", "Pick folders to scan once local-file playback lands in music-core; tracks from disk will show up here.", cx)),
        )
    }
}
