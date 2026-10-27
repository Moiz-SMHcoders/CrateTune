//! Any browse target as titled shelves (`Route::Browse`): mood / genre
//! category pages and "see all" targets that aren't an artist, album or
//! playlist. More shelves load as you scroll (`browse_continuation`).

use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, ScrollHandle, Styled as _,
    Task, Window,
};
use gpui_component::v_flex;
use music_core::innertube::BrowseTarget;

use super::PageView;
use crate::app::MusicApp;
use crate::browse::{Load, PageCache, Paging, fetch, on_view};
use crate::components::items::{ShelfModel, ShelfStyle, load_state, more_footer, shelf_view};
use crate::components::{empty_state, page_scroll, page_title};
use crate::icons::Lucide;

#[derive(Clone)]
struct Data {
    title: Option<String>,
    shelves: Vec<ShelfModel>,
    paging: Paging,
}

pub struct BrowsePage {
    target: BrowseTarget,
    name: String,
    scroll: ScrollHandle,
    data: Load<Data>,
    _task: Option<Task<()>>,
}

impl BrowsePage {
    pub fn build(
        id: String,
        params: Option<String>,
        name: String,
        cx: &mut Context<MusicApp>,
    ) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|cx| {
            let mut page = Self {
                target: BrowseTarget {
                    browse_id: id,
                    params,
                    page_type: None,
                },
                name,
                scroll: scroll.clone(),
                data: Load::Loading,
                _task: None,
            };
            page.load(cx);
            page
        });
        PageView::new(page, Some(scroll))
    }

    fn key(&self) -> String {
        format!(
            "browse:{}:{}",
            self.target.browse_id,
            self.target.params.as_deref().unwrap_or("")
        )
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        if let Some(data) = PageCache::get::<Data>(&self.key(), cx) {
            self.data = Load::Ready(data);
            return;
        }
        self.data = Load::Loading;
        let target = self.target.clone();
        self._task = Some(fetch(
            cx,
            move |client, filter| {
                let page = client.browse(&target)?;
                Ok((page.title, filter.shelves(page.shelves), page.continuation))
            },
            |this: &mut Self, result, cx| {
                let result = result.map(|(title, shelves, next)| Data {
                    title,
                    shelves: ShelfModel::all(shelves),
                    paging: Paging::new(next),
                });
                this.data = Load::from_result(result);
                this.store(cx);
            },
        ));
    }

    fn store(&self, cx: &mut Context<Self>) {
        if let Load::Ready(data) = &self.data {
            let mut data = data.clone();
            data.paging = Paging::new(data.paging.continuation.clone());
            PageCache::put(&self.key(), data, cx);
        }
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(data) = self.data.ready_mut() else {
            return;
        };
        let Some(token) = data.paging.start(Some(&self.scroll)) else {
            return;
        };
        self._task = Some(fetch(
            cx,
            move |client, filter| {
                let page = client.browse_continuation(&token)?;
                Ok((filter.shelves(page.shelves), page.continuation))
            },
            |this: &mut Self, result, cx| {
                if let Some(data) = this.data.ready_mut() {
                    match result {
                        Ok((shelves, next)) => {
                            data.shelves.extend(ShelfModel::all(shelves));
                            data.paging.finish(Ok(next));
                        }
                        Err(e) => data.paging.finish(Err(e)),
                    }
                }
                this.store(cx);
            },
        ));
        cx.notify();
    }
}

impl Render for BrowsePage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self
            .data
            .ready()
            .is_some_and(|d| d.paging.should_load(&self.scroll))
        {
            self.load_more(cx);
        }
        let title = self
            .data
            .ready()
            .and_then(|d| d.title.clone())
            .unwrap_or_else(|| self.name.clone());
        let mut content = v_flex().gap_8().child(page_title(title, None, cx));
        match &self.data {
            Load::Ready(data) if data.shelves.is_empty() => {
                content = content.child(empty_state(
                    Lucide::Shapes,
                    "Nothing here",
                    "This page has no playlists or songs (or all of them are filtered out).",
                    cx,
                ))
            }
            Load::Ready(data) => {
                for (i, model) in data.shelves.iter().enumerate() {
                    content = content.child(shelf_view(
                        &format!("browse-{i}"),
                        model,
                        ShelfStyle::Auto,
                        cx,
                    ));
                }
                if data.paging.has_more() || data.paging.error.is_some() {
                    content = content.child(more_footer(
                        "browse",
                        data.paging.loading,
                        data.paging.error.as_deref(),
                        on_view(cx, |this: &mut Self, _, cx| {
                            if let Some(d) = this.data.ready_mut() {
                                d.paging.error = None;
                            }
                            this.load_more(cx)
                        }),
                        cx,
                    ));
                }
            }
            Load::Loading => content = content.child(load_state("browse", None, |_, _| {}, cx)),
            Load::Failed(e) => {
                content = content.child(load_state(
                    "browse",
                    Some(e),
                    on_view(cx, |this: &mut Self, _, cx| this.load(cx)),
                    cx,
                ))
            }
        }
        page_scroll("browse-page", &self.scroll, content)
    }
}
