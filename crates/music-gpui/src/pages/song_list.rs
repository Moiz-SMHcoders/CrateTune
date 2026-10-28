//! A virtualized list of library songs with an optional multi-select mode and a
//! selection action bar (play, queue, add to playlist, like, download, remove).
//! Pages own a [`SongList`] and hand an accessor to it to [`SongList::render`].

use std::collections::BTreeSet;
use std::rc::Rc;

use gpui::{
    AnyElement, Context, IntoElement, ParentElement as _, Pixels, Render, ScrollHandle,
    SharedString, Size, Styled as _, Window, div, prelude::FluentBuilder as _, px, size,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, VirtualListScrollHandle,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    scroll::ScrollableElement as _,
    v_virtual_list,
};
use music_core::Track;

use super::is_playing;
use crate::app::update_app;
use crate::components::track_row::{ROW_HEIGHT, TrackRow};
use crate::icons::Lucide;

/// Extra controls at the end of a row (e.g. reorder / remove in a playlist).
pub type RowExtra<P> = Rc<dyn Fn(&mut P, usize, &mut Window, &mut Context<P>) -> AnyElement>;

pub struct SongList {
    pub tracks: Rc<Vec<Track>>,
    sizes: Rc<Vec<Size<Pixels>>>,
    /// `Some` while selecting (indices into `tracks`).
    pub selection: Option<BTreeSet<usize>>,
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
            sizes: Rc::default(),
            selection: None,
            scroll: VirtualListScrollHandle::new(),
        }
    }

    /// The scroll handle the router saves / restores.
    pub fn scroll_handle(&self) -> ScrollHandle {
        self.scroll.base_handle().clone()
    }

    pub fn set_tracks(&mut self, tracks: Vec<Track>) {
        let n = tracks.len();
        self.sizes = Rc::new(vec![size(px(0.), px(ROW_HEIGHT)); n]);
        self.tracks = Rc::new(tracks);
        if let Some(selection) = &mut self.selection {
            selection.retain(|&i| i < n);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    pub fn selected_tracks(&self) -> Vec<Track> {
        match &self.selection {
            Some(sel) => sel
                .iter()
                .filter_map(|&i| self.tracks.get(i).cloned())
                .collect(),
            None => Vec::new(),
        }
    }

    pub fn toggle_selecting(&mut self) {
        self.selection = match self.selection {
            Some(_) => None,
            None => Some(BTreeSet::new()),
        };
    }

    /// The list itself; fills the remaining height of a flex column.
    pub fn render<P: Render>(
        &self,
        id: &'static str,
        get: fn(&mut P) -> &mut SongList,
        extra: Option<RowExtra<P>>,
        cx: &mut Context<P>,
    ) -> AnyElement {
        let list = v_virtual_list(
            cx.entity(),
            id,
            self.sizes.clone(),
            move |page, range, window, cx| {
                let list = get(page);
                let tracks = list.tracks.clone();
                let selection = list.selection.clone();
                range
                    .map(|ix| {
                        let active = is_playing(&tracks[ix].video_id, cx);
                        let check = selection.as_ref().map(|sel| {
                            Checkbox::new(SharedString::from(format!("{id}-sel-{ix}")))
                                .checked(sel.contains(&ix))
                                .on_click(cx.listener(
                                    move |page: &mut P, checked: &bool, _, cx| {
                                        if let Some(sel) = &mut get(page).selection {
                                            if *checked {
                                                sel.insert(ix);
                                            } else {
                                                sel.remove(&ix);
                                            }
                                        }
                                        cx.notify();
                                    },
                                ))
                        });
                        let trailing = extra.as_ref().map(|f| f(page, ix, window, cx));
                        h_flex()
                            .w_full()
                            .px_4()
                            .gap_2()
                            .children(check)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(TrackRow::new(id, tracks.clone(), ix).active(active)),
                            )
                            .children(trailing)
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
        let selecting = self.selection.is_some();
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
                    .on_click(cx.listener(move |page: &mut P, _, _, cx| {
                        get(page).toggle_selecting();
                        cx.notify();
                    })),
            )
    }

    /// Actions for the selected songs, shown while selecting. `remove` adds a page-specific
    /// "Remove" action (from the library / playlist).
    pub fn selection_bar<P: Render>(
        &self,
        id: &'static str,
        get: fn(&mut P) -> &mut SongList,
        remove: Option<(&'static str, RemoveHandler<P>)>,
        cx: &mut Context<P>,
    ) -> Option<AnyElement> {
        let selection = self.selection.as_ref()?;
        let count = selection.len();
        let all = count == self.tracks.len() && count > 0;
        let theme = cx.theme();
        let picked = self.selected_tracks();
        let none = picked.is_empty();
        let action = |key: &str, icon: Lucide, label: &str| {
            Button::new(SharedString::from(format!("{id}-{key}")))
                .ghost()
                .small()
                .icon(icon)
                .label(label.to_owned())
                .disabled(none)
        };
        let with = |f: fn(Vec<Track>, &mut Window, &mut gpui::App)| {
            let picked = picked.clone();
            move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut gpui::App| {
                f(picked.clone(), window, cx)
            }
        };
        let bar = h_flex()
            .flex_wrap()
            .gap_1()
            .px_3()
            .py_1p5()
            .rounded(px(8.))
            .bg(theme.secondary)
            .child(
                div()
                    .px_2()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(tp!("common.selected", count)),
            )
            .child(
                Button::new(SharedString::from(format!("{id}-all")))
                    .ghost()
                    .small()
                    .label(if all {
                        t!("song_list.select_none")
                    } else {
                        t!("common.select_all")
                    })
                    .on_click(cx.listener(move |page: &mut P, _, _, cx| {
                        let list = get(page);
                        let n = list.tracks.len();
                        if let Some(sel) = &mut list.selection {
                            if sel.len() == n {
                                sel.clear();
                            } else {
                                *sel = (0..n).collect();
                            }
                        }
                        cx.notify();
                    })),
            )
            .child(
                action("sel-play", Lucide::Play, t!("common.play")).on_click(with(|t, _, cx| {
                    update_app(cx, |app, cx| app.play_list(Rc::new(t), 0, cx));
                })),
            )
            .child(
                action("sel-next", Lucide::ListStart, t!("common.play_next")).on_click(with(
                    |t, window, cx| {
                        update_app(cx, |app, cx| app.queue_play_next(t, window, cx));
                    },
                )),
            )
            .child(
                action("sel-queue", Lucide::ListEnd, t!("common.add_to_queue")).on_click(with(
                    |t, window, cx| {
                        update_app(cx, |app, cx| app.queue_add(t, window, cx));
                    },
                )),
            )
            .child(
                action(
                    "sel-playlist",
                    Lucide::ListPlus,
                    t!("common.add_to_playlist"),
                )
                .on_click(with(|t, window, cx| {
                    update_app(cx, |app, cx| app.add_tracks_to_playlist(t, window, cx));
                })),
            )
            .child(
                action("sel-like", Lucide::Heart, t!("common.like")).on_click(with(
                    |t, window, cx| {
                        update_app(cx, |app, cx| app.set_liked(t, true, window, cx));
                    },
                )),
            )
            .child(
                action("sel-download", Lucide::Download, t!("common.download")).on_click(with(
                    |t, window, cx| {
                        update_app(cx, |app, cx| app.download_tracks(t, window, cx));
                    },
                )),
            )
            .when_some(remove, |bar, (label, handler)| {
                let picked = picked.clone();
                let indices: Vec<usize> = selection.iter().copied().collect();
                bar.child(
                    action("sel-remove", Lucide::Trash, label).on_click(cx.listener(
                        move |page: &mut P, _, window, cx| {
                            handler(page, picked.clone(), indices.clone(), window, cx);
                            if let Some(sel) = &mut get(page).selection {
                                sel.clear();
                            }
                            cx.notify();
                        },
                    )),
                )
            });
        Some(bar.into_any_element())
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
