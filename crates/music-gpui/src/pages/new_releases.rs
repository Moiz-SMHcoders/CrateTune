//! New releases — stub until InnerTube new releases exist.

use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, ScrollHandle, Styled as _,
    Window,
};

use super::PageView;
use crate::app::MusicApp;
use crate::components::{empty_state, page_scroll, page_title};
use crate::icons::Lucide;

pub struct NewReleasesPage {
    scroll: ScrollHandle,
}

impl NewReleasesPage {
    pub fn build(cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|_| Self {
            scroll: scroll.clone(),
        });
        PageView::new(page, Some(scroll))
    }
}

impl Render for NewReleasesPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        page_scroll(
            "new_releases-page",
            &self.scroll,
            gpui_component::v_flex()
                .gap_6()
                .child(page_title("New releases", None, cx))
                .child(empty_state(Lucide::Sparkles, "New releases are on their way", "New albums and singles arrive with the InnerTube new-releases browse in music-core.", cx)),
        )
    }
}
