//! Search results for one query (`Route::Search { query }`). The shell owns the
//! search field and debounces typing; each query gets a fresh page.
//!
//! Songs only for now (`InnerTube::search_songs`); the full search (albums,
//! artists, playlists, suggestions) plugs in here once music-core has it.

use std::rc::Rc;

use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Pixels, Render, SharedString, Size,
    Styled as _, Task, Window, div, prelude::FluentBuilder as _, px, size,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _, VirtualListScrollHandle, h_flex,
    scroll::ScrollableElement as _, spinner::Spinner, v_flex, v_virtual_list,
};
use music_core::{InnerTube, Track};

use super::{PageView, is_playing};
use crate::app::{MusicApp, update_app};
use crate::components::empty_state;
use crate::components::track_row::{ROW_HEIGHT, TrackRow};
use crate::icons::Lucide;

enum Status {
    Idle,
    Searching,
    Done,
    Failed(String),
}

pub struct SearchPage {
    query: String,
    status: Status,
    tracks: Rc<Vec<Track>>,
    row_sizes: Rc<Vec<Size<Pixels>>>,
    scroll: VirtualListScrollHandle,
    _search: Option<Task<()>>,
}

impl SearchPage {
    pub fn build(query: String, cx: &mut Context<MusicApp>) -> PageView {
        let scroll = VirtualListScrollHandle::new();
        let base = scroll.base_handle().clone();
        let page = cx.new(|cx| {
            let mut page = Self {
                query: query.clone(),
                status: Status::Idle,
                tracks: Rc::default(),
                row_sizes: Rc::default(),
                scroll,
                _search: None,
            };
            page.start(cx);
            page
        });
        PageView::new(page, Some(base))
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        if self.query.is_empty() {
            return;
        }
        self.status = Status::Searching;
        let query = self.query.clone();
        self._search = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { InnerTube::default().search_songs(&query) })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(tracks) => {
                        this.row_sizes = Rc::new(
                            tracks
                                .iter()
                                .map(|_| size(px(0.), px(ROW_HEIGHT)))
                                .collect(),
                        );
                        this.tracks = Rc::new(tracks);
                        this.status = Status::Done;
                        let tracks = this.tracks.clone();
                        update_app(cx, |app, cx| app.on_search_results(tracks, cx));
                    }
                    Err(e) => this.status = Status::Failed(e.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
    }
}

impl Render for SearchPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let status: Option<SharedString> = match &self.status {
            Status::Idle => None,
            Status::Searching => Some("Searching…".into()),
            Status::Done => Some(format!("{} songs", self.tracks.len()).into()),
            Status::Failed(_) => Some("Search failed".into()),
        };
        let header = h_flex()
            .px_8()
            .pt_6()
            .pb_3()
            .gap_3()
            .items_end()
            .child(
                div()
                    .text_2xl()
                    .font_weight(gpui::FontWeight::BOLD)
                    .truncate()
                    .child(if self.query.is_empty() {
                        "Search".to_owned()
                    } else {
                        format!("“{}”", self.query)
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .pb_1()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .when(matches!(self.status, Status::Searching), |this| {
                        this.child(Spinner::new().small())
                    })
                    .children(status),
            );

        let body = if self.tracks.is_empty() {
            let (title, detail) = match &self.status {
                Status::Failed(e) => ("Search failed".to_owned(), e.clone()),
                Status::Searching => ("Searching…".into(), String::new()),
                Status::Done => ("No songs found".into(), "Try another search.".into()),
                Status::Idle => (
                    "Search YouTube Music".into(),
                    "Type a song, artist or album in the search field (⌘F).".into(),
                ),
            };
            div()
                .flex_1()
                .child(empty_state(Lucide::Search, title, detail, cx))
                .into_any_element()
        } else {
            let list = v_virtual_list(
                cx.entity(),
                "results",
                self.row_sizes.clone(),
                move |this, range, _window, cx| {
                    let tracks = this.tracks.clone();
                    range
                        .map(|ix| {
                            let active = is_playing(&tracks[ix].video_id, cx);
                            div()
                                .px_4()
                                .child(TrackRow::new("result", tracks.clone(), ix).active(active))
                        })
                        .collect::<Vec<_>>()
                },
            )
            .track_scroll(&self.scroll);
            div()
                .flex_1()
                .min_h_0()
                .relative()
                .overflow_hidden()
                .child(list)
                .vertical_scrollbar(&self.scroll)
                .into_any_element()
        };
        v_flex().size_full().child(header).child(body)
    }
}
