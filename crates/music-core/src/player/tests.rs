//! End-to-end tests of the engine without an output device: the
//! [`HeadlessOutput`] is pulled by the test (standing in for the audio
//! callback) a few times faster than real time.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::*;
use crate::testutil::{Reply, TestServer, range_of};
use crate::{ResolvedStream, TrackDecoder};

const RATE: u32 = 48_000;
const CHIRP: &[u8] = include_bytes!("../../tests/fixtures/chirp_frag.m4a");
const PREVIEW_LEN: usize = 50_000;

/// Resolves ids against a local server: `full` (the chirp), `preview` (the
/// chirp cut off after 50 kB with 403s afterwards), anything else fails.
struct FakeResolver {
    full: TestServer,
    preview: TestServer,
}

impl FakeResolver {
    fn new() -> Arc<Self> {
        let full = TestServer::file(CHIRP.into());
        let preview = TestServer::start(|_, start, end| {
            if start > 0 {
                Reply::Status(403)
            } else {
                range_of(&CHIRP[..PREVIEW_LEN], start, end)
            }
        });
        Arc::new(Self { full, preview })
    }
}

impl StreamResolver for FakeResolver {
    fn resolve(&self, video_id: &str) -> Result<ResolvedStream> {
        let base = match video_id {
            "full" => &self.full.base,
            "preview" => &self.preview.base,
            other => return Err(Error::Unplayable(format!("{other}: video unavailable"))),
        };
        Ok(ResolvedStream {
            url: format!("{base}/{video_id}"),
            itag: 140,
            mime_type: "audio/mp4".into(),
            content_length: Some(CHIRP.len() as u64),
            user_agent: "test",
            loudness_db: Some(3.0),
        })
    }
}

fn remote(id: &str) -> Track {
    Track {
        video_id: id.into(),
        title: id.into(),
        duration_secs: Some(20),
        ..Default::default()
    }
}

fn local(name: &str) -> Track {
    Track {
        video_id: format!("local:{name}"),
        title: name.into(),
        local_path: Some(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name),
        ),
        ..Default::default()
    }
}

fn player() -> (Player, HeadlessOutput, Arc<FakeResolver>) {
    let resolver = FakeResolver::new();
    let (player, output) = Player::headless(resolver.clone(), RATE).unwrap();
    (player, output, resolver)
}

fn title(s: &PlaybackState) -> &str {
    s.track.as_ref().map_or("", |t| t.title.as_str())
}

/// Polls without pulling audio (the deck is idle, voices fill their queues).
fn wait_for(player: &Player, what: &str, cond: impl Fn(&PlaybackState) -> bool) -> PlaybackState {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let s = player.state();
        if cond(&s) {
            return s;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {s:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Output (left channel) and every state seen while pulling.
struct Run {
    audio: Vec<f32>,
    states: Vec<PlaybackState>,
}

/// Pulls audio (~3x real time) until `cond` holds.
fn pull_until(
    player: &Player,
    output: &mut HeadlessOutput,
    what: &str,
    cond: impl Fn(&PlaybackState) -> bool,
) -> Run {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut run = Run {
        audio: Vec::new(),
        states: Vec::new(),
    };
    loop {
        let s = player.state();
        let done = cond(&s);
        run.states.push(s);
        if done {
            return run;
        }
        assert!(
            Instant::now() < deadline,
            "timed out pulling until {what}: {:?}",
            run.states.last()
        );
        run.audio.extend(output.by_ref().take(480 * 2).step_by(2));
        std::thread::sleep(Duration::from_millis(3));
    }
}

fn is_sound(s: f32) -> bool {
    s.abs() > 1e-4
}

/// Index range from the first to the last audible sample.
fn sound_span(audio: &[f32]) -> (usize, usize) {
    let first = audio.iter().position(|&s| is_sound(s)).expect("no sound");
    let last = audio.iter().rposition(|&s| is_sound(s)).unwrap();
    (first, last)
}

fn longest_silence(audio: &[f32]) -> usize {
    let (mut best, mut run) = (0, 0);
    for &s in audio {
        run = if is_sound(s) { 0 } else { run + 1 };
        best = best.max(run);
    }
    best
}

fn zero_crossing_hz(audio: &[f32]) -> f64 {
    let crossings = audio
        .windows(2)
        .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
        .count();
    crossings as f64 / 2.0 / (audio.len() as f64 / RATE as f64)
}

fn secs(frames: usize) -> f64 {
    frames as f64 / RATE as f64
}

/// Where the preview's audio ends, as the engine sees it.
fn preview_end() -> Duration {
    let buffer = crate::StreamBuffer::new(Some(CHIRP.len() as u64));
    buffer.append(&CHIRP[..PREVIEW_LEN]);
    buffer.finish(crate::BufferEnd::Truncated);
    TrackDecoder::from_stream(buffer, "audio/mp4")
        .unwrap()
        .scan_end()
        .unwrap()
}

#[test]
fn plays_a_local_file_and_ends() {
    let (player, mut out, _r) = player();
    player.play(local("sine.mp3"));
    let run = pull_until(&player, &mut out, "end", |s| {
        s.status == PlaybackStatus::Ended
    });
    let (first, last) = sound_span(&run.audio);
    assert!(
        (secs(last - first) - 1.0).abs() < 0.06,
        "{}",
        secs(last - first)
    );
    let s = player.state();
    assert_eq!(title(&s), "sine.mp3");
    assert!(s.duration.unwrap().abs_diff(Duration::from_secs(1)) < Duration::from_millis(60));
    assert!(s.position > Duration::from_millis(900));
    assert!(!s.preview_only && s.error_count == 0 && s.advance_count == 0);
    assert!(
        run.states
            .iter()
            .any(|s| s.status == PlaybackStatus::Playing)
    );
}

#[test]
fn gapless_transition_to_the_preloaded_next_track() {
    let (player, mut out, _r) = player();
    player.play(local("a440.wav"));
    player.set_next(Some(local("b880.wav")));
    wait_for(&player, "preload", |s| s.next_preloaded);
    let run = pull_until(&player, &mut out, "end of b", |s| {
        s.advance_count == 1 && s.status == PlaybackStatus::Ended
    });

    // Order seen by a polling UI: a440, then b880 together with advance_count 1.
    let first_b = run
        .states
        .iter()
        .position(|s| title(s) == "b880.wav")
        .unwrap();
    assert!(
        run.states[..first_b]
            .iter()
            .all(|s| s.advance_count == 0 && title(s) == "a440.wav")
    );
    assert!(
        run.states[first_b..]
            .iter()
            .all(|s| s.advance_count == 1 && s.next_track.is_none())
    );

    // 0.5 s + 0.5 s of audio with no gap in between.
    let (first, last) = sound_span(&run.audio);
    let audio = &run.audio[first..=last];
    assert!(
        (secs(audio.len()) - 1.0).abs() < 0.005,
        "{}",
        secs(audio.len())
    );
    assert!(
        longest_silence(audio) <= 2,
        "gap of {} frames",
        longest_silence(audio)
    );
    let half = RATE as usize / 2;
    let a_hz = zero_crossing_hz(&audio[1000..half - 1000]);
    let b_hz = zero_crossing_hz(&audio[half + 1000..audio.len() - 1000]);
    assert!(
        (a_hz - 440.0).abs() < 5.0 && (b_hz - 880.0).abs() < 5.0,
        "{a_hz} {b_hz}"
    );
}

#[test]
fn preview_streams_progressively_and_ends_cleanly() {
    let (player, mut out, _r) = player();
    player.play(remote("preview"));
    let run = pull_until(&player, &mut out, "end", |s| {
        s.status == PlaybackStatus::Ended
    });
    let s = player.state();
    assert!(s.preview_only);
    let end = preview_end();
    assert!(
        s.position.abs_diff(end) < Duration::from_millis(150),
        "{:?} vs {end:?}",
        s.position
    );
    // The container still claims the full 20 s.
    assert!(s.duration.unwrap() > Duration::from_secs(19));
    assert!(
        run.states
            .iter()
            .any(|s| s.status == PlaybackStatus::Playing)
    );
    assert_eq!(s.error_count, 0);
}

#[test]
fn crossfade_starts_before_a_preview_ends() {
    let (player, mut out, _r) = player();
    player.set_crossfade(Duration::from_secs(2));
    player.play(remote("preview"));
    player.set_next(Some(local("sine.mp3")));
    wait_for(&player, "preview detected + next preloaded", |s| {
        s.preview_only && s.next_preloaded && s.status == PlaybackStatus::Playing
    });
    // Give the voice a moment to find where the preview's audio ends.
    std::thread::sleep(Duration::from_millis(100));
    let run = pull_until(&player, &mut out, "end of next", |s| {
        s.advance_count == 1 && s.status == PlaybackStatus::Ended
    });
    let end = preview_end();
    let last_preview_pos = run
        .states
        .iter()
        .filter(|s| s.advance_count == 0)
        .map(|s| s.position)
        .max()
        .unwrap();
    // The switch happened ~2.15 s before the preview's data ran out.
    assert!(
        last_preview_pos < end - Duration::from_millis(1500),
        "{last_preview_pos:?} vs end {end:?}"
    );
    assert!(
        run.states.iter().any(|s| s.crossfading),
        "crossfade observed"
    );
    let s = player.state();
    assert_eq!(title(&s), "sine.mp3");
    assert!(!s.preview_only);
}

#[test]
fn failed_track_is_reported_and_auto_skipped() {
    let (player, mut out, _r) = player();
    player.play(remote("gone"));
    let s = wait_for(&player, "error", |s| {
        matches!(s.status, PlaybackStatus::Error(_))
    });
    assert_eq!(
        (s.error_count, s.failed_track_id.as_deref()),
        (1, Some("gone"))
    );

    // With auto-skip, the next track takes over (also when set after the failure).
    player.set_auto_skip(true);
    player.set_next(Some(local("a440.wav")));
    let s = wait_for(&player, "skip", |s| s.advance_count == 1);
    assert_eq!(title(&s), "a440.wav");
    pull_until(&player, &mut out, "playing", |s| {
        s.status == PlaybackStatus::Playing
    });

    // A failing track with the next one already queued is skipped the same way.
    player.play(remote("gone-too"));
    player.set_next(Some(local("b880.wav")));
    let s = wait_for(&player, "second skip", |s| s.advance_count == 2);
    assert_eq!(title(&s), "b880.wav");
    assert_eq!(
        (s.error_count, s.failed_track_id.as_deref()),
        (2, Some("gone-too"))
    );
}

#[test]
fn sleep_timer_pauses_at_the_end_of_the_track() {
    let (player, mut out, _r) = player();
    player.play(local("a440.wav"));
    player.set_next(Some(local("b880.wav")));
    player.set_sleep_timer(Some(SleepTimer::EndOfTrack));
    let s = wait_for(&player, "preload", |s| s.next_preloaded);
    assert_eq!(
        s.sleep_timer,
        Some(SleepTimerState {
            remaining: None,
            end_of_track: true
        })
    );
    let run = pull_until(&player, &mut out, "paused on next", |s| {
        s.advance_count == 1 && s.status == PlaybackStatus::Paused
    });
    let s = player.state();
    assert_eq!(title(&s), "b880.wav");
    assert!(s.position < Duration::from_millis(20));
    assert!(s.sleep_timer.is_none());
    let (first, last) = sound_span(&run.audio);
    assert!(
        (secs(last - first) - 0.5).abs() < 0.01,
        "only the first track played"
    );

    // A duration timer pauses mid-track.
    player.resume();
    player.set_sleep_timer(Some(SleepTimer::After(Duration::from_millis(150))));
    let s = wait_for(&player, "countdown", |s| s.sleep_timer.is_some());
    assert!(s.sleep_timer.unwrap().remaining.unwrap() <= Duration::from_millis(150));
    let s = wait_for(&player, "sleep", |s| s.status == PlaybackStatus::Paused);
    assert!(s.sleep_timer.is_none());
}

#[test]
fn speed_and_pitch_change_the_output() {
    let (player, mut out, _r) = player();
    player.set_speed(2.0);
    player.play(local("a440.wav"));
    let run = pull_until(&player, &mut out, "end", |s| {
        s.status == PlaybackStatus::Ended
    });
    let (first, last) = sound_span(&run.audio);
    assert!(
        (secs(last - first) - 0.25).abs() < 0.03,
        "{}",
        secs(last - first)
    );
    let hz = zero_crossing_hz(&run.audio[first + 1000..last - 1000]);
    assert!((hz - 440.0).abs() < 15.0, "{hz}");
    assert_eq!(player.state().speed, 2.0);

    player.set_speed(1.0);
    player.set_pitch(12.0);
    player.play(local("a440.wav"));
    // The previous track's `Ended` may still be visible until `play` is handled.
    wait_for(&player, "restart", |s| s.status != PlaybackStatus::Ended);
    let run = pull_until(&player, &mut out, "end", |s| {
        s.status == PlaybackStatus::Ended
    });
    assert_eq!(player.state().pitch_semitones, 12.0);
    let (first, last) = sound_span(&run.audio);
    assert!(
        (secs(last - first) - 0.5).abs() < 0.03,
        "{}",
        secs(last - first)
    );
    let hz = zero_crossing_hz(&run.audio[first + 1000..last - 1000]);
    assert!((hz - 880.0).abs() < 20.0, "{hz}");
}

#[test]
fn streams_seek_pause_and_replay() {
    let (player, mut out, _r) = player();
    player.play(remote("full"));
    pull_until(&player, &mut out, "1 s in", |s| {
        s.position > Duration::from_secs(1)
    });
    assert!(!player.state().preview_only);
    player.seek(Duration::from_secs(15));
    let s = pull_until(&player, &mut out, "after seek", |s| {
        s.position >= Duration::from_secs(15)
    })
    .states
    .pop()
    .unwrap();
    assert!(s.position < Duration::from_secs(16));
    pull_until(&player, &mut out, "end", |s| {
        s.status == PlaybackStatus::Ended
    });
    let s = player.state();
    assert!(
        s.position > Duration::from_millis(19_800),
        "{:?}",
        s.position
    );

    // Resume after the end replays from the start.
    player.resume();
    let s = pull_until(&player, &mut out, "replay", |s| {
        s.status == PlaybackStatus::Playing && s.position > Duration::ZERO
    })
    .states
    .pop()
    .unwrap();
    assert!(s.position < Duration::from_secs(2));

    // Pausing silences the output and holds the position.
    player.pause();
    let paused = wait_for(&player, "paused", |s| s.status == PlaybackStatus::Paused).position;
    let audible = out
        .by_ref()
        .take(RATE as usize * 2)
        .filter(|s| is_sound(*s))
        .count();
    assert!(
        audible < 2 * 2 * 1024,
        "{audible} audible samples while paused"
    );
    std::thread::sleep(Duration::from_millis(150));
    assert!(player.state().position.abs_diff(paused) < Duration::from_millis(40));
}

#[test]
fn preloaded_next_track_starts_instantly_when_played() {
    let (player, _out, _r) = player();
    player.play(local("a440.wav"));
    player.set_next(Some(local("b880.wav")));
    wait_for(&player, "preload", |s| s.next_preloaded);
    player.play(local("b880.wav"));
    let s = wait_for(&player, "switch", |s| title(s) == "b880.wav");
    assert_eq!(s.advance_count, 0, "a user switch is not an advance");
    let s = wait_for(&player, "playing", |s| s.status == PlaybackStatus::Playing);
    assert!(s.next_track.is_none() && !s.next_preloaded);
}
