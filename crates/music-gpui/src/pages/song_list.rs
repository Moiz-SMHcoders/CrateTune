//! A virtualized list of library songs with an optional checkbox column for
//! multi-select (the selection itself and its action bar are app-wide:
//! `selection.rs`, `components/selection_bar.rs`). Pages own a [`SongList`] and hand
//! an accessor to it to [`SongList::render`].

use std::rc::Rc;

use gpui::{
    AnyElement, Context, IntoElement, ParentElement as _, Pixels, Render, ScrollHandle,
    SharedString, Size, Styled as _, Window, div, px, size,
};
use gpui_component::{
    Disableable as _, Sizable as _, VirtualListScrollHandle,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    scroll::ScrollableElement as _,
    v_virtual_list,
};
use music_core::Track;

use super::is_playing;
use crate::app::update_app;
use crate::components::track_row::{RowMeta, TrackRow};
use crate::icons::Lucide;

/// Extra controls at the end of a row (e.g. reorder / remove in a playlist).
pub type RowExtra<P> = Rc<dyn Fn(&mut P, usize, &mut Window, &mut Context<P>) -> AnyElement>;

pub struct SongList {
    pub tracks: Rc<Vec<Track>>,
    /// The library's data per row (Added / Plays columns), when the page has it.
    meta: Rc<Vec<RowMeta>>,
    sizes: Rc<Vec<Size<Pixels>>>,
    /// The checkbox column is shown ("Select"); ⌘ / Shift-click select without it.
    pub selecting: bool,
    scroll: VirtualListScrollHandle,
}

impl Default for SongList {
    fn default() -> Self {
        Self::new()
    }
}

impl SongList {
    pub fn new() -> Self {
        Self {
            tracks: Rc::default(),
            meta: Rc::default(),
            sizes: Rc::default(),
            selecting: false,
            scroll: VirtualListScrollHandle::new(),
        }
    }

    /// The scroll handle the router saves / restores.
    pub fn scroll_handle(&self) -> ScrollHandle {
        self.scroll.base_handle().clone()
    }

    pub fn set_tracks(&mut self, tracks: Vec<Track>) {
        let n = tracks.len();
        self.sizes = Rc::new(vec![size(px(0.), px(crate::theme::row_height())); n]);
        self.tracks = Rc::new(tracks);
        self.meta = Rc::default();
    }

    /// Library songs: their tracks and what the Added / Plays columns show.
    pub fn set_songs(&mut self, songs: Vec<(Track, RowMeta)>) {
        let (tracks, meta): (Vec<_>, Vec<_>) = songs.into_iter().unzip();
        self.set_tracks(tracks);
        self.meta = Rc::new(meta);
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    /// Shows or hides the checkbox column; hiding it drops the selection.
    pub fn toggle_selecting(&mut self, window: &mut Window) {
        self.selecting = !self.selecting;
        if !self.selecting {
            crate::selection::clear(window);
        }
    }

    /// The list itself; fills the remaining height of a flex column.
    pub fn render<P: Render>(
        &self,
        id: &'static str,
        get: fn(&mut P) -> &mut SongList,
        extra: Option<RowExtra<P>>,
        cx: &mut Context<P>,
    ) -> AnyElement {
        // The density setting changed since the sizes were made: lay out with the new
        // row height until the page sets its tracks again.
        let height = px(crate::theme::row_height());
        let sizes = if self.sizes.first().is_some_and(|s| s.height != height) {
            Rc::new(vec![size(px(0.), height); self.sizes.len()])
        } else {
            self.sizes.clone()
        };
        let list = v_virtual_list(cx.entity(), id, sizes, move |page, range, window, cx| {
            let list = get(page);
            let tracks = list.tracks.clone();
            let meta = list.meta.clone();
            let selecting = list.selecting;
            range
                .map(|ix| {
                    let active = is_playing(&tracks[ix].video_id, cx);
                    let check = selecting.then(|| {
                        let tracks = tracks.clone();
                        Checkbox::new(SharedString::from(format!("{id}-sel-{ix}")))
                            .checked(crate::selection::is_selected(id, ix, &tracks[ix].video_id))
                            .on_click(move |checked: &bool, window, _| {
                                let on = *checked;
                                crate::selection::update(window, |s| {
                                    s.set(id, &tracks, ix, on);
                                    true
                                });
                            })
                    });
                    let trailing = extra.as_ref().map(|f| f(page, ix, window, cx));
                    h_flex()
                        .w_full()
                        .px_4()
                        .gap_2()
                        .children(check)
                        .child(
                            div().flex_1().min_w_0().child(
                                TrackRow::new(id, tracks.clone(), ix)
                                    .active(active)
                                    .meta(meta.get(ix).copied()),
                            ),
                        )
                        .children(trailing)
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.scroll);
        div()
            .flex_1()
            .min_h_0()
            .relative()
            .overflow_hidden()
            .child(list)
            .vertical_scrollbar(&self.scroll)
            .into_any_element()
    }

    /// "Play all / Shuffle / Select" buttons for a list header.
    pub fn header_buttons<P: Render>(
        &self,
        id: &'static str,
        get: fn(&mut P) -> &mut SongList,
        cx: &mut Context<P>,
    ) -> impl IntoElement + use<P> {
        let (tracks, tracks2) = (self.tracks.clone(), self.tracks.clone());
        let empty = self.tracks.is_empty();
        let selecting = self.selecting;
        h_flex()
            .gap_1()
            .flex_wrap()
            .child(
                Button::new(SharedString::from(format!("{id}-play")))
                    .primary()
                    .small()
                    .icon(Lucide::Play)
                    .label(t!("common.play"))
                    .disabled(empty)
                    .on_click(move |_, _, cx| {
                        let tracks = tracks.clone();
                        update_app(cx, |app, cx| app.play_list(tracks, 0, cx));
                    }),
            )
            .child(
                Button::new(SharedString::from(format!("{id}-shuffle")))
                    .ghost()
                    .small()
                    .icon(Lucide::Shuffle)
                    .label(t!("common.shuffle"))
                    .disabled(empty)
                    .on_click(move |_, _, cx| {
                        let tracks = tracks2.clone();
                        update_app(cx, |app, cx| {
                            app.set_shuffle(true, cx);
                            let start = random_index(tracks.len());
                            app.play_list(tracks, start, cx);
                        });
                    }),
            )
            .child(
                Button::new(SharedString::from(format!("{id}-select")))
                    .ghost()
                    .small()
                    .icon(Lucide::ListChecks)
                    .label(if selecting {
                        t!("common.done")
                    } else {
                        t!("common.select")
                    })
                    .disabled(empty)
                    .on_click(cx.listener(move |page: &mut P, _, window, cx| {
                        get(page).toggle_selecting(window);
                        cx.notify();
                    })),
            )
    }

    /// Gives the list's selection a "Remove" action (from the library / playlist) in
    /// the app-wide selection bar. Called while the page renders.
    pub fn register_remove<P: Render>(
        &self,
        id: &'static str,
        remove: Option<(&'static str, RemoveHandler<P>)>,
        cx: &mut Context<P>,
    ) {
        let Some((label, handler)) = remove else {
            crate::selection::unregister_remove(id);
            return;
        };
        let page = cx.entity().downgrade();
        crate::selection::register_remove(
            id,
            label,
            Rc::new(move |tracks, indices, window, cx| {
                let handler = handler.clone();
                page.update(cx, |page, cx| handler(page, tracks, indices, window, cx))
                    .ok();
            }),
        );
    }
}

/// Removes the selected songs (tracks, their indices in the list).
pub type RemoveHandler<P> =
    Rc<dyn Fn(&mut P, Vec<Track>, Vec<usize>, &mut Window, &mut Context<P>)>;

/// A start index for "Shuffle" (the queue shuffles the rest).
pub fn random_index(len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos() as usize);
    nanos % len
}
