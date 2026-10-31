//! [`VideoPlayer`]: one video, fetched fragment by fragment and decoded a few
//! frames ahead on a worker thread; the UI takes the frame for the audio
//! clock's position each time it draws ([`VideoPlayer::present`]).
//!
//! The worker only runs while the queue has room: paused, it decodes up to
//! the budget and sleeps. Dropping the player stops it (between two steps;
//! a fetch in flight finishes first, its result is thrown away).

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use super::decode::{self, Picture};
use super::fetch::{RangeFetcher, Refresh};
use super::mp4::{InitSegment, Sample, SegmentIndex, parse_fragment};
use super::sync::{self, Choice, DecoderPosition};
use super::timeline::TimeMap;
use crate::{Error, ResolvedStream, Result};

/// Decoded frames held ahead at most (and at least, whatever their size).
const MAX_AHEAD: usize = 8;
const MIN_AHEAD: usize = 3;
/// Memory budget of the frames held ahead.
const AHEAD_BYTES: usize = 16 << 20;
/// Frames closer together than this are thinned out (at most ~30 fps).
pub const MIN_FRAME_GAP: Duration = Duration::from_millis(30);
/// While the queue is empty and the clock runs, look again this soon.
const POLL: Duration = Duration::from_millis(50);

/// A decoded frame ready to show.
#[derive(Debug)]
pub struct VideoFrame {
    /// Increases with every frame of a player.
    pub id: u64,
    pub pts: Duration,
    pub picture: Picture,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoStatus {
    /// Fetching the index or the first fragment.
    Loading,
    /// Frames are coming.
    Ready,
    /// Gave up (the reason is for the log and `@video-status`).
    Failed(String),
}

/// What [`VideoPlayer::present`] decided for one redraw.
#[derive(Debug, Default)]
pub struct Presented {
    /// A new frame for the screen (the previous one can go).
    pub frame: Option<VideoFrame>,
    /// The video has a picture for this moment of the song (`false`: show the
    /// artwork, e.g. during a stretch the music video doesn't share).
    pub visible: bool,
    /// Draw again after this long (the next frame), while the clock runs.
    pub next_in: Option<Duration>,
}

/// Counters for `@video-status` and the log.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VideoStats {
    pub shown: u64,
    /// Decoded, but late by the time they were due.
    pub dropped: u64,
    /// Decoder repositions (seeks, jumps).
    pub seeks: u64,
}

pub struct VideoPlayer {
    shared: Arc<Shared>,
    map: TimeMap,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

#[derive(Default)]
struct State {
    /// Ascending presentation times.
    frames: VecDeque<VideoFrame>,
    bytes: usize,
    capacity: Option<usize>,
    /// A reposition for the worker to make.
    seek: Option<Duration>,
    /// The last reposition, until its first frame arrived.
    seeking_to: Option<Duration>,
    /// Bumped by every reposition: frames decoded for an older one are dropped.
    generation: u64,
    decoded_to: Option<Duration>,
    /// The frame on screen: nothing older is queued after it.
    shown: Option<Duration>,
    ended: bool,
    status: Option<VideoStatus>,
    stop: bool,
    stats: VideoStats,
    next_id: u64,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl State {
    fn request_seek(&mut self, to: Duration) {
        self.frames.clear();
        self.bytes = 0;
        self.seek = Some(to);
        self.seeking_to = Some(to);
        self.decoded_to = None;
        self.shown = None;
        self.ended = false;
        self.generation += 1;
        self.stats.seeks += 1;
    }

    fn full(&self) -> bool {
        let cap = self.capacity.unwrap_or(MAX_AHEAD);
        self.frames.len() >= cap
    }
}

impl VideoPlayer {
    /// Starts on `stream` (from [`crate::youtube::WebResolver::resolve_video`]) at
    /// the video time for `audio_position`; `refresh` resolves the same video
    /// again when its URL stops working.
    pub fn start(
        stream: ResolvedStream,
        refresh: Option<Refresh>,
        map: TimeMap,
        audio_position: Duration,
    ) -> Result<Self> {
        let format = stream
            .video
            .clone()
            .ok_or_else(|| Error::Unplayable("not a video stream".into()))?;
        let mut state = State {
            status: Some(VideoStatus::Loading),
            ..State::default()
        };
        state.request_seek(map.video_time(audio_position).unwrap_or_default());
        state.stats.seeks = 0;
        let shared = Arc::new(Shared {
            state: Mutex::new(state),
            wake: Condvar::new(),
        });
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("music-core-video".into())
            .spawn(move || {
                let fetcher = RangeFetcher::new(stream, refresh);
                if let Err(e) = run(&worker, &fetcher, &format) {
                    let mut state = worker.lock();
                    if !state.stop {
                        log::warn!("video: {e}");
                        state.status = Some(VideoStatus::Failed(e.to_string()));
                    }
                }
            })
            .map_err(|e| Error::Unplayable(format!("video thread: {e}")))?;
        Ok(Self { shared, map })
    }

    /// The frame for the audio at `audio_position` (see [`Presented`]).
    /// `playing`: the clock runs at `speed` (otherwise no next frame is due).
    pub fn present(&self, audio_position: Duration, playing: bool, speed: f32) -> Presented {
        let Some(clock) = self.map.video_time(audio_position) else {
            return Presented {
                frame: None,
                visible: false,
                next_in: playing.then_some(Duration::from_millis(250)),
            };
        };
        let mut state = self.shared.lock();
        let pending: Vec<Duration> = state.frames.iter().map(|f| f.pts).collect();
        let decoder = DecoderPosition {
            decoded_to: state.decoded_to,
            seeking_to: state.seeking_to,
            ended: state.ended,
        };
        let mut frame = None;
        match sync::choose(&pending, clock, decoder) {
            Choice::Show { index } => {
                for _ in 0..index {
                    if let Some(late) = state.frames.pop_front() {
                        state.bytes = state.bytes.saturating_sub(late.picture.bytes());
                        state.stats.dropped += 1;
                    }
                }
                frame = state.frames.pop_front();
                if let Some(f) = &frame {
                    state.bytes = state.bytes.saturating_sub(f.picture.bytes());
                    state.stats.shown += 1;
                    state.shown = Some(f.pts);
                }
                self.shared.wake.notify_all();
            }
            Choice::Keep => {}
            Choice::Seek(to) => {
                log::debug!("video: repositioning at {to:?}");
                state.request_seek(to);
                self.shared.wake.notify_all();
            }
        }
        let failed = matches!(state.status, Some(VideoStatus::Failed(_)));
        let next_in = if playing {
            let pending: Vec<Duration> = state.frames.iter().map(|f| f.pts).collect();
            Some(sync::next_due(&pending, clock, speed, MIN_FRAME_GAP).unwrap_or(POLL))
        } else {
            // Paused: only while the frame of this moment is still on its way
            // (the start, or a seek made while paused).
            (state.seeking_to.is_some() || state.shown.is_none()).then_some(POLL)
        };
        let next_in = next_in.filter(|_| !failed);
        Presented {
            frame,
            visible: true,
            next_in,
        }
    }

    pub fn status(&self) -> VideoStatus {
        self.shared
            .lock()
            .status
            .clone()
            .unwrap_or(VideoStatus::Loading)
    }

    pub fn stats(&self) -> VideoStats {
        self.shared.lock().stats
    }
}

impl Drop for VideoPlayer {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.stop = true;
        state.frames.clear();
        self.shared.wake.notify_all();
    }
}

/// The worker: index, then fragments and frames as the queue makes room.
fn run(shared: &Shared, fetcher: &RangeFetcher, format: &super::VideoFormat) -> Result<()> {
    let cancelled = || shared.lock().stop;
    let (a, b) = format.init_range;
    let init = InitSegment::parse(&fetcher.range(a, b, &cancelled)?)?;
    let (a, b) = format.index_range;
    let index = SegmentIndex::parse(&fetcher.range(a, b, &cancelled)?, a)?;
    let mut decoder = decode::open(&init)?;
    log::info!(
        "video: {}×{} via {}, {} fragments",
        init.width,
        init.height,
        decode::decoder_name(),
        index.fragments.len()
    );

    let mut next_fragment = 0usize;
    let mut samples: VecDeque<Sample> = VecDeque::new();
    let mut skip_before = Duration::ZERO;
    let mut last_kept: Option<Duration> = None;
    let mut generation = 0u64;
    let mut ahead: Option<(usize, std::thread::JoinHandle<Result<Vec<u8>>>)> = None;
    loop {
        {
            let mut state = shared.lock();
            while !state.stop && state.seek.is_none() && (state.ended || state.full()) {
                state = shared
                    .wake
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            if state.stop {
                return Ok(());
            }
            if let Some(to) = state.seek.take() {
                generation = state.generation;
                drop(state);
                decoder.flush();
                samples.clear();
                next_fragment = index.find(to).unwrap_or(0);
                skip_before = to;
                last_kept = None;
            }
        }
        if samples.is_empty() {
            if next_fragment >= index.fragments.len() {
                let rest = decoder.finish()?;
                deliver(shared, generation, rest, skip_before, &mut last_kept);
                shared.lock().ended = true;
                continue;
            }
            let fragment = index.fragments[next_fragment];
            let data = match ahead.take() {
                Some((i, handle)) if i == next_fragment => handle
                    .join()
                    .map_err(|_| Error::Unplayable("video fetch thread".into()))??,
                _ => fetcher.range(fragment.start, fragment.end, &cancelled)?,
            };
            // Fetch the next fragment meanwhile: decoding this one takes
            // seconds of video, fetching the next a fraction of a second.
            if let Some(next) = index.fragments.get(next_fragment + 1).copied() {
                let (fetcher, i) = (fetcher.clone(), next_fragment + 1);
                ahead = std::thread::Builder::new()
                    .name("music-core-video-fetch".into())
                    .spawn(move || fetcher.range(next.start, next.end, &|| false))
                    .ok()
                    .map(|h| (i, h));
            }
            samples = parse_fragment(&data, &init)?.into();
            next_fragment += 1;
        }
        let Some(sample) = samples.pop_front() else {
            continue;
        };
        let pictures = decoder.decode(&sample)?;
        deliver(shared, generation, pictures, skip_before, &mut last_kept);
    }
}

/// Queues decoded pictures (sorted by time), except ones before the
/// reposition target and ones the frame-rate cap thins out.
fn deliver(
    shared: &Shared,
    generation: u64,
    pictures: Vec<(Duration, Picture)>,
    skip_before: Duration,
    last_kept: &mut Option<Duration>,
) {
    if pictures.is_empty() {
        return;
    }
    let mut state = shared.lock();
    if state.generation != generation || state.stop {
        return;
    }
    for (pts, picture) in pictures {
        state.decoded_to = Some(state.decoded_to.map_or(pts, |d| d.max(pts)));
        // The frame showing at the target starts up to a frame before it.
        if pts + MIN_FRAME_GAP * 2 < skip_before {
            continue;
        }
        if last_kept.is_some_and(|last| pts > last && pts - last < MIN_FRAME_GAP)
            || state.shown.is_some_and(|shown| pts <= shown)
        {
            continue;
        }
        *last_kept = Some(last_kept.map_or(pts, |last| last.max(pts)));
        if state.capacity.is_none() {
            let per = picture.bytes().max(1);
            state.capacity = Some((AHEAD_BYTES / per).clamp(MIN_AHEAD, MAX_AHEAD));
        }
        state.bytes += picture.bytes();
        let id = state.next_id;
        state.next_id += 1;
        let at = state.frames.partition_point(|f| f.pts <= pts);
        state.frames.insert(at, VideoFrame { id, pts, picture });
        state.seeking_to = None;
        state.status = Some(VideoStatus::Ready);
    }
    shared.wake.notify_all();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestServer;
    use crate::video::VideoFormat;
    use crate::video::mp4::tests as clip;
    use std::time::Instant;

    fn clip_stream(server: &TestServer) -> ResolvedStream {
        ResolvedStream {
            url: format!("{}/clip.mp4", server.base),
            itag: 160,
            content_length: Some(clip::CLIP.len() as u64),
            video: Some(VideoFormat {
                width: 128,
                height: 72,
                fps: 10,
                codecs: "avc1.4d400b".into(),
                init_range: (0, clip::INIT_END as u64),
                index_range: (clip::INDEX.0 as u64, clip::INDEX.1 as u64),
                bitrate: None,
            }),
            ..Default::default()
        }
    }

    /// Asks for frames until one is shown (or 10 s pass).
    fn frame_at(player: &VideoPlayer, at: Duration) -> VideoFrame {
        let started = Instant::now();
        loop {
            if let Some(f) = player.present(at, false, 1.0).frame {
                return f;
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "no frame at {at:?}"
            );
            assert!(
                !matches!(player.status(), VideoStatus::Failed(_)),
                "{:?}",
                player.status()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Fetching, decoding, presenting and repositioning on the fixture clip,
    /// served over HTTP in byte ranges like googlevideo.
    #[test]
    fn plays_seeks_and_follows_the_clock() {
        if !decode::decoding_supported() {
            eprintln!("video: no H.264 decoder on this machine, skipped");
            return;
        }
        let server = TestServer::file(clip::CLIP.into());
        let player =
            VideoPlayer::start(clip_stream(&server), None, TimeMap::Same, Duration::ZERO).unwrap();
        let f = frame_at(&player, Duration::ZERO);
        assert_eq!(f.pts, Duration::ZERO);
        assert_eq!(f.picture.size(), (128, 72));

        // The clock moves on: the frame of that moment, earlier ones dropped.
        let f = frame_at(&player, Duration::from_millis(350));
        assert_eq!(f.pts, Duration::from_millis(300));
        assert!(player.stats().dropped >= 2, "{:?}", player.stats());

        // A seek forward into the last fragment, then back to the start.
        let f = frame_at(&player, Duration::from_millis(2_520));
        assert_eq!(f.pts, Duration::from_millis(2_500));
        let f = frame_at(&player, Duration::from_millis(1_010));
        assert_eq!(f.pts, Duration::from_millis(1_000));
        assert!(player.stats().seeks >= 2, "{:?}", player.stats());
        assert_eq!(player.status(), VideoStatus::Ready);

        // Paused: the same moment shows nothing new and asks for no redraw.
        let p = player.present(Duration::from_millis(1_010), false, 1.0);
        assert!(
            p.frame.is_none() && p.visible && p.next_in.is_none(),
            "{p:?}"
        );
        // Playing: the next frame is due in 90 ms (once it's decoded; until
        // then the redraw comes back sooner).
        let started = Instant::now();
        loop {
            let p = player.present(Duration::from_millis(1_010), true, 1.0);
            if p.next_in == Some(Duration::from_millis(90)) {
                break;
            }
            assert!(p.next_in <= Some(POLL), "{:?}", p.next_in);
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(5));
        }

        // Fragments were fetched by range: init, index, then fragments.
        let requests = server.requests.lock().unwrap().clone();
        assert_eq!(requests[0].1, 0);
        assert_eq!(requests[1].1, clip::INDEX.0 as u64);
        drop(player);
    }

    #[test]
    fn outside_the_shared_stretches_there_is_no_picture() {
        let server = TestServer::file(clip::CLIP.into());
        let map = TimeMap::stretches(vec![crate::video::SharedStretch {
            audio: Duration::from_secs(10),
            video: Duration::ZERO,
            length: Duration::from_secs(3),
        }]);
        let player = VideoPlayer::start(clip_stream(&server), None, map, Duration::ZERO).unwrap();
        let p = player.present(Duration::ZERO, true, 1.0);
        assert!(!p.visible && p.frame.is_none());
        if decode::decoding_supported() {
            let f = frame_at(&player, Duration::from_millis(12_520));
            assert_eq!(f.pts, Duration::from_millis(2_500));
        }
    }

    #[test]
    fn a_broken_stream_fails_cleanly() {
        let server = TestServer::start(|_, _, _| crate::testutil::Reply::Status(404));
        let player =
            VideoPlayer::start(clip_stream(&server), None, TimeMap::Same, Duration::ZERO).unwrap();
        let started = Instant::now();
        while player.status() == VideoStatus::Loading {
            assert!(started.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(matches!(player.status(), VideoStatus::Failed(_)));
        let p = player.present(Duration::ZERO, true, 1.0);
        assert!(p.frame.is_none() && p.next_in.is_none());
    }
}
