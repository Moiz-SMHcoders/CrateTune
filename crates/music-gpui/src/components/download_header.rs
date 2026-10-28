//! "Download all" for a list header (liked songs, library songs, playlists,
//! albums), after ArchiveTune's `HeaderDownloadState` (GPL-3.0): the list's
//! YouTube songs (local files don't count) are either not downloaded, partly
//! downloaded, being downloaded ("N of M", Cancel) or all downloaded ("Remove
//! downloads"). The state comes from the UI-thread download flags
//! (`store::download_flag`), so it costs no database or manager call; the
//! downloads themselves go through the download manager's queue (two at a time),
//! so a 1,000-song list queues 1,000 jobs, not 1,000 requests.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::{
    AnyElement, App, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div, px,
    relative,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use music_core::Track;

use crate::app::update_app;
use crate::icons::Lucide;
use crate::store::{DownloadFlag, download_flag, flags_generation};

/// Download state of a whole list (ArchiveTune's `HeaderDownloadState`, with
/// "downloading" split from "partial").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderDownloadState {
    /// Nothing downloaded or queued (also: no downloadable songs, `total == 0`).
    None { total: usize },
    /// Some songs stored, nothing in progress.
    Partial { done: usize, total: usize },
    /// Songs queued or downloading.
    Downloading { done: usize, total: usize },
    /// Every song stored.
    All { total: usize },
}

impl HeaderDownloadState {
    pub fn total(self) -> usize {
        match self {
            HeaderDownloadState::None { total }
            | HeaderDownloadState::Partial { total, .. }
            | HeaderDownloadState::Downloading { total, .. }
            | HeaderDownloadState::All { total } => total,
        }
    }
}

/// The distinct YouTube video ids of `tracks` (local files can't be downloaded).
pub fn downloadable_ids(tracks: &[Track]) -> Vec<&str> {
    let mut seen = HashSet::new();
    tracks
        .iter()
        .filter(|t| !t.is_local())
        .map(|t| t.video_id.as_str())
        .filter(|id| seen.insert(*id))
        .collect()
}

/// The state of a list whose download flags `flag` reports. Stored previews
/// (`Done { complete: false }`) count as downloaded, failed ones as not.
pub fn header_state(
    ids: &[&str],
    flag: impl Fn(&str) -> Option<DownloadFlag>,
) -> HeaderDownloadState {
    let total = ids.len();
    let (mut done, mut active) = (0, 0);
    for id in ids {
        match flag(id) {
            Some(DownloadFlag::Done { .. }) => done += 1,
            Some(DownloadFlag::Queued | DownloadFlag::Downloading) => active += 1,
            Some(DownloadFlag::Failed) | None => {}
        }
    }
    if total > 0 && done == total {
        HeaderDownloadState::All { total }
    } else if active > 0 {
        HeaderDownloadState::Downloading { done, total }
    } else if done > 0 {
        HeaderDownloadState::Partial { done, total }
    } else {
        HeaderDownloadState::None { total }
    }
}

/// Songs of `tracks` still to download (not stored, not queued; failed ones again).
pub fn missing(tracks: &[Track], flag: impl Fn(&str) -> Option<DownloadFlag>) -> Vec<Track> {
    let mut seen = HashSet::new();
    tracks
        .iter()
        .filter(|t| !t.is_local() && seen.insert(t.video_id.as_str()))
        .filter(|t| matches!(flag(&t.video_id), None | Some(DownloadFlag::Failed)))
        .cloned()
        .collect()
}

thread_local! {
    /// Per list (its `Rc` address): flag generation, length and the state, so a
    /// header doesn't re-scan thousands of ids on every frame while scrolling.
    static CACHE: RefCell<HashMap<usize, (u64, usize, HeaderDownloadState)>> = RefCell::default();
}

/// [`header_state`] of `tracks` with the current flags, cached per list.
pub fn state_of(tracks: &Rc<Vec<Track>>) -> HeaderDownloadState {
    let key = Rc::as_ptr(tracks) as usize;
    let generation = flags_generation();
    if let Some(state) = CACHE.with(|c| {
        c.borrow()
            .get(&key)
            .filter(|(g, len, _)| *g == generation && *len == tracks.len())
            .map(|e| e.2)
    }) {
        return state;
    }
    let state = header_state(&downloadable_ids(tracks), download_flag);
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 32 {
            c.clear();
        }
        c.insert(key, (generation, tracks.len(), state));
    });
    state
}

/// The header control: "Download all", progress + Cancel, or "Remove downloads".
/// `None` when the list has no downloadable songs. `download` overrides what
/// "Download all" does (e.g. an online playlist first loads its remaining pages).
pub fn download_button(
    id: &str,
    tracks: &Rc<Vec<Track>>,
    download: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    cx: &App,
) -> Option<AnyElement> {
    let state = state_of(tracks);
    if state.total() == 0 {
        return None;
    }
    let theme = cx.theme();
    let (muted, track_bg, fill) = (theme.muted_foreground, theme.secondary, theme.progress_bar);
    let list = tracks.clone();
    let start = move |window: &mut Window, cx: &mut App| match &download {
        Some(f) => f(window, cx),
        None => {
            let list = (*list).clone();
            update_app(cx, |app, cx| app.download_collection(list, window, cx));
        }
    };
    let element = match state {
        HeaderDownloadState::None { .. } | HeaderDownloadState::Partial { .. } => {
            let label = match state {
                HeaderDownloadState::Partial { done, total } => {
                    t!("downloads.header.download_rest", done = done, total = total)
                }
                _ => t!("downloads.header.download_all").to_owned(),
            };
            Button::new(SharedString::from(format!("{id}-download-all")))
                .outline()
                .small()
                .icon(Lucide::Download)
                .label(label)
                .on_click(move |_, window, cx| start(window, cx))
                .into_any_element()
        }
        HeaderDownloadState::Downloading { done, total } => {
            let list = tracks.clone();
            h_flex()
                .gap_2()
                .child(
                    v_flex()
                        .gap_1()
                        .child(div().text_xs().text_color(muted).child(t!(
                            "downloads.header.progress",
                            done = done,
                            total = total
                        )))
                        .child(
                            div()
                                .w(px(120.))
                                .h(px(4.))
                                .rounded_full()
                                .bg(track_bg)
                                .child(
                                    div()
                                        .h_full()
                                        .rounded_full()
                                        .bg(fill)
                                        .w(relative(done as f32 / total.max(1) as f32)),
                                ),
                        ),
                )
                .child(
                    Button::new(SharedString::from(format!("{id}-download-cancel")))
                        .ghost()
                        .small()
                        .icon(Lucide::X)
                        .label(t!("common.cancel"))
                        .on_click(move |_, window, cx| {
                            let list = (*list).clone();
                            update_app(cx, |app, cx| app.cancel_collection(list, window, cx));
                        }),
                )
                .into_any_element()
        }
        HeaderDownloadState::All { total } => {
            let list = tracks.clone();
            Button::new(SharedString::from(format!("{id}-download-remove")))
                .ghost()
                .small()
                .icon(Lucide::CircleCheck)
                .label(t!("downloads.header.remove"))
                .tooltip(tp!("downloads.header.all_downloaded", total))
                .on_click(move |_, window, cx| {
                    let list = list.clone();
                    window.open_alert_dialog(cx, move |alert, _, _| {
                        let list = list.clone();
                        alert
                            .title(t!("downloads.header.remove_title"))
                            .description(tp!("downloads.header.remove_detail", total))
                            .show_cancel(true)
                            .ok_text(t!("downloads.header.remove"))
                            .on_ok(move |_, window, cx| {
                                let list = (*list).clone();
                                update_app(cx, |app, cx| {
                                    app.remove_collection_downloads(list, window, cx)
                                });
                                true
                            })
                    });
                })
                .into_any_element()
        }
    };
    Some(element)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str) -> Track {
        Track {
            video_id: id.into(),
            title: id.into(),
            ..Default::default()
        }
    }

    fn flags(pairs: &[(&str, DownloadFlag)]) -> impl Fn(&str) -> Option<DownloadFlag> + use<> {
        let map: HashMap<String, DownloadFlag> =
            pairs.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect();
        move |id| map.get(id).copied()
    }

    const DONE: DownloadFlag = DownloadFlag::Done { complete: true };

    #[test]
    fn states_follow_the_flags() {
        let ids = ["a", "b", "c"];
        assert_eq!(
            header_state(&ids, flags(&[])),
            HeaderDownloadState::None { total: 3 }
        );
        assert_eq!(
            header_state(&ids, flags(&[("a", DONE)])),
            HeaderDownloadState::Partial { done: 1, total: 3 }
        );
        assert_eq!(
            header_state(&ids, flags(&[("a", DONE), ("b", DownloadFlag::Queued)])),
            HeaderDownloadState::Downloading { done: 1, total: 3 }
        );
        assert_eq!(
            header_state(&ids, flags(&[("c", DownloadFlag::Downloading)])),
            HeaderDownloadState::Downloading { done: 0, total: 3 }
        );
        let all = flags(&[
            ("a", DONE),
            ("b", DONE),
            ("c", DownloadFlag::Done { complete: false }),
        ]);
        assert_eq!(
            header_state(&ids, all),
            HeaderDownloadState::All { total: 3 }
        );
        // Failed ones are "not downloaded".
        assert_eq!(
            header_state(
                &ids,
                flags(&[("a", DONE), ("b", DONE), ("c", DownloadFlag::Failed)])
            ),
            HeaderDownloadState::Partial { done: 2, total: 3 }
        );
        assert_eq!(
            header_state(&[], flags(&[])),
            HeaderDownloadState::None { total: 0 }
        );
    }

    #[test]
    fn local_files_and_duplicates_are_skipped() {
        let mut local = track("local:abc");
        local.local_path = Some("/music/a.flac".into());
        let tracks = vec![track("a"), track("b"), track("a"), local];
        assert_eq!(downloadable_ids(&tracks), ["a", "b"]);
        let f = flags(&[("b", DownloadFlag::Queued)]);
        let todo = missing(&tracks, f);
        assert_eq!(
            todo.len(),
            1,
            "b is queued, a appears twice, local is skipped"
        );
        assert_eq!(todo[0].video_id, "a");
        let f = flags(&[("a", DownloadFlag::Failed)]);
        assert_eq!(missing(&tracks, f).len(), 2, "failed ones are retried");
    }

    #[test]
    fn large_lists_are_cheap() {
        let tracks: Vec<Track> = (0..5000).map(|i| track(&format!("id{i}"))).collect();
        let pairs: Vec<(String, DownloadFlag)> =
            (0..2500).map(|i| (format!("id{i}"), DONE)).collect();
        let map: HashMap<String, DownloadFlag> = pairs.into_iter().collect();
        let started = std::time::Instant::now();
        let state = header_state(&downloadable_ids(&tracks), |id| map.get(id).copied());
        assert_eq!(
            state,
            HeaderDownloadState::Partial {
                done: 2500,
                total: 5000
            }
        );
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
    }
}
