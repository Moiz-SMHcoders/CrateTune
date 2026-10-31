//! Queue tabs: several queues kept side by side ("Main", "Workout", "Focus"), each
//! remembering its songs, current track, position, shuffle order and repeat mode.
//!
//! Idea from Zuno (Apache-2.0, `src/player/TabManager.ts`), which gives every UI tab
//! its own player and page stack. Here only the *queue* is per tab: one engine plays,
//! so one tab is the playing one and its queue is the live [`crate::queue::Queue`]
//! (saved as before under the library's saved queue). The others are kept as
//! [`SavedQueue`]s. Clicking a tab shows it in the queue panel (a preview, see
//! [`TabPreview`]); "Continue" makes it the playing tab: the live queue goes into the
//! old tab with its position, and the chosen one plays on from where it was left.
//!
//! Stored in the library database under [`TABS_KEY`]; a library without it (or one
//! from an older version) has a single tab holding the saved queue, so nothing changes
//! for anyone who never opens a second tab.

use music_core::Track;
use music_core::library::SavedQueue;
use serde::{Deserialize, Serialize};

/// Library key of the tabs (the playing tab's queue stays under the saved queue's key).
pub const TABS_KEY: &str = "gpui.queue_tabs";
/// More would not fit the panel's tab row.
pub const MAX_TABS: usize = 8;
/// Longest tab name.
pub const MAX_NAME: usize = 40;

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct QueueTab {
    pub id: u64,
    /// Empty: the first tab's default name ("Main", translated when shown).
    pub name: String,
    /// The queue while this isn't the playing tab.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue: Option<SavedQueue>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct QueueTabs {
    pub tabs: Vec<QueueTab>,
    /// The tab whose queue is the live one.
    pub playing: usize,
    /// The tab the queue panel shows (not saved: the panel opens on the playing tab).
    #[serde(skip)]
    pub viewed: usize,
}

impl Default for QueueTabs {
    fn default() -> Self {
        Self {
            tabs: vec![QueueTab {
                id: 1,
                ..QueueTab::default()
            }],
            playing: 0,
            viewed: 0,
        }
    }
}

/// What the panel shows for a tab that isn't playing.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct TabPreview {
    pub current: Option<Track>,
    /// Where it was left in the current track.
    pub position_ms: u64,
    /// "Next in queue": songs queued by hand.
    pub user: Vec<Track>,
    /// "Next from …".
    pub context: Vec<Track>,
    pub context_name: Option<String>,
    pub context_radio: bool,
}

impl QueueTabs {
    /// Makes stored tabs consistent (at least one tab, indices in range, unique ids;
    /// the playing tab's queue is the live one, so a stored copy is dropped).
    pub fn sanitize(mut self) -> Self {
        if self.tabs.is_empty() {
            return Self::default();
        }
        self.tabs.truncate(MAX_TABS);
        let mut next = 1;
        for tab in &mut self.tabs {
            tab.id = next;
            next += 1;
            tab.name = clean_name(&tab.name);
        }
        self.playing = self.playing.min(self.tabs.len() - 1);
        self.tabs[self.playing].queue = None;
        self.viewed = self.playing;
        self
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn viewing_playing(&self) -> bool {
        self.viewed == self.playing
    }

    pub fn can_add(&self) -> bool {
        self.tabs.len() < MAX_TABS
    }

    fn next_id(&self) -> u64 {
        self.tabs.iter().map(|t| t.id).max().unwrap_or(0) + 1
    }

    /// Adds an empty tab named `name` and shows it. Returns its index.
    pub fn add(&mut self, name: &str) -> Option<usize> {
        if !self.can_add() {
            return None;
        }
        let id = self.next_id();
        self.tabs.push(QueueTab {
            id,
            name: clean_name(name),
            queue: Some(SavedQueue::default()),
        });
        self.viewed = self.tabs.len() - 1;
        Some(self.viewed)
    }

    pub fn rename(&mut self, index: usize, name: &str) -> bool {
        let name = clean_name(name);
        match self.tabs.get_mut(index) {
            Some(tab) if !name.is_empty() && tab.name != name => {
                tab.name = name;
                true
            }
            _ => false,
        }
    }

    /// Removes a tab that isn't playing (there is always one tab left).
    pub fn remove(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() || index == self.playing || self.tabs.len() == 1 {
            return false;
        }
        let was_viewed = self.viewed == index;
        self.tabs.remove(index);
        if self.playing > index {
            self.playing -= 1;
        }
        if was_viewed {
            // The removed tab was shown: show the playing one instead.
            self.viewed = self.playing;
        } else if self.viewed > index {
            self.viewed -= 1;
        }
        true
    }

    /// Shows tab `index` in the panel.
    pub fn view(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() || index == self.viewed {
            return false;
        }
        self.viewed = index;
        true
    }

    /// Makes `to` the playing tab: `live` (the queue playing now, with its position) is
    /// kept in the old tab, and the queue to load is returned. `None` if `to` already
    /// plays or doesn't exist.
    pub fn switch(&mut self, to: usize, live: SavedQueue) -> Option<SavedQueue> {
        if to >= self.tabs.len() || to == self.playing {
            return None;
        }
        self.tabs[self.playing].queue = Some(live);
        let next = self.tabs[to].queue.take().unwrap_or_default();
        self.playing = to;
        self.viewed = to;
        Some(next)
    }

    /// The panel's view of a tab that isn't playing (`None` for the playing one, which
    /// the panel shows live).
    pub fn preview(&self, index: usize) -> Option<TabPreview> {
        if index == self.playing {
            return None;
        }
        let saved = self.tabs.get(index)?.queue.as_ref()?;
        Some(preview(saved))
    }
}

/// Play order of a saved queue split the way the live queue shows it.
pub fn preview(saved: &SavedQueue) -> TabPreview {
    let n = saved.tracks.len();
    let order: Vec<usize> = match &saved.shuffle_order {
        Some(order) if is_permutation(order, n) => order.clone(),
        _ => (0..n).collect(),
    };
    let mut preview = TabPreview {
        context_name: saved.context_name.clone(),
        context_radio: saved.context_radio,
        ..TabPreview::default()
    };
    if n == 0 {
        return preview;
    }
    let index = saved.index.min(n - 1);
    let at = order.iter().position(|&i| i == index).unwrap_or(0);
    preview.current = Some(saved.tracks[index].clone());
    preview.position_ms = saved.position_ms;
    for &i in &order[at + 1..] {
        let track = saved.tracks[i].clone();
        if saved.user_queued.contains(&i) {
            preview.user.push(track);
        } else {
            preview.context.push(track);
        }
    }
    preview
}

fn is_permutation(order: &[usize], n: usize) -> bool {
    let mut sorted = order.to_vec();
    sorted.sort_unstable();
    sorted.len() == n && sorted.iter().enumerate().all(|(i, v)| i == *v)
}

/// Trimmed, one line, at most [`MAX_NAME`] characters.
pub fn clean_name(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_NAME)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str) -> Track {
        Track {
            video_id: id.into(),
            title: id.to_uppercase(),
            ..Track::default()
        }
    }

    fn saved(ids: &[&str], index: usize) -> SavedQueue {
        SavedQueue::new(
            ids.iter().map(|id| track(id)).collect(),
            index,
            std::time::Duration::from_millis(42_000),
        )
    }

    fn ids(tracks: &[Track]) -> Vec<&str> {
        tracks.iter().map(|t| t.video_id.as_str()).collect()
    }

    #[test]
    fn switching_keeps_each_queue_and_its_position() {
        let mut tabs = QueueTabs::default();
        assert_eq!(tabs.add("Workout"), Some(1));
        assert_eq!(tabs.viewed, 1);
        assert_eq!(tabs.playing, 0);
        // The new tab is empty; switching to it stores the live queue in "Main".
        let main = saved(&["a", "b", "c"], 1);
        let loaded = tabs.switch(1, main.clone()).unwrap();
        assert!(loaded.tracks.is_empty());
        assert_eq!((tabs.playing, tabs.viewed), (1, 1));
        assert_eq!(tabs.tabs[0].queue.as_ref(), Some(&main));
        assert!(
            tabs.tabs[1].queue.is_none(),
            "the playing tab is the live queue"
        );
        // Back to Main: its queue and position come back, Workout keeps its own.
        let workout = saved(&["x"], 0);
        let back = tabs.switch(0, workout.clone()).unwrap();
        assert_eq!(back, main);
        assert_eq!(back.position_ms, 42_000);
        assert_eq!(tabs.tabs[1].queue.as_ref(), Some(&workout));
        // Switching to the playing tab (or nowhere) does nothing.
        assert_eq!(tabs.switch(0, SavedQueue::default()), None);
        assert_eq!(tabs.switch(9, SavedQueue::default()), None);
    }

    #[test]
    fn tabs_can_be_named_added_and_removed() {
        let mut tabs = QueueTabs::default();
        assert!(tabs.rename(0, "  Main \n mix "));
        assert_eq!(tabs.tabs[0].name, "Main mix");
        assert!(!tabs.rename(0, "   "), "an empty name is refused");
        assert_eq!(clean_name(&"x".repeat(100)).len(), MAX_NAME);
        for i in 1..MAX_TABS {
            assert_eq!(tabs.add(&format!("T{i}")), Some(i));
        }
        assert_eq!(tabs.add("too many"), None);
        // The playing tab can't be removed; others can, indices follow.
        tabs.switch(3, SavedQueue::default());
        assert!(!tabs.remove(3));
        assert!(tabs.remove(1));
        assert_eq!(tabs.playing, 2);
        assert_eq!(tabs.tabs[2].name, "T3");
        tabs.view(4);
        assert!(tabs.remove(4));
        assert_eq!(
            tabs.viewed, tabs.playing,
            "the shown tab went: show the playing one"
        );
        let mut one = QueueTabs::default();
        assert!(!one.remove(0), "the last tab stays");
    }

    #[test]
    fn stored_tabs_are_sanitized() {
        let json = r#"{"tabs":[{"id":7,"name":"A","queue":{"tracks":[],"index":0}},{"id":7,"name":" B "}],"playing":5}"#;
        let tabs: QueueTabs = serde_json::from_str(json).unwrap();
        let tabs = tabs.sanitize();
        assert_eq!(tabs.playing, 1);
        assert_eq!(tabs.viewed, 1);
        assert_eq!(tabs.tabs[1].name, "B");
        assert_ne!(tabs.tabs[0].id, tabs.tabs[1].id);
        assert!(tabs.tabs[1].queue.is_none());
        // Nothing stored (or garbage): one tab, the saved queue as before.
        let empty: QueueTabs = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.sanitize(), QueueTabs::default());
        // Round trip without the view state.
        let mut tabs = QueueTabs::default();
        tabs.add("Focus");
        let back: QueueTabs = serde_json::from_str(&serde_json::to_string(&tabs).unwrap()).unwrap();
        assert_eq!(back.tabs, tabs.tabs);
        assert_eq!(back.viewed, 0);
    }

    #[test]
    fn preview_follows_play_order_and_parts() {
        let mut q = saved(&["a", "b", "c", "d", "e"], 1);
        q.user_queued = vec![3];
        q.context_name = Some("Album".into());
        let p = preview(&q);
        assert_eq!(p.current.unwrap().video_id, "b");
        assert_eq!(ids(&p.user), ["d"]);
        assert_eq!(ids(&p.context), ["c", "e"]);
        assert_eq!(p.context_name.as_deref(), Some("Album"));
        // Shuffled: the saved play order.
        q.shuffle_order = Some(vec![1, 4, 0, 3, 2]);
        let p = preview(&q);
        assert_eq!(ids(&p.context), ["e", "a", "c"]);
        // A broken order falls back to the list's.
        q.shuffle_order = Some(vec![1, 1]);
        assert_eq!(ids(&preview(&q).context), ["c", "e"]);
        assert_eq!(preview(&SavedQueue::default()), TabPreview::default());
        // The playing tab has no preview.
        let tabs = QueueTabs::default();
        assert_eq!(tabs.preview(0), None);
    }
}
