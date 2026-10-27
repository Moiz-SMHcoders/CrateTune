//! Play queue: the search results a track was started from, and where we are in them.
//!
//! Pure bookkeeping (no GPUI, no player), so the navigation rules are unit-tested:
//!
//! * clicking a result plays it with the whole result list as the queue;
//! * "next" (button, media key, or the track ending) moves one forward and
//!   stops at the end of the list;
//! * "previous" restarts the current track once it has played for more than
//!   [`RESTART_THRESHOLD`], otherwise goes one back (restarting the first track).

use std::rc::Rc;
use std::time::Duration;

use music_core::Track;

/// "Previous" restarts the current track instead when it is past this position.
pub const RESTART_THRESHOLD: Duration = Duration::from_secs(3);

/// What "previous" should do.
#[derive(Debug, Clone, PartialEq)]
pub enum Previous {
    /// Seek the current track back to 0.
    Restart,
    Play(Track),
}

#[derive(Default)]
pub struct Queue {
    tracks: Rc<Vec<Track>>,
    index: Option<usize>,
}

impl Queue {
    /// Starts playing `tracks[index]` with `tracks` as the context.
    pub fn replace(&mut self, tracks: Rc<Vec<Track>>, index: usize) -> Option<Track> {
        let track = tracks.get(index)?.clone();
        self.tracks = tracks;
        self.index = Some(index);
        Some(track)
    }

    pub fn current(&self) -> Option<&Track> {
        self.tracks.get(self.index?)
    }

    /// The track auto-advance / "next" would play.
    pub fn peek_next(&self) -> Option<&Track> {
        self.tracks.get(self.index? + 1)
    }

    pub fn has_next(&self) -> bool {
        self.peek_next().is_some()
    }

    /// Moves to the next track; `None` (and no change) at the end of the queue.
    pub fn advance(&mut self) -> Option<Track> {
        let next = self.index? + 1;
        let track = self.tracks.get(next)?.clone();
        self.index = Some(next);
        Some(track)
    }

    /// "Previous" at `position` into the current track.
    pub fn previous(&mut self, position: Duration) -> Previous {
        let Some(index) = self.index else {
            return Previous::Restart;
        };
        if position > RESTART_THRESHOLD || index == 0 {
            return Previous::Restart;
        }
        self.index = Some(index - 1);
        Previous::Play(self.tracks[index - 1].clone())
    }

    /// Whether `video_id` is the queue's current track (e.g. the one that just ended).
    pub fn is_current(&self, video_id: &str) -> bool {
        self.current().is_some_and(|t| t.video_id == video_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracks(n: usize) -> Rc<Vec<Track>> {
        Rc::new(
            (0..n)
                .map(|i| Track {
                    video_id: format!("v{i}"),
                    title: format!("Song {i}"),
                    artists: "Artist".into(),
                    album: None,
                    duration_secs: Some(200),
                    thumbnail_url: None,
                })
                .collect(),
        )
    }

    fn id(track: Option<Track>) -> Option<String> {
        track.map(|t| t.video_id)
    }

    #[test]
    fn empty_queue_does_nothing() {
        let mut q = Queue::default();
        assert!(q.current().is_none());
        assert!(!q.has_next());
        assert_eq!(q.advance(), None);
        assert_eq!(q.previous(Duration::ZERO), Previous::Restart);
        assert!(!q.is_current("v0"));
    }

    #[test]
    fn replace_plays_the_clicked_result_in_context() {
        let mut q = Queue::default();
        assert_eq!(id(q.replace(tracks(5), 2)), Some("v2".into()));
        assert!(q.is_current("v2"));
        assert_eq!(q.peek_next().map(|t| t.video_id.as_str()), Some("v3"));
        // Out of range: nothing changes.
        assert_eq!(q.replace(tracks(2), 7), None);
        assert!(q.is_current("v2"));
    }

    #[test]
    fn next_walks_forward_and_stops_at_the_end() {
        let mut q = Queue::default();
        q.replace(tracks(3), 1);
        assert_eq!(id(q.advance()), Some("v2".into()));
        assert!(!q.has_next());
        assert_eq!(q.advance(), None);
        assert!(q.is_current("v2"));
    }

    #[test]
    fn previous_restarts_after_three_seconds() {
        let mut q = Queue::default();
        q.replace(tracks(3), 2);
        assert_eq!(q.previous(Duration::from_millis(3_001)), Previous::Restart);
        assert!(q.is_current("v2"));
        // Exactly at the threshold still goes back.
        assert_eq!(
            q.previous(RESTART_THRESHOLD),
            Previous::Play(tracks(3)[1].clone())
        );
        assert_eq!(
            q.previous(Duration::from_secs(1)),
            Previous::Play(tracks(3)[0].clone())
        );
        // The first track can only restart.
        assert_eq!(q.previous(Duration::ZERO), Previous::Restart);
        assert!(q.is_current("v0"));
    }

    #[test]
    fn a_new_search_does_not_disturb_the_queue_until_a_click() {
        let mut q = Queue::default();
        let first = tracks(4);
        q.replace(first, 0);
        // The results list is replaced elsewhere; the queue keeps its own Rc.
        assert_eq!(id(q.advance()), Some("v1".into()));
    }
}
