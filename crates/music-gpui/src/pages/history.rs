//! History: tracks played this session (from the queue's history), newest
//! first. Persistent listening history comes from the library database later.

use std::rc::Rc;

use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, ScrollHandle, Styled as _,
    Window,
};
use gpui_component::v_flex;
use music_core::Track;

use super::{PageView, is_playing};
use crate::app::{MusicApp, read_app};
use crate::components::track_row::TrackRow;
use crate::components::{empty_state, page_scroll, page_title};
use crate::icons::Lucide;

pub struct HistoryPage {
    scroll: ScrollHandle,
}

impl HistoryPage {
    pub fn build(cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|_| Self {
            scroll: scroll.clone(),
        });
        PageView::new(page, Some(scroll))
    }
}

impl Render for HistoryPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let played: Rc<Vec<Track>> = Rc::new(
            read_app(cx, |app, _| app.queue.history().cloned().collect()).unwrap_or_default(),
        );
        let mut content = v_flex().gap_6().child(page_title(
            "History",
            Some("Played this session. Saved history arrives with the library database.".into()),
            cx,
        ));
        if played.is_empty() {
            content = content.child(empty_state(
                Lucide::Clock,
                "Nothing played yet",
                "Songs you skip past or finish show up here.",
                cx,
            ));
        } else {
            content = content.child(v_flex().children((0..played.len()).map(|ix| {
                let active = is_playing(&played[ix].video_id, cx);
                TrackRow::new("history", played.clone(), ix)
                    .numbered(false)
                    .active(active)
            })));
        }
        page_scroll("history-page", &self.scroll, content)
    }
}
