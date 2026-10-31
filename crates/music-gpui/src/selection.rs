//! Multi-select of song rows, on every song list: ⌘-click (Ctrl-click on Windows and
//! Linux) adds or removes a row, Shift-click selects the range from the last row
//! clicked, ⌘A / Ctrl+A selects the whole list under the pointer, Esc clears. A plain
//! click still plays the list from that row (and drops the selection).
//!
//! Selection model ported from Zuno (Apache-2.0, `src/ui/hooks/useTrackSelection.ts`,
//! `src/ui/components/SelectionBar.tsx`): modelled on file managers, the anchor of a
//! Shift range is the last row touched, and the selected songs are acted on in list
//! order. The selection bar ([`crate::components::selection_bar`]) floats above the
//! player bar while something is selected.
//!
//! One selection exists at a time, for one list. Lists are told apart by their row
//! id prefix (`TrackRow::new`'s first argument, unique per page list); rows match by
//! index *and* video id, so a page that rebuilds its list each frame keeps the
//! selection while a list whose songs changed doesn't show a stale one. The state is
//! thread-local (UI thread only), read while rows render.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::{App, Window};
use music_core::Track;

/// How a row was clicked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    /// No modifier: play from the row.
    Plain,
    /// ⌘ / Ctrl: add or remove the row.
    Toggle,
    /// Shift: select from the anchor to the row.
    Range,
}

impl Click {
    pub fn from_modifiers(modifiers: &gpui::Modifiers) -> Self {
        let secondary = if cfg!(target_os = "macos") {
            modifiers.platform
        } else {
            modifiers.control
        };
        if modifiers.shift {
            Click::Range
        } else if secondary {
            Click::Toggle
        } else {
            Click::Plain
        }
    }
}

/// What the row should do after a click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Play the list from the row (as before multi-select existed).
    Play,
    /// The click changed the selection; nothing plays.
    Selected,
}

/// "Remove" for the selected songs of one list (from a playlist, the library, …).
pub type RemoveFn = Rc<dyn Fn(Vec<Track>, Vec<usize>, &mut Window, &mut App)>;

#[derive(Default)]
pub struct Selection {
    /// Row id prefix of the list the selection is in.
    list: Option<String>,
    /// That list as last clicked (for the bar's actions and ⌘A).
    tracks: Rc<Vec<Track>>,
    /// Selected indices → their video ids.
    picked: BTreeMap<usize, String>,
    anchor: Option<usize>,
    /// The list the pointer was last over (⌘A without a selection).
    hovered: Option<(String, Rc<Vec<Track>>)>,
    /// Per list prefix: the label and handler of its "Remove" action.
    remove: BTreeMap<String, (String, RemoveFn)>,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.picked.is_empty()
    }

    pub fn len(&self) -> usize {
        self.picked.len()
    }

    pub fn list(&self) -> Option<&str> {
        self.list.as_deref()
    }

    /// The selected list's songs.
    pub fn tracks(&self) -> Rc<Vec<Track>> {
        self.tracks.clone()
    }

    pub fn is_selected(&self, list: &str, index: usize, video_id: &str) -> bool {
        self.list.as_deref() == Some(list)
            && self.picked.get(&index).is_some_and(|id| id == video_id)
    }

    /// Whether every row of the list is selected.
    pub fn all_selected(&self) -> bool {
        !self.tracks.is_empty() && self.picked.len() == self.tracks.len()
    }

    /// Switches to `list` (forgetting a selection in another list) and refreshes its
    /// songs.
    fn focus(&mut self, list: &str, tracks: &Rc<Vec<Track>>) {
        if self.list.as_deref() != Some(list) {
            self.picked.clear();
            self.anchor = None;
            self.list = Some(list.to_owned());
        }
        self.tracks = tracks.clone();
        // Rows that changed under the selection are dropped.
        let tracks = &self.tracks;
        self.picked
            .retain(|&i, id| tracks.get(i).is_some_and(|t| &t.video_id == id));
    }

    fn pick(&mut self, index: usize) {
        if let Some(track) = self.tracks.get(index) {
            self.picked.insert(index, track.video_id.clone());
        }
    }

    /// A click on row `index` of `list` (whose songs are `tracks`).
    pub fn click(
        &mut self,
        list: &str,
        tracks: &Rc<Vec<Track>>,
        index: usize,
        click: Click,
    ) -> Outcome {
        match click {
            Click::Plain => {
                self.clear();
                self.anchor = Some(index);
                Outcome::Play
            }
            Click::Toggle => {
                self.focus(list, tracks);
                if self.picked.remove(&index).is_none() {
                    self.pick(index);
                }
                self.anchor = Some(index);
                Outcome::Selected
            }
            Click::Range => {
                // The anchor of a range is the last row touched in *this* list.
                let same = self.list.as_deref() == Some(list);
                let anchor = self.anchor.filter(|_| same).unwrap_or(index);
                self.focus(list, tracks);
                let (from, to) = if anchor <= index {
                    (anchor, index)
                } else {
                    (index, anchor)
                };
                for i in from..=to.min(tracks.len().saturating_sub(1)) {
                    self.pick(i);
                }
                self.anchor = Some(anchor);
                Outcome::Selected
            }
        }
    }

    /// A row's checkbox (the library lists' select mode): adds or removes the row.
    pub fn set(&mut self, list: &str, tracks: &Rc<Vec<Track>>, index: usize, on: bool) {
        self.focus(list, tracks);
        if on {
            self.pick(index);
        } else {
            self.picked.remove(&index);
        }
        self.anchor = Some(index);
    }

    /// ⌘A: everything in the selected list, else in the list under the pointer.
    /// Returns whether anything was selected.
    pub fn select_all(&mut self) -> bool {
        let target = match (&self.list, &self.hovered) {
            (Some(list), _) if !self.picked.is_empty() => Some((list.clone(), self.tracks.clone())),
            (_, Some((list, tracks))) => Some((list.clone(), tracks.clone())),
            _ => None,
        };
        let Some((list, tracks)) = target else {
            return false;
        };
        self.select_all_in(&list, &tracks);
        !self.picked.is_empty()
    }

    pub fn select_all_in(&mut self, list: &str, tracks: &Rc<Vec<Track>>) {
        self.focus(list, tracks);
        for i in 0..tracks.len() {
            self.pick(i);
        }
    }

    pub fn clear(&mut self) {
        self.picked.clear();
        self.anchor = None;
    }

    /// The pointer is over a row of `list`.
    pub fn hover(&mut self, list: &str, tracks: &Rc<Vec<Track>>) {
        let same = self
            .hovered
            .as_ref()
            .is_some_and(|(l, t)| l == list && Rc::ptr_eq(t, tracks));
        if !same {
            self.hovered = Some((list.to_owned(), tracks.clone()));
        }
    }

    /// The selected songs in list order, and their indices.
    pub fn selected(&self) -> (Vec<Track>, Vec<usize>) {
        self.picked
            .keys()
            .filter_map(|&i| Some((self.tracks.get(i)?.clone(), i)))
            .unzip()
    }

    /// The selection's "Remove" action, if its list has one.
    pub fn remove_action(&self) -> Option<(String, RemoveFn)> {
        self.remove.get(self.list.as_deref()?).cloned()
    }
}

thread_local! {
    static SELECTION: RefCell<Selection> = RefCell::default();
}

/// Reads the selection.
pub fn with<R>(f: impl FnOnce(&Selection) -> R) -> R {
    SELECTION.with(|s| f(&s.borrow()))
}

/// Changes the selection; the window is redrawn when `f` returns true.
pub fn update(window: &mut Window, f: impl FnOnce(&mut Selection) -> bool) -> bool {
    let changed = SELECTION.with(|s| f(&mut s.borrow_mut()));
    if changed {
        // Rows live in cached page views: a refresh re-renders them all.
        window.refresh();
    }
    changed
}

pub fn is_selected(list: &str, index: usize, video_id: &str) -> bool {
    with(|s| s.is_selected(list, index, video_id))
}

pub fn is_empty() -> bool {
    with(Selection::is_empty)
}

/// Clears the selection (Esc, navigation). Returns whether there was one.
pub fn clear(window: &mut Window) -> bool {
    update(window, |s| {
        let had = !s.is_empty();
        s.clear();
        had
    })
}

/// Remembers the list under the pointer (cheap; called on hover).
pub fn hover(list: &str, tracks: &Rc<Vec<Track>>) {
    SELECTION.with(|s| s.borrow_mut().hover(list, tracks));
}

/// Registers the "Remove" action of a list (called while the page renders).
pub fn register_remove(list: &str, label: &str, handler: RemoveFn) {
    SELECTION.with(|s| {
        s.borrow_mut()
            .remove
            .insert(list.to_owned(), (label.to_owned(), handler));
    });
}

/// The list has no "Remove" (any more).
pub fn unregister_remove(list: &str) {
    SELECTION.with(|s| {
        s.borrow_mut().remove.remove(list);
    });
}

/// The bar's actions (the dispatch is tested on its own).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchAction {
    Play,
    PlayNext,
    AddToQueue,
    AddToPlaylist,
    Like,
    Dislike,
    Download,
    Remove,
}

impl BatchAction {
    /// The actions the bar offers: `remove` where the list has a "Remove", downloads
    /// only for YouTube songs, dislikes only where YouTube can rate.
    pub fn offered(tracks: &[Track], remove: bool) -> Vec<BatchAction> {
        let youtube = tracks.iter().any(|t| !t.is_local());
        let mut out = vec![
            BatchAction::Play,
            BatchAction::PlayNext,
            BatchAction::AddToQueue,
            BatchAction::AddToPlaylist,
            BatchAction::Like,
        ];
        if youtube {
            out.push(BatchAction::Dislike);
            out.push(BatchAction::Download);
        }
        if remove {
            out.push(BatchAction::Remove);
        }
        out
    }

    /// Songs the action applies to (local files can't be disliked or downloaded).
    pub fn applies_to(self, tracks: Vec<Track>) -> Vec<Track> {
        match self {
            BatchAction::Dislike | BatchAction::Download => {
                tracks.into_iter().filter(|t| !t.is_local()).collect()
            }
            _ => tracks,
        }
    }

    /// Whether the selection is dropped after the action (it is kept for queueing, so
    /// the same songs can also go to a playlist).
    pub fn clears_selection(self) -> bool {
        matches!(
            self,
            BatchAction::Play | BatchAction::Remove | BatchAction::Dislike
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(n: usize) -> Rc<Vec<Track>> {
        Rc::new(
            (0..n)
                .map(|i| Track {
                    video_id: format!("v{i}"),
                    ..Track::default()
                })
                .collect(),
        )
    }

    fn picked(s: &Selection) -> Vec<usize> {
        s.selected().1
    }

    #[test]
    fn plain_clicks_still_play() {
        let mut s = Selection::default();
        let l = list(5);
        assert_eq!(s.click("a", &l, 2, Click::Plain), Outcome::Play);
        assert!(s.is_empty());
        // ...and drop a selection.
        s.click("a", &l, 1, Click::Toggle);
        assert_eq!(s.click("a", &l, 3, Click::Plain), Outcome::Play);
        assert!(s.is_empty());
    }

    #[test]
    fn toggle_and_range() {
        let mut s = Selection::default();
        let l = list(10);
        assert_eq!(s.click("a", &l, 2, Click::Toggle), Outcome::Selected);
        s.click("a", &l, 5, Click::Toggle);
        assert_eq!(picked(&s), [2, 5]);
        // Shift extends from the last row touched (5) — either direction.
        s.click("a", &l, 8, Click::Range);
        assert_eq!(picked(&s), [2, 5, 6, 7, 8]);
        s.click("a", &l, 3, Click::Range);
        assert_eq!(picked(&s), [2, 3, 4, 5, 6, 7, 8]);
        // Toggling a selected row removes it.
        s.click("a", &l, 4, Click::Toggle);
        assert_eq!(picked(&s), [2, 3, 5, 6, 7, 8]);
        assert!(s.is_selected("a", 3, "v3"));
        assert!(!s.is_selected("a", 4, "v4"));
        assert!(!s.is_selected("b", 3, "v3"), "another list");
        // Shift without an anchor selects just the row; out-of-range ends are clamped.
        let mut s = Selection::default();
        s.click("a", &l, 4, Click::Range);
        assert_eq!(picked(&s), [4]);
        s.click("a", &l, 42, Click::Range);
        assert_eq!(picked(&s), [4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn one_list_at_a_time() {
        let mut s = Selection::default();
        let (a, b) = (list(4), list(6));
        s.click("a", &a, 1, Click::Toggle);
        s.click("b", &b, 3, Click::Toggle);
        assert_eq!(s.list(), Some("b"));
        assert_eq!(picked(&s), [3]);
        // The anchor of another list isn't used for a range.
        s.click("a", &a, 2, Click::Range);
        assert_eq!((s.list(), picked(&s)), (Some("a"), vec![2]));
    }

    #[test]
    fn rows_that_changed_are_not_selected() {
        let mut s = Selection::default();
        let l = list(4);
        s.click("a", &l, 1, Click::Toggle);
        s.click("a", &l, 3, Click::Toggle);
        // The same list rebuilt (a new Rc each frame): still selected.
        let rebuilt = list(4);
        assert!(s.is_selected("a", 1, &rebuilt[1].video_id));
        // Songs moved: the row shows another song now.
        assert!(!s.is_selected("a", 1, "something else"));
        // Refreshing with a shorter list drops what's gone.
        s.click("a", &list(2), 0, Click::Toggle);
        assert_eq!(picked(&s), [0, 1]);
    }

    #[test]
    fn select_all_targets_the_selected_or_hovered_list() {
        let mut s = Selection::default();
        assert!(!s.select_all(), "nothing hovered, nothing selected");
        let (a, b) = (list(3), list(5));
        s.hover("b", &b);
        assert!(s.select_all());
        assert_eq!((s.list(), picked(&s)), (Some("b"), vec![0, 1, 2, 3, 4]));
        assert!(s.all_selected());
        s.clear();
        s.click("a", &a, 0, Click::Toggle);
        assert!(
            s.select_all(),
            "the list with a selection wins over the hovered one"
        );
        assert_eq!((s.list(), picked(&s)), (Some("a"), vec![0, 1, 2]));
        // Checkboxes.
        s.set("a", &a, 1, false);
        assert_eq!(picked(&s), [0, 2]);
        assert!(!s.all_selected());
    }

    #[test]
    fn batch_actions_dispatch() {
        let yt = Track {
            video_id: "dQw4w9WgXcQ".into(),
            ..Track::default()
        };
        let local = Track {
            video_id: "local:1".into(),
            local_path: Some("/music/a.flac".into()),
            ..Track::default()
        };
        use BatchAction::*;
        assert_eq!(
            BatchAction::offered(&[yt.clone(), local.clone()], true),
            [
                Play,
                PlayNext,
                AddToQueue,
                AddToPlaylist,
                Like,
                Dislike,
                Download,
                Remove
            ]
        );
        assert_eq!(
            BatchAction::offered(std::slice::from_ref(&local), false),
            [Play, PlayNext, AddToQueue, AddToPlaylist, Like]
        );
        let both = vec![yt.clone(), local.clone()];
        assert_eq!(Download.applies_to(both.clone()), [yt.clone()]);
        assert_eq!(Dislike.applies_to(both.clone()), [yt.clone()]);
        assert_eq!(AddToQueue.applies_to(both.clone()), both);
        assert!(Play.clears_selection() && Remove.clears_selection());
        assert!(!AddToQueue.clears_selection() && !AddToPlaylist.clears_selection());
        // The remove handler is per list.
        let mut s = Selection::default();
        let l = list(2);
        s.remove
            .insert("a".into(), ("Remove".into(), Rc::new(|_, _, _, _| {})));
        s.click("b", &l, 0, Click::Toggle);
        assert!(s.remove_action().is_none());
        s.click("a", &l, 0, Click::Toggle);
        assert_eq!(
            s.remove_action().map(|(label, _)| label).as_deref(),
            Some("Remove")
        );
    }
}
