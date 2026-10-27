//! Moods & genres (`InnerTube::moods`): one grid of tiles per category; a tile
//! opens its category page (`Route::Browse`).

use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, ScrollHandle, SharedString,
    Styled as _, Task, Window,
};
use gpui_component::v_flex;
use music_core::innertube::MoodCategory;

use super::PageView;
use crate::app::MusicApp;
use crate::browse::{Load, PageCache, fetch, on_view};
use crate::components::items::{load_state, mood_tile};
use crate::components::{empty_state, grid, page_scroll, page_title, section_header};
use crate::icons::Lucide;

const CACHE_KEY: &str = "moods";

pub struct MoodsPage {
    scroll: ScrollHandle,
    data: Load<Vec<MoodCategory>>,
    _task: Option<Task<()>>,
}

impl MoodsPage {
    pub fn build(cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|cx| {
            let mut page = Self {
                scroll: scroll.clone(),
                data: Load::Loading,
                _task: None,
            };
            page.load(cx);
            page
        });
        PageView::new(page, Some(scroll))
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        if let Some(data) = PageCache::get::<Vec<MoodCategory>>(CACHE_KEY, cx) {
            self.data = Load::Ready(data);
            return;
        }
        self.data = Load::Loading;
        self._task = Some(fetch(
            cx,
            |client, _| Ok(client.moods()?.categories),
            |this: &mut Self, result, cx| {
                if let Ok(data) = &result {
                    PageCache::put(CACHE_KEY, data.clone(), cx);
                }
                this.data = Load::from_result(result);
            },
        ));
    }
}

impl Render for MoodsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut content = v_flex()
            .gap_8()
            .child(page_title("Moods & genres", None, cx));
        match &self.data {
            Load::Ready(categories) if categories.is_empty() => {
                content = content.child(empty_state(
                    Lucide::Shapes,
                    "Nothing here",
                    "YouTube Music returned no moods or genres for this region.",
                    cx,
                ))
            }
            Load::Ready(categories) => {
                for (c, category) in categories.iter().enumerate() {
                    content = content.child(
                        v_flex()
                            .gap_3()
                            .child(section_header(category.title.clone(), None, None, cx))
                            .child(grid(
                                category
                                    .buttons
                                    .iter()
                                    .enumerate()
                                    .map(|(i, button)| {
                                        mood_tile(
                                            SharedString::from(format!("mood-{c}-{i}")),
                                            button,
                                            cx,
                                        )
                                    })
                                    .collect(),
                            )),
                    );
                }
            }
            Load::Loading => content = content.child(load_state("moods", None, |_, _| {}, cx)),
            Load::Failed(e) => {
                content = content.child(load_state(
                    "moods",
                    Some(e),
                    on_view(cx, |this: &mut Self, _, cx| this.load(cx)),
                    cx,
                ))
            }
        }
        page_scroll("moods-page", &self.scroll, content)
    }
}
