//! Stats — stub until the library database records plays.

use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, ScrollHandle, Styled as _,
    Window,
};

use super::PageView;
use crate::app::MusicApp;
use crate::components::{empty_state, page_scroll, page_title};
use crate::icons::Lucide;

pub struct StatsPage {
    scroll: ScrollHandle,
}

impl StatsPage {
    pub fn build(cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|_| Self {
            scroll: scroll.clone(),
        });
        PageView::new(page, Some(scroll))
    }
}

impl Render for StatsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        page_scroll(
            "stats-page",
            &self.scroll,
            gpui_component::v_flex()
                .gap_6()
                .child(page_title("Stats", None, cx))
                .child(empty_state(Lucide::ChartColumn, "No listening stats yet", "Top songs, artists and listening time arrive with the library database (play history) in music-core.", cx)),
        )
    }
}
