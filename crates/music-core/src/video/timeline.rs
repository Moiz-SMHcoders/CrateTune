//! Which moment of the video belongs to the moment of the audio playing.
//!
//! A track that is a video plays its own audio: the times are the same. A song
//! shown with its music video (the counterpart YouTube Music pairs it with)
//! is not: the video may open with an intro, break in the middle for a scene
//! without the song, or run on after it. YouTube sends the stretches the two
//! share (`segmentMap`: start in the song, start in the video, length); the
//! idea of following them comes from Limusic (GPL-3.0, issue #535 there).
//! Outside every stretch the video has nothing that matches the song, and the
//! artwork is shown instead.

use std::time::Duration;

/// A stretch played by both the song and its video.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SharedStretch {
    /// Where it starts in the song (the audio).
    pub audio: Duration,
    /// Where it starts in the video.
    pub video: Duration,
    pub length: Duration,
}

/// A gap this short between two stretches (YouTube's are often 1 ms apart) is
/// bridged: the picture keeps going instead of flashing the artwork.
const BRIDGE: Duration = Duration::from_millis(1500);

/// Maps the audio's position to the video's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TimeMap {
    /// The video is the track playing.
    #[default]
    Same,
    /// The song's music video, through the stretches they share (sorted by
    /// [`SharedStretch::audio`]). Empty: YouTube sent none, the two are
    /// assumed to line up.
    Stretches(Vec<SharedStretch>),
}

impl TimeMap {
    pub fn stretches(mut stretches: Vec<SharedStretch>) -> Self {
        stretches.retain(|s| !s.length.is_zero());
        stretches.sort_by_key(|s| s.audio);
        Self::Stretches(stretches)
    }

    /// The video time to show at `audio`, or `None` when the video has no
    /// matching picture there.
    pub fn video_time(&self, audio: Duration) -> Option<Duration> {
        let stretches = match self {
            Self::Same => return Some(audio),
            Self::Stretches(s) if s.is_empty() => return Some(audio),
            Self::Stretches(s) => s,
        };
        // The last stretch starting at or before `audio`.
        let i = stretches.partition_point(|s| s.audio <= audio);
        let Some(s) = i.checked_sub(1).map(|i| stretches[i]) else {
            // Before the first stretch: a short lead-in is bridged.
            let first = stretches[0];
            return (first.audio - audio <= BRIDGE).then_some(first.video);
        };
        let into = audio - s.audio;
        if into < s.length {
            return Some(s.video + into);
        }
        // Past this stretch: hold its last picture for a short gap.
        let gap_end = stretches.get(i).map(|next| next.audio);
        let over = into - s.length;
        let bridged = match gap_end {
            Some(next) => next - (s.audio + s.length) <= BRIDGE,
            None => over <= BRIDGE,
        };
        bridged.then(|| s.video + s.length.saturating_sub(Duration::from_millis(1)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    /// The "Locked Away" pair Limusic recorded: the song is the primary, the
    /// video opens 22.7 s earlier and breaks for 14.5 s in the middle.
    fn locked_away() -> TimeMap {
        TimeMap::stretches(vec![
            SharedStretch {
                audio: ms(86_439),
                video: ms(123_644),
                length: ms(141_061),
            },
            SharedStretch {
                audio: ms(0),
                video: ms(22_698),
                length: ms(86_438),
            },
        ])
    }

    #[test]
    fn maps_through_the_shared_stretches() {
        let map = locked_away();
        assert_eq!(map.video_time(ms(0)), Some(ms(22_698)));
        assert_eq!(map.video_time(ms(60_000)), Some(ms(82_698)));
        // The 1 ms seam between the two stretches is bridged.
        assert_eq!(map.video_time(ms(86_438)), Some(ms(22_698 + 86_437)));
        assert_eq!(map.video_time(ms(86_439)), Some(ms(123_644)));
        assert_eq!(map.video_time(ms(100_000)), Some(ms(137_205)));
        // The song's end, then a little past it (held), then well past.
        assert_eq!(map.video_time(ms(227_499)), Some(ms(264_704)));
        assert!(map.video_time(ms(228_000)).is_some());
        assert_eq!(map.video_time(ms(240_000)), None);
    }

    #[test]
    fn gaps_and_lead_ins_show_nothing_unless_short() {
        let map = TimeMap::stretches(vec![
            SharedStretch {
                audio: ms(5_000),
                video: ms(0),
                length: ms(10_000),
            },
            SharedStretch {
                audio: ms(20_000),
                video: ms(12_000),
                length: ms(10_000),
            },
        ]);
        assert_eq!(map.video_time(ms(0)), None);
        assert_eq!(map.video_time(ms(4_000)), Some(ms(0)));
        assert_eq!(map.video_time(ms(16_000)), None);
        assert_eq!(map.video_time(ms(21_000)), Some(ms(13_000)));
        assert_eq!(TimeMap::Same.video_time(ms(7)), Some(ms(7)));
        assert_eq!(
            TimeMap::stretches(Vec::new()).video_time(ms(7)),
            Some(ms(7))
        );
    }
}
