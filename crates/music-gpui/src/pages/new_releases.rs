//! New releases (`InnerTube::new_releases`): new albums and singles as a grid.

use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, ScrollHandle, SharedString,
    Styled as _, Task, Window,
};
use gpui_component::v_flex;
use music_core::innertube::Item;

use super::PageView;
use crate::app::MusicApp;
use crate::browse::{Load, PageCache, fetch, on_view};
use crate::components::items::{Playable, item_card, load_state};
use crate::components::{empty_state, grid, page_scroll, page_title};
use crate::icons::Lucide;

const CACHE_KEY: &str = "new-releases";

pub struct NewReleasesPage {
    scroll: ScrollHandle,
    data: Load<(Vec<Item>, Playable)>,
    _task: Option<Task<()>>,
}

impl NewReleasesPage {
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
        if let Some(items) = PageCache::get::<Vec<Item>>(CACHE_KEY, cx) {
            let songs = Playable::new(&items);
            self.data = Load::Ready((items, songs));
            return;
        }
        self.data = Load::Loading;
        self._task = Some(fetch(
            cx,
            |client, filter| Ok(filter.items(client.new_releases()?)),
            |this: &mut Self, result, cx| {
                if let Ok(items) = &result {
                    PageCache::put(CACHE_KEY, items.clone(), cx);
                }
                this.data = Load::from_result(result.map(|items| {
                    let songs = Playable::new(&items);
                    (items, songs)
                }));
            },
        ));
    }
}

impl Render for NewReleasesPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut content = v_flex().gap_6().child(page_title(
            t!("nav.new_releases"),
            Some(t!("new_releases.subtitle").into()),
            cx,
        ));
        match &self.data {
            Load::Ready((items, _)) if items.is_empty() => {
                content = content.child(empty_state(
                    Lucide::Sparkles,
                    t!("new_releases.empty.title"),
                    t!("new_releases.empty.detail"),
                    cx,
                ))
            }
            Load::Ready((items, songs)) => {
                content = content.child(grid(
                    items
                        .iter()
                        .enumerate()
                        .map(|(i, item)| {
                            item_card(
                                SharedString::from(format!("new-{i}")),
                                item,
                                songs.context(i),
                            )
                            .into_any_element()
                        })
                        .collect(),
                ))
            }
            Load::Loading => content = content.child(load_state("new", None, |_, _| {}, cx)),
            Load::Failed(e) => {
                content = content.child(load_state(
                    "new",
                    Some(e),
                    on_view(cx, |this: &mut Self, _, cx| this.load(cx)),
                    cx,
                ))
            }
        }
        page_scroll("new-releases-page", &self.scroll, content)
    }
}
