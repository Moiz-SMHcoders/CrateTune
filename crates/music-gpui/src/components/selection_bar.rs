//! The bar for multi-selected songs (`selection.rs`): it floats above the player bar
//! while songs are selected in any list, so the rows stay where they are while you
//! aim at it. Play, play next, add to queue, add to playlist, like, dislike,
//! download, the list's own "Remove" where it has one, select all / none, clear.
//! Layout after Zuno's `SelectionBar.tsx` (Apache-2.0).

use std::rc::Rc;

use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
};

use crate::app::{MusicApp, update_app};
use crate::icons::Lucide;
use crate::selection::{self, BatchAction};

impl MusicApp {
    /// The floating bar, or nothing without a selection.
    pub(crate) fn render_selection_bar(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (count, tracks, all, remove) = selection::with(|s| {
            (
                s.len(),
                s.selected().0,
                s.all_selected(),
                s.remove_action().map(|(label, _)| label),
            )
        });
        if count == 0 {
            return None;
        }
        let theme = cx.theme();
        let actions = BatchAction::offered(&tracks, remove.is_some());
        let mut bar = h_flex()
            // Clicks on the bar must not reach the rows under it.
            .occlude()
            .w_full()
            .max_w(px(920.))
            .flex_wrap()
            .justify_center()
            .items_center()
            .gap_1()
            .px_2()
            .py_1p5()
            .rounded(crate::theme::radius(18.))
            .bg(theme.secondary)
            .border_1()
            .border_color(theme.border)
            .shadow_lg()
            .child(
                div()
                    .px_2()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tp!("common.selected", count)),
            );
        for action in actions {
            let (icon, label): (Lucide, SharedString) = match action {
                BatchAction::Play => (Lucide::Play, t!("common.play").into()),
                BatchAction::PlayNext => (Lucide::ListStart, t!("common.play_next").into()),
                BatchAction::AddToQueue => (Lucide::ListEnd, t!("common.add_to_queue").into()),
                BatchAction::AddToPlaylist => {
                    (Lucide::ListPlus, t!("common.add_to_playlist").into())
                }
                BatchAction::Like => (Lucide::Heart, t!("common.like").into()),
                BatchAction::Dislike => (Lucide::ThumbsDown, t!("common.dislike").into()),
                BatchAction::Download => (Lucide::Download, t!("common.download").into()),
                BatchAction::Remove => (Lucide::Trash, remove.clone().unwrap_or_default().into()),
            };
            bar = bar.child(
                Button::new(SharedString::from(format!("sel-{action:?}")))
                    .ghost()
                    .small()
                    .icon(icon)
                    .label(label)
                    .on_click(move |_, window, cx| run(action, window, cx)),
            );
        }
        bar = bar
            .child(div().mx_0p5().h(px(20.)).w(px(1.)).bg(theme.border))
            .child(
                Button::new("sel-all")
                    .ghost()
                    .small()
                    .label(if all {
                        t!("song_list.select_none")
                    } else {
                        t!("common.select_all")
                    })
                    .tooltip(t!("selection.tooltip.select_all"))
                    .on_click(move |_, window, _| {
                        selection::update(window, |s| {
                            if all {
                                s.clear();
                            } else {
                                let list = s.list().unwrap_or_default().to_owned();
                                let tracks = s.tracks();
                                s.select_all_in(&list, &tracks);
                            }
                            true
                        });
                    }),
            )
            .child(
                Button::new("sel-clear")
                    .ghost()
                    .small()
                    .icon(IconName::Close)
                    .tooltip(t!("selection.tooltip.clear"))
                    .on_click(|_, window, _| {
                        selection::clear(window);
                    }),
            );
        Some(bar.into_any_element())
    }

    /// Runs a [`BatchAction`] on the selected songs (except `Remove`, which is the
    /// list's own handler).
    pub(crate) fn run_batch(
        &mut self,
        action: BatchAction,
        tracks: Vec<music_core::Track>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tracks = action.applies_to(tracks);
        if tracks.is_empty() {
            return;
        }
        match action {
            BatchAction::Play => self.play_list(Rc::new(tracks), 0, cx),
            BatchAction::PlayNext => self.queue_play_next(tracks, window, cx),
            BatchAction::AddToQueue => self.queue_add(tracks, window, cx),
            BatchAction::AddToPlaylist => self.add_tracks_to_playlist(tracks, window, cx),
            BatchAction::Like => self.set_liked(tracks, true, window, cx),
            BatchAction::Dislike => self.dislike_many(tracks, window, cx),
            BatchAction::Download => self.download_tracks(tracks, window, cx),
            BatchAction::Remove => {}
        }
    }
}

/// A click on one of the bar's actions.
fn run(action: BatchAction, window: &mut Window, cx: &mut gpui::App) {
    let (tracks, indices, remove) = selection::with(|s| {
        let (tracks, indices) = s.selected();
        (tracks, indices, s.remove_action())
    });
    match (action, remove) {
        (BatchAction::Remove, Some((_, handler))) => handler(tracks, indices, window, cx),
        (BatchAction::Remove, None) => {}
        _ => {
            update_app(cx, |app, cx| app.run_batch(action, tracks, window, cx));
        }
    }
    if action.clears_selection() {
        selection::clear(window);
    }
}
