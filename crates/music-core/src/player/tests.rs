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
/// chirp cut off after 50 kB with 403s afterwards), `expired` / `expiring`
/// (module-like progressive streams whose URL already expired / drops after
/// 60 kB and then answers 403; [`StreamResolver::refresh_track`] hands out a
/// working one), anything else fails.
struct FakeResolver {
    full: TestServer,
    preview: TestServer,
    modules: TestServer,
    refreshes: std::sync::atomic::AtomicU32,
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
        let modules = TestServer::start(|path, start, end| {
            if path.starts_with("/stale") {
                if start > 0 || path.contains("expired") {
                    return Reply::Status(403);
                }
                return Reply::Partial {
                    body: CHIRP.to_vec(),
                    start: 0,
                    total: CHIRP.len() as u64,
                    sent: 60_000,
                };
            }
            range_of(CHIRP, start, end)
        });
        Arc::new(Self {
            full,
            preview,
            modules,
            refreshes: Default::default(),
        })
    }

    fn module_stream(&self, path: &str, valid_for: Duration, expired: bool) -> ResolvedStream {
        let now = std::time::SystemTime::now();
        ResolvedStream {
            url: format!("{}{path}", self.modules.base),
            mime_type: "audio/mp4".into(),
            progressive: true,
            full_access: true,
            source: Some(crate::StreamSource {
                kind: crate::SourceKind::Module,
                provider: "Test".into(),
                module_id: Some("test".into()),
                ..Default::default()
            }),
            valid_until: Some(if expired {
                now - valid_for
            } else {
                now + valid_for
            }),
            ..Default::default()
        }
    }
}

impl StreamResolver for FakeResolver {
    fn resolve(&self, video_id: &str) -> Result<ResolvedStream> {
        let hour = Duration::from_secs(3600);
        match video_id {
            "expired" => return Ok(self.module_stream("/stale/expired", hour, true)),
            "expiring" => return Ok(self.module_stream("/stale/expiring", hour, false)),
            _ => {}
        }
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
            ..Default::default()
        })
    }

    fn refresh_track(
        &self,
        track: &Track,
        _: Quality,
        stale: &ResolvedStream,
    ) -> Result<ResolvedStream> {
        assert!(stale.url.contains(&track.video_id));
        self.refreshes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(self.module_stream(
            &format!("/fresh/{}", track.video_id),
            Duration::from_secs(3600),
            false,
        ))
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
    pull_fast_until(player, output, 480, what, cond)
}

/// Like [`pull_until`], `frames` per ~3 ms.
fn pull_fast_until(
    player: &Player,
    output: &mut HeadlessOutput,
    frames: usize,
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
        run.audio
            .extend(output.by_ref().take(frames * 2).step_by(2));
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
fn gapless_aac_tracks_join_without_priming_or_padding() {
    // One chirp split into two AAC files at 0.6 s (not a frame boundary): each
    // has 1024 frames of encoder priming and a padded last frame, which the
    // edit list trims. Played back to back, they must reproduce the chirp.
    const AAC_RATE: u32 = 44_100;
    let (player, mut out) = Player::headless(FakeResolver::new(), AAC_RATE).unwrap();
    player.play(local("gapless_a.m4a"));
    player.set_next(Some(local("gapless_b.m4a")));
    wait_for(&player, "preload", |s| s.next_preloaded);
    let run = pull_until(&player, &mut out, "end of b", |s| {
        s.advance_count == 1 && s.status == PlaybackStatus::Ended
    });
    let a = player_track_duration(&run, "gapless_a.m4a");
    assert_eq!(a, Some(Duration::from_millis(600)));

    // Align the output with the source signal on the first track...
    let reference = |i: i64| {
        if i < 0 {
            0.0
        } else {
            crate::decode::tests::reference_chirp(i as u64)
        }
    };
    let start = run.audio.iter().position(|&s| is_sound(s)).unwrap();
    let from = start.saturating_sub(4);
    let (lag, corr) = crate::decode::tests::best_lag(&run.audio[from..from + 8192], reference, 16);
    assert!(corr > 0.99, "{corr}");
    let origin = from as i64 - lag;
    // ...then the whole 1.3 s, across the switch, must line up with it: a gap
    // or overlap of even a few frames at 0.6 s would shift the second half.
    let total = 57_330; // 0.6 s + 0.7 s at 44.1 kHz
    let audio = &run.audio[origin as usize..];
    assert!(audio.len() >= total, "{}", audio.len());
    let boundary = 26_460;
    let window = &audio[boundary - 4096..boundary + 4096];
    let (lag, corr) =
        crate::decode::tests::best_lag(window, |i| reference(i + boundary as i64 - 4096), 64);
    assert_eq!(
        lag, 0,
        "track b is shifted by {lag} frames (correlation {corr})"
    );
    assert!(corr > 0.99, "{corr}");
    let tail = &audio[total - 4096..total];
    let (lag, corr) =
        crate::decode::tests::best_lag(tail, |i| reference(i + total as i64 - 4096), 64);
    assert_eq!(
        lag, 0,
        "end of track b shifted by {lag} (correlation {corr})"
    );
    // Nothing inserted around the switch, and the audio stops right at 1.3 s.
    assert!(longest_silence(&audio[boundary - 2048..boundary + 2048]) <= 2);
    assert!(
        audio[total + 64..].iter().all(|s| !is_sound(*s)),
        "audio continues past the end of track b"
    );
}

/// The duration the player reported for `name` while it was current.
fn player_track_duration(run: &Run, name: &str) -> Option<Duration> {
    run.states
        .iter()
        .find(|s| title(s) == name && s.duration.is_some())
        .and_then(|s| s.duration)
}

#[test]
fn module_streams_recover_from_expired_urls_mid_stream_and_when_preloaded() {
    let (player, mut out, resolver) = player();
    // Its URL dies after 60 kB (of 330 kB): re-resolved, resumed at the same byte.
    player.play(remote("expiring"));
    // Preloaded with a URL that has expired already: re-resolved before use.
    player.set_next(Some(remote("expired")));
    let run = pull_fast_until(&player, &mut out, 2400, "end of both", |s| {
        s.advance_count == 1 && s.status == PlaybackStatus::Ended
    });
    assert!(
        run.states
            .iter()
            .all(|s| s.error_count == 0 && !s.preview_only)
    );
    assert_eq!(
        resolver.refreshes.load(std::sync::atomic::Ordering::SeqCst),
        2
    );
    // Two full 20 s chirps, back to back.
    let (first, last) = sound_span(&run.audio);
    assert!(
        (secs(last - first) - 40.0).abs() < 0.2,
        "{}",
        secs(last - first)
    );
    assert!(longest_silence(&run.audio[first..=last]) < RATE as usize / 10);
    let requests = resolver.modules.requests.lock().unwrap().clone();
    assert!(requests.contains(&("/stale/expiring".into(), 0)));
    assert!(
        requests.contains(&("/fresh/expiring".into(), 60_000)),
        "{requests:?}"
    );
    assert!(requests.contains(&("/fresh/expired".into(), 0)));
    assert!(!requests.iter().any(|(p, _)| p == "/stale/expired"));
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

#[test]
fn spectrum_follows_muted_playback_and_decays_when_paused() {
    use crate::{Spectrum, SpectrumConfig};

    let (player, mut out, _r) = player();
    let bands = 32;
    let edges = SpectrumConfig::default().band_edges(bands);
    let a440 = edges
        .windows(2)
        .position(|e| e[0] <= 440.0 && 440.0 < e[1])
        .unwrap();
    let mut spectrum = Spectrum::default();
    // Pulls ~one display frame of audio (≈ real time), then polls like a UI.
    let mut frame = |player: &Player, spectrum: &mut Spectrum| {
        out.by_ref().take(800 * 2).for_each(drop);
        std::thread::sleep(Duration::from_millis(16));
        player.spectrum_into(bands, spectrum);
    };

    // Disabled by default: nothing, even while playing.
    assert!(!player.spectrum_enabled());
    player.play(local("sine.mp3"));
    wait_for(&player, "loaded", |s| s.status != PlaybackStatus::Loading);
    for _ in 0..5 {
        frame(&player, &mut spectrum);
    }
    assert!(spectrum.is_silent() && spectrum.bands.len() == bands);

    // Enabled with the volume at 0: the tap is pre-volume.
    player.set_spectrum_enabled(true);
    player.set_volume(0.0);
    let mut lit = Spectrum::default();
    for _ in 0..20 {
        frame(&player, &mut spectrum);
        if spectrum.bands[a440] > lit.bands.get(a440).copied().unwrap_or(0.0) {
            lit = spectrum.clone();
        }
    }
    assert_eq!(player.state().status, PlaybackStatus::Playing);
    let top = (0..bands)
        .max_by(|&a, &b| lit.bands[a].total_cmp(&lit.bands[b]))
        .unwrap();
    assert_eq!(top, a440, "{:?}", lit.bands);
    assert!(lit.bands[a440] > 0.5, "{:?}", lit.bands);
    assert!(lit.level > 0.3, "level {}", lit.level);
    let highs = edges.windows(2).position(|e| e[0] > 3000.0).unwrap();
    assert!(
        lit.bands[highs..].iter().all(|&b| b < 0.1),
        "{:?}",
        lit.bands
    );

    // Paused: falls to exactly zero within a few seconds.
    player.pause();
    let paused_at = Instant::now();
    while !spectrum.is_silent() {
        assert!(
            paused_at.elapsed() < Duration::from_secs(5),
            "no decay: {spectrum:?}"
        );
        frame(&player, &mut spectrum);
    }
    assert!(
        player.state().position < Duration::from_millis(900),
        "paused mid-track"
    );
}

/// Pulls audio until the waveform status satisfies `cond`.
fn pull_until_waveform(
    player: &Player,
    output: &mut HeadlessOutput,
    what: &str,
    cond: impl Fn(&WaveformStatus) -> bool,
) -> WaveformStatus {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let w = player.waveform();
        if cond(&w) {
            return w;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {w:?}"
        );
        output.by_ref().take(480 * 2).for_each(drop);
        std::thread::sleep(Duration::from_millis(3));
    }
}

#[test]
fn waveform_is_computed_cached_and_follows_the_track() {
    let dir =
        std::env::temp_dir().join(format!("music-core-player-waveform-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (player, mut out, _r) = player();
    player.set_waveform_cache(Some(dir.clone()));
    player.play(remote("full"));
    wait_for(&player, "playing", |s| s.status == PlaybackStatus::Playing);
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(
        player.waveform(),
        WaveformStatus::default(),
        "off by default"
    );

    // A stream: decoded (AAC) once the download is in, then cached.
    player.set_waveform_enabled(true);
    let w = pull_until_waveform(&player, &mut out, "full", |w| {
        matches!(w.state, WaveformState::Ready(_))
    });
    assert_eq!(w.video_id.as_deref(), Some("full"));
    assert!(!w.from_cache);
    let cost = w.cost.unwrap();
    assert_eq!(cost.method, "decode");
    assert!(cost.audio > Duration::from_secs(19), "{cost:?}");
    assert!(w.ready_for("full").is_some() && w.ready_for("other").is_none());
    let WaveformState::Ready(first) = w.state else {
        unreachable!()
    };
    assert_eq!(first.len(), crate::WAVEFORM_BUCKETS);

    // A local Opus file: read from the bitstream after playback started.
    player.play(local("quiet_loud_celt.webm"));
    let w = pull_until_waveform(&player, &mut out, "opus", |w| {
        w.video_id.as_deref() == Some("local:quiet_loud_celt.webm")
            && matches!(w.state, WaveformState::Ready(_))
    });
    assert_eq!(w.cost.unwrap().method, "opus-bitstream");
    let WaveformState::Ready(opus) = &w.state else {
        unreachable!()
    };
    let half = opus.len() / 2;
    assert!(opus.peaks[..half - 8].iter().all(|&p| p < 20));
    assert!(opus.peaks[half + 8..].iter().all(|&p| p > 200));

    // A preview can't have one; the job ends without touching the next track.
    player.play(remote("preview"));
    pull_until_waveform(&player, &mut out, "preview", |w| {
        w.video_id.as_deref() == Some("preview") && matches!(w.state, WaveformState::Unavailable(_))
    });

    // The same stream again comes from the cache.
    player.play(remote("full"));
    let w = pull_until_waveform(&player, &mut out, "cached", |w| {
        w.video_id.as_deref() == Some("full") && matches!(w.state, WaveformState::Ready(_))
    });
    assert!(w.from_cache && w.cost.is_none());
    assert_eq!(w.state, WaveformState::Ready(first));

    player.set_waveform_enabled(false);
    pull_until_waveform(&player, &mut out, "off", |w| {
        *w == WaveformStatus::default()
    });
    let _ = std::fs::remove_dir_all(&dir);
}
