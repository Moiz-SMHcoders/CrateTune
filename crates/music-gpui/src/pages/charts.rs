//! Charts — stub until InnerTube charts exist.

use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, ScrollHandle, Styled as _,
    Window,
};

use super::PageView;
use crate::app::MusicApp;
use crate::components::{empty_state, page_scroll, page_title};
use crate::icons::Lucide;

pub struct ChartsPage {
    scroll: ScrollHandle,
}

impl ChartsPage {
    pub fn build(cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|_| Self {
            scroll: scroll.clone(),
        });
        PageView::new(page, Some(scroll))
    }
}

impl Render for ChartsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        page_scroll(
            "charts-page",
            &self.scroll,
            gpui_component::v_flex()
                .gap_6()
                .child(page_title("Charts", None, cx))
                .child(empty_state(Lucide::ChartNoAxesColumn, "Charts are on their way", "Top songs, videos and artists by country arrive with the InnerTube charts browse in music-core.", cx)),
        )
    }
}
