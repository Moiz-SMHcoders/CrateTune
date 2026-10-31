// Auto-skip-on-error limits and next-track preloading ported from ArchiveTune
// app/src/main/kotlin/moe/rukamori/archivetune/playback/MusicService.kt (skipOnError,
// CROSSFADE_PREPARE_AHEAD_MS) and playback/preload/NextStreamPreloader.kt
// — © Rukamori and ArchiveTune contributors, GPL-3.0
//! The audio engine behind [`Player`].
//!
//! Threads: the engine thread owns the output device and all decisions; one
//! voice thread per loaded track (resolve → progressive download → decode →
//! speed/pitch → resample) fills a short queue; the deck, running inside the
//! output callback, mixes the current voice, the preloaded next one for
//! gapless switches and crossfades, and fades. UIs only ever poll
//! [`Player::state`] (and, for visualizers, [`Player::spectrum`]).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use rodio::MixerDeviceSink;

use crate::deck::{Deck, HeadlessOutput, MAX_CROSSFADE};
use crate::equalizer::EqControl;
use crate::output::{self, OutputDevice};
use crate::silence::SilenceConfig;
use crate::sleep::{SleepClock, SleepTimer, SleepTimerState};
use crate::spectrum::{Spectrum, SpectrumAnalyzer, SpectrumConfig, SpectrumTap};
use crate::voice::{Phase, Settings, Voice, VoiceConfig};
use crate::waveform::{self, WAVEFORM_BUCKETS, Waveform, WaveformCache, WaveformCost};
use crate::{EqualizerSettings, Error, Quality, Result, StreamResolver, StreamSource, Track};

const TICK: Duration = Duration::from_millis(100);
/// Start loading the next track this long before the current one ends.
const PRELOAD_AHEAD: Duration = Duration::from_secs(30);
/// Auto-skips in a row (without a track playing in between) before giving up.
const MAX_CONSECUTIVE_SKIPS: u32 = 3;
/// How often to retry a missing output device.
const DEVICE_RETRY: Duration = Duration::from_secs(3);
/// The waveform decode starts once this much of the track has played (so it
/// never delays the first audio), or once the download is complete.
const WAVEFORM_START_AFTER: Duration = Duration::from_millis(500);
/// [`Player::reload_stream`] leaves a track alone this close to its end.
const RELOAD_MIN_REMAINING: Duration = Duration::from_secs(4);
/// ...and keeps a preloaded next track (loaded at the old quality) this close
/// to the end, so the gapless switch stays gapless.
const RELOAD_KEEP_NEXT: Duration = Duration::from_secs(12);
/// A replacement stream this far behind the playing one seeks again (its first
/// seek was rejected, e.g. it waited too long for data).
const RELOAD_RESEEK_BEHIND: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Default)]
pub enum PlaybackStatus {
    #[default]
    Idle,
    /// Resolving + buffering the start of the current track.
    Loading,
    Playing,
    Paused,
    Ended,
    Error(String),
}

/// Snapshot of the engine, cheap to clone. UIs poll this on a timer.
#[derive(Debug, Clone)]
pub struct PlaybackState {
    pub track: Option<Track>,
    pub status: PlaybackStatus,
    /// Position in the track (track time, independent of the playback speed).
    pub position: Duration,
    pub duration: Option<Duration>,
    /// User volume, 0.0..=1.0 (loudness normalization is applied on top).
    pub volume: f32,
    /// Only the start of the track could be fetched (no PoToken); playback ends early.
    pub preview_only: bool,
    /// Incremented whenever the engine moves on to the next track by itself
    /// (gapless or crossfade transition, auto-skip after an error). `track` is
    /// then the track given to [`Player::set_next`]; a UI should advance its
    /// queue to it and call `set_next` with the one after.
    pub advance_count: u64,
    /// Tracks that failed to load since the player started.
    pub error_count: u64,
    /// Video id of the last track that failed to load.
    pub failed_track_id: Option<String>,
    /// The upcoming track as last given to [`Player::set_next`].
    pub next_track: Option<Track>,
    /// The next track is loaded and buffered, ready for a gapless switch.
    pub next_preloaded: bool,
    /// The armed sleep timer and the time left.
    pub sleep_timer: Option<SleepTimerState>,
    /// Playback speed (1.0 = normal); `position` advances this much faster than real time.
    pub speed: f32,
    /// Pitch shift in semitones.
    pub pitch_semitones: f32,
    /// Waiting for audio data mid-playback (slow network, seek into a part
    /// that has not downloaded yet).
    pub buffering: bool,
    /// Two tracks are being crossfaded right now.
    pub crossfading: bool,
    /// Name of the output device in use (`None` without one).
    pub output_device: Option<String>,
    /// Where the current track's audio comes from (YouTube, a provider module,
    /// a file) and its codec / sample rate / bit depth once decoding started.
    pub source: Option<StreamSource>,
    /// [`Player::reload_stream`] is loading another stream of the current
    /// track; it takes over at the same position (then `source` changes).
    pub switching_stream: bool,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            track: None,
            status: PlaybackStatus::Idle,
            position: Duration::ZERO,
            duration: None,
            volume: 1.0,
            preview_only: false,
            advance_count: 0,
            error_count: 0,
            failed_track_id: None,
            next_track: None,
            next_preloaded: false,
            sleep_timer: None,
            speed: 1.0,
            pitch_semitones: 0.0,
            buffering: false,
            crossfading: false,
            output_device: None,
            source: None,
            switching_stream: false,
        }
    }
}

/// The current track's waveform ([`Player::waveform`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub enum WaveformState {
    /// Waveforms are off, or nothing is loaded.
    #[default]
    Off,
    /// Looking in the cache, waiting for playback to start, or decoding.
    Pending,
    Ready(Arc<Waveform>),
    /// Can't be computed for this track (a preview, a stream of unknown
    /// length such as a live stream, a failed download or decode).
    Unavailable(String),
}

/// Snapshot of the waveform work for the current track.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WaveformStatus {
    /// The track this is about (`None` with nothing loaded).
    pub video_id: Option<String>,
    pub state: WaveformState,
    /// Read from the cache (no decode).
    pub from_cache: bool,
    /// What the decode cost (`None` from the cache or before it finished).
    pub cost: Option<WaveformCost>,
}

impl WaveformStatus {
    /// The waveform, if it is ready and belongs to `video_id`.
    pub fn ready_for(&self, video_id: &str) -> Option<&Arc<Waveform>> {
        match &self.state {
            WaveformState::Ready(w) if self.video_id.as_deref() == Some(video_id) => Some(w),
            _ => None,
        }
    }
}

enum Command {
    Play(Track),
    TogglePause,
    Pause,
    Resume,
    Seek(Duration),
    SetVolume(f32),
    SetNormalization(bool),
    SetNext(Option<Track>),
    SetCrossfade(Duration),
    SetSkipSilence(Option<SilenceConfig>),
    SetSpeed(f32),
    SetPitch(f32),
    SetQuality(Quality),
    ReloadStream,
    SetAutoSkip(bool),
    SetSleepTimer(Option<SleepTimer>),
    SetOutputDevice(Option<String>),
    SetWaveform(bool),
    SetWaveformCache(Option<PathBuf>),
    DeviceLost(String),
    /// From the device worker.
    DeviceOpened {
        request: u64,
        result: Result<Opened>,
    },
    DeviceChecked {
        request: u64,
        present: bool,
    },
    /// Internal state as text, for test failure messages.
    #[cfg(test)]
    Debug(Sender<String>),
}

/// Background audio engine. Cloning shares the same engine.
#[derive(Clone)]
pub struct Player {
    tx: Sender<Command>,
    state: Arc<Mutex<PlaybackState>>,
    eq: Arc<EqControl>,
    tap: Arc<SpectrumTap>,
    /// Shared analyzer behind [`Player::spectrum`], created on first use.
    analyzer: Arc<Mutex<Option<SpectrumAnalyzer>>>,
    waveform: Arc<Mutex<WaveformStatus>>,
}

impl Player {
    /// Opens the default output device on a dedicated audio thread.
    pub fn new(resolver: Arc<dyn StreamResolver>) -> Result<Self> {
        Self::start(resolver, None)
    }

    /// A player without an output device: the returned source yields the
    /// mixed output (interleaved stereo at `sample_rate`) and must be pulled
    /// by the caller, e.g. for tests, offline rendering, or a custom sink.
    pub fn headless(
        resolver: Arc<dyn StreamResolver>,
        sample_rate: u32,
    ) -> Result<(Self, HeadlessOutput)> {
        let deck = Deck::new(sample_rate.max(1));
        let player = Self::start(resolver, Some(deck.clone()))?;
        Ok((player, HeadlessOutput::new(deck)))
    }

    fn start(resolver: Arc<dyn StreamResolver>, headless: Option<Arc<Deck>>) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let state = Arc::new(Mutex::new(PlaybackState::default()));
        let eq = Arc::new(EqControl::default());
        let waveform = Arc::new(Mutex::new(WaveformStatus::default()));
        let (ready_tx, ready_rx) = mpsc::channel();
        let (engine_tx, engine_state, engine_eq) = (tx.clone(), state.clone(), eq.clone());
        let engine_waveform = waveform.clone();
        thread::Builder::new()
            .name("music-core-audio".into())
            .spawn(move || {
                let (output, deck) = match headless {
                    Some(deck) => (Output::Headless, deck),
                    None => match open_output(None, &engine_tx) {
                        Ok(opened) => {
                            let rate = opened.sink.config().sample_rate().get();
                            let jobs = spawn_device_worker(engine_tx.clone());
                            (
                                Output::Device(Box::new(DeviceOutput::new(opened, None, jobs))),
                                Deck::new(rate),
                            )
                        }
                        Err(e) => {
                            let _ = ready_tx.send(Err(e));
                            return;
                        }
                    },
                };
                let tap = deck.spectrum_tap();
                let mut engine = Engine::new(engine_state, engine_eq, resolver, deck, output);
                engine.waveform.status = engine_waveform;
                engine.attach_output();
                let _ = ready_tx.send(Ok(tap));
                engine.run(rx);
            })
            .map_err(|e| Error::Audio(e.to_string()))?;
        let tap = ready_rx
            .recv()
            .map_err(|_| Error::Audio("audio thread died".into()))??;
        Ok(Self {
            tx,
            state,
            eq,
            tap,
            analyzer: Arc::default(),
            waveform,
        })
    }

    fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }

    /// Replaces whatever is playing with `track`: its `local_path` when set,
    /// otherwise the YouTube stream for its `video_id` (played while it downloads).
    pub fn play(&self, track: Track) {
        self.send(Command::Play(track));
    }

    pub fn toggle_pause(&self) {
        self.send(Command::TogglePause);
    }

    /// Pauses if playing; otherwise does nothing.
    pub fn pause(&self) {
        self.send(Command::Pause);
    }

    /// Resumes if paused, or replays the track if it ended; otherwise does nothing.
    pub fn resume(&self) {
        self.send(Command::Resume);
    }

    /// Seeks the current track. A target past a preview's data is rejected
    /// (playback continues where it was); one past the downloaded part of a
    /// complete stream waits up to 10 s for the download (`buffering`).
    pub fn seek(&self, position: Duration) {
        self.send(Command::Seek(position));
    }

    pub fn set_volume(&self, volume: f32) {
        self.send(Command::SetVolume(volume.clamp(0.0, 1.0)));
    }

    /// Loudness normalization (YouTube `loudnessDb`, ReplayGain / R128 tags of
    /// local files), on by default. Applies to the playing track right away.
    pub fn set_normalization(&self, enabled: bool) {
        self.send(Command::SetNormalization(enabled));
    }

    /// The track to play after the current one (call whenever the queue
    /// changes). The engine starts loading it ~30 s before the current track
    /// ends and switches to it gaplessly (or crossfades, see
    /// [`Self::set_crossfade`]); the switch shows up as a new
    /// [`PlaybackState::advance_count`]. If the current track already ended,
    /// setting a next track starts it. `None` makes the current track the last.
    pub fn set_next(&self, track: Option<Track>) {
        self.send(Command::SetNext(track));
    }

    /// Crossfade between consecutive tracks, 0 (off, gapless) to 12 s.
    /// Applies to the end of a preview too.
    pub fn set_crossfade(&self, duration: Duration) {
        self.send(Command::SetCrossfade(duration.min(MAX_CROSSFADE)));
    }

    /// Skip silence at track edges and long silent gaps (`None`: off).
    pub fn set_skip_silence(&self, config: Option<SilenceConfig>) {
        self.send(Command::SetSkipSilence(config));
    }

    /// Playback speed, 0.5..=2.0, keeping the pitch.
    pub fn set_speed(&self, speed: f32) {
        self.send(Command::SetSpeed(speed));
    }

    /// Pitch shift in semitones (-12..=12), keeping the speed.
    pub fn set_pitch(&self, semitones: f32) {
        self.send(Command::SetPitch(semitones));
    }

    /// Stream quality for tracks loaded from now on.
    pub fn set_quality(&self, quality: Quality) {
        self.send(Command::SetQuality(quality));
    }

    /// Resolves the current track again with the current settings (e.g. after
    /// [`Self::set_quality`]) and switches to the new stream at the same
    /// position without a gap: the playing stream continues until the new
    /// one has audio there, then a 40 ms cross-fade hands over.
    /// [`PlaybackState::switching_stream`] is set meanwhile. If the new stream
    /// turns out to be the same format, or fails, the current one just plays
    /// on. A preloaded next track is loaded again too (unless the current one
    /// is about to end). Does nothing for local files, a track that is still
    /// loading its first stream (which restarts with the new settings
    /// instead), or one within a few seconds of its end.
    pub fn reload_stream(&self) {
        self.send(Command::ReloadStream);
    }

    /// When a track fails to load, move on to the next track (if one is set)
    /// instead of stopping with [`PlaybackStatus::Error`]. Gives up after a
    /// few failures in a row.
    pub fn set_auto_skip(&self, enabled: bool) {
        self.send(Command::SetAutoSkip(enabled));
    }

    /// Arms (or with `None` cancels) the sleep timer.
    pub fn set_sleep_timer(&self, timer: Option<SleepTimer>) {
        self.send(Command::SetSleepTimer(timer));
    }

    /// Output devices present right now. Asks the OS, which a misbehaving
    /// driver can stall: call it off the UI thread.
    pub fn output_devices() -> Vec<OutputDevice> {
        output::list()
    }

    /// Switches output to the device with this id (or name), `None` for the
    /// system default. A missing device falls back to the default and is
    /// picked up again when it reappears; if the device in use disappears,
    /// output moves to the default.
    pub fn set_output_device(&self, id: Option<String>) {
        self.send(Command::SetOutputDevice(id));
    }

    /// Engine, deck and voice state as text, for test failure messages.
    #[cfg(test)]
    pub(crate) fn debug_dump(&self) -> String {
        let (tx, rx) = mpsc::channel();
        self.send(Command::Debug(tx));
        rx.recv_timeout(Duration::from_secs(2))
            .unwrap_or_else(|_| "engine did not answer".into())
    }

    pub fn state(&self) -> PlaybackState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Computes the loudness envelope ([`Waveform`]) of each track that
    /// becomes current while on (off by default): looked up in the cache
    /// (see [`Self::set_waveform_cache`]), else decoded once on a separate
    /// low-priority thread with its own decoder over the same bytes — the
    /// local file, or the stream as it downloads (no extra requests). The
    /// decode starts after the first 0.5 s have played (or once the download
    /// is complete), so it never delays the first audio, and is cancelled
    /// when the track changes or this is turned off. Poll [`Self::waveform`].
    pub fn set_waveform_enabled(&self, enabled: bool) {
        self.send(Command::SetWaveform(enabled));
    }

    /// Directory for finished waveforms (a few hundred bytes per track,
    /// keyed by video id or local file + modification time); `None`: no cache.
    pub fn set_waveform_cache(&self, dir: Option<PathBuf>) {
        self.send(Command::SetWaveformCache(dir));
    }

    /// The current track's waveform and how it was obtained. Cheap (a lock
    /// and an `Arc` clone); meant for the UI's poll.
    pub fn waveform(&self) -> WaveformStatus {
        self.waveform
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Applies `settings` to the playing track within ~150 ms (the decoded
    /// audio queued ahead; smoothed, no restart) and to every track after it.
    ///
    /// Out-of-range values are clamped; see [`EqualizerSettings::sanitize`].
    pub fn set_equalizer(&self, mut settings: EqualizerSettings) {
        settings.sanitize();
        self.eq.set(settings);
    }

    /// Turns the equalizer off (bit-exact passthrough).
    pub fn clear_equalizer(&self) {
        self.eq.set(EqualizerSettings::default());
    }

    /// The settings last passed to [`Self::set_equalizer`] (disabled by default).
    pub fn equalizer(&self) -> EqualizerSettings {
        self.eq.get()
    }

    /// Feeds the output into the spectrum analyzer (off by default). While
    /// off, the audio callback skips the tap entirely (one atomic load per
    /// block) and [`Self::spectrum`] decays to zeros. Turn it on while a
    /// visualizer is visible.
    pub fn set_spectrum_enabled(&self, enabled: bool) {
        self.tap.set_enabled(enabled);
    }

    pub fn spectrum_enabled(&self) -> bool {
        self.tap.is_enabled()
    }

    /// The current spectrum in `bands` log-spaced bands (see
    /// [`SpectrumConfig`]), for a visualizer polling at display rate (≤ 60 Hz).
    /// Reflects the final mix the listener hears (after EQ, normalization and
    /// crossfades) before the volume, so muted playback still moves; decays to
    /// zero when paused, stopped or silent. Needs
    /// [`Self::set_spectrum_enabled`].
    ///
    /// All clones of the player share this analyzer and its smoothing; views
    /// with different band counts should each own a [`Self::spectrum_analyzer`].
    pub fn spectrum(&self, bands: usize) -> Spectrum {
        let mut out = Spectrum::default();
        self.spectrum_into(bands, &mut out);
        out
    }

    /// [`Self::spectrum`] into `out`, reusing its buffers (no allocation per frame).
    pub fn spectrum_into(&self, bands: usize, out: &mut Spectrum) {
        let mut analyzer = self.analyzer.lock().unwrap_or_else(|e| e.into_inner());
        analyzer
            .get_or_insert_with(|| self.spectrum_analyzer())
            .analyze(bands, out);
    }

    /// Changes the band mapping and smoothing of [`Self::spectrum`].
    pub fn set_spectrum_config(&self, config: SpectrumConfig) {
        let mut analyzer = self.analyzer.lock().unwrap_or_else(|e| e.into_inner());
        analyzer
            .get_or_insert_with(|| self.spectrum_analyzer())
            .set_config(config);
    }

    /// A separate analyzer on the same output, with its own band count,
    /// configuration and smoothing (e.g. one per visualizer).
    pub fn spectrum_analyzer(&self) -> SpectrumAnalyzer {
        SpectrumAnalyzer::new(self.tap.clone(), SpectrumConfig::default())
    }
}

enum Output {
    Headless,
    // Boxed: the device side is a few hundred bytes (more on Linux / Windows).
    Device(Box<DeviceOutput>),
}

/// The output device, managed without ever blocking the engine thread: every
/// CoreAudio/WASAPI/ALSA call (listing, opening, closing) runs on the device
/// worker, because a misbehaving driver can stall them for a long time (seen
/// with a hung virtual device, which also blocked unrelated property queries).
struct DeviceOutput {
    sink: Option<MixerDeviceSink>,
    /// Device the user picked (`None`: system default).
    wanted: Option<String>,
    /// The open device is the wanted one.
    on_wanted: bool,
    /// The wanted device was absent at the last check; switch back when it returns.
    wanted_missing: bool,
    name: Option<String>,
    jobs: Sender<DeviceJob>,
    /// Latest job sent to the worker; older answers are discarded.
    request: u64,
    busy: bool,
    last_check: Instant,
}

/// A stream opened by the device worker.
struct Opened {
    sink: MixerDeviceSink,
    name: String,
    on_wanted: bool,
    wanted_missing: bool,
}

enum DeviceJob {
    Open {
        request: u64,
        wanted: Option<String>,
    },
    Check {
        request: u64,
        wanted: String,
    },
}

impl DeviceOutput {
    fn new(opened: Opened, wanted: Option<String>, jobs: Sender<DeviceJob>) -> Self {
        Self {
            sink: Some(opened.sink),
            wanted,
            on_wanted: opened.on_wanted,
            wanted_missing: opened.wanted_missing,
            name: Some(opened.name),
            jobs,
            request: 0,
            busy: false,
            last_check: Instant::now(),
        }
    }

    fn send(&mut self, job: impl FnOnce(u64) -> DeviceJob) {
        self.request += 1;
        self.busy = true;
        self.last_check = Instant::now();
        let _ = self.jobs.send(job(self.request));
    }
}

/// Opens the wanted (or default) device. Blocking; runs on the device worker.
fn open_output(wanted: Option<&str>, tx: &Sender<Command>) -> Result<Opened> {
    let lost = tx.clone();
    let on_error = move |e: rodio::cpal::Error| {
        if output::is_fatal(e.kind()) {
            let _ = lost.send(Command::DeviceLost(e.to_string()));
        } else {
            log::debug!("audio output: {e}");
        }
    };
    let (sink, name, on_wanted) = output::open(wanted, on_error)?;
    let wanted_missing = !on_wanted && wanted.is_some_and(|w| !output::is_present(w));
    Ok(Opened {
        sink,
        name,
        on_wanted,
        wanted_missing,
    })
}

fn spawn_device_worker(tx: Sender<Command>) -> Sender<DeviceJob> {
    let (jobs, rx) = mpsc::channel::<DeviceJob>();
    let spawned = thread::Builder::new()
        .name("music-core-devices".into())
        .spawn(move || {
            for job in rx {
                let reply = match job {
                    DeviceJob::Open { request, wanted } => Command::DeviceOpened {
                        request,
                        result: open_output(wanted.as_deref(), &tx),
                    },
                    DeviceJob::Check { request, wanted } => Command::DeviceChecked {
                        request,
                        present: output::is_present(&wanted),
                    },
                };
                if tx.send(reply).is_err() {
                    return;
                }
            }
        });
    if let Err(e) = spawned {
        log::warn!("no device worker: {e}");
    }
    jobs
}

struct Replacement {
    voice: Arc<Voice>,
    /// Given to the deck for the handoff.
    armed: bool,
}

struct Engine {
    state: Arc<Mutex<PlaybackState>>,
    deck: Arc<Deck>,
    config: VoiceConfig,
    output: Output,
    current: Option<Arc<Voice>>,
    /// Preloaded voice for `next_track` (also handed to the deck).
    next: Option<Arc<Voice>>,
    /// Another stream of the current track ([`Player::reload_stream`]);
    /// handed to the deck once it is open and differs from the current one.
    replacement: Option<Replacement>,
    next_track: Option<Track>,
    last_voice_id: u64,
    seen_advances: u64,
    /// Voice whose failure was already counted.
    reported_failure: Option<u64>,
    auto_skip: bool,
    consecutive_errors: u32,
    sleep: SleepClock,
    /// Paused at the end of the last track by the sleep timer; resuming
    /// reports `Ended` so the UI's queue moves on.
    held_at_end: bool,
    waveform: WaveformJobs,
}

/// The waveform worker of the current voice ([`Player::set_waveform_enabled`]).
#[derive(Default)]
struct WaveformJobs {
    enabled: bool,
    cache: Option<Arc<WaveformCache>>,
    status: Arc<Mutex<WaveformStatus>>,
    /// The voice the running (or finished) job belongs to, and its cancel flag.
    job: Option<(u64, Arc<AtomicBool>)>,
}

impl WaveformJobs {
    fn status(&self) -> MutexGuard<'_, WaveformStatus> {
        self.status.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn cancel(&mut self) {
        if let Some((_, cancel)) = self.job.take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Starts (or keeps) the job for `voice`; called on every engine tick.
    fn follow(&mut self, voice: Option<&Arc<Voice>>) {
        let voice = match voice {
            Some(voice) if self.enabled => voice,
            _ => {
                self.cancel();
                let mut status = self.status();
                if *status != WaveformStatus::default() {
                    *status = WaveformStatus::default();
                }
                return;
            }
        };
        if self.job.as_ref().is_some_and(|(id, _)| *id == voice.id) {
            return;
        }
        self.cancel();
        let video_id = voice.track.video_id.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.job = Some((voice.id, cancel.clone()));
        {
            let mut status = self.status();
            // The same song again (repeat one, replay): keep what we have.
            if status.video_id.as_deref() == Some(video_id.as_str())
                && matches!(status.state, WaveformState::Ready(_))
            {
                return;
            }
            *status = WaveformStatus {
                video_id: Some(video_id),
                state: WaveformState::Pending,
                ..WaveformStatus::default()
            };
        }
        let (voice, cache, status) = (voice.clone(), self.cache.clone(), self.status.clone());
        let spawned = thread::Builder::new()
            .name("music-core-waveform".into())
            .spawn(move || waveform_job(&voice, cache.as_deref(), &cancel, &status));
        if let Err(e) = spawned {
            let mut status = self.status();
            status.state = WaveformState::Unavailable(format!("no thread: {e}"));
        }
    }
}

/// One track's waveform: the cache, else (once playback has started or the
/// download is complete) a full decode; the result goes to `status` unless
/// `cancel` was set meanwhile.
fn waveform_job(
    voice: &Voice,
    cache: Option<&WaveformCache>,
    cancel: &AtomicBool,
    status: &Mutex<WaveformStatus>,
) {
    waveform::lower_thread_priority();
    let publish = |state: WaveformState, from_cache: bool, cost: Option<WaveformCost>| {
        let mut status = status.lock().unwrap_or_else(|e| e.into_inner());
        if !cancel.load(Ordering::Relaxed)
            && status.video_id.as_deref() == Some(voice.track.video_id.as_str())
        {
            status.state = state;
            status.from_cache = from_cache;
            status.cost = cost;
        }
    };
    let key = waveform::cache_key(&voice.track);
    if let (Some(cache), Some(key)) = (cache, &key)
        && let Some(found) = cache.get(key)
    {
        publish(WaveformState::Ready(Arc::new(found)), true, None);
        return;
    }
    // Wait until the track plays (or its download is complete).
    let source = loop {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        match voice.phase() {
            Phase::Failed(e) => {
                publish(WaveformState::Unavailable(e), false, None);
                return;
            }
            Phase::Loading => {}
            Phase::Ready => {
                let source = voice.waveform_source();
                let downloaded = matches!(
                    &source,
                    Some(waveform::WaveformSource::Stream { buffer, .. })
                        if buffer.end().is_some()
                );
                if source.is_some() && (downloaded || voice.played() >= WAVEFORM_START_AFTER) {
                    break source;
                }
            }
        }
        thread::sleep(TICK);
    };
    let Some(source) = source else { return };
    match waveform::compute(&source, WAVEFORM_BUCKETS, cancel) {
        Ok((found, cost)) => {
            log::debug!(
                "{}: waveform in {:?} CPU / {:?} for {:?} of audio",
                voice.track.video_id,
                cost.cpu,
                cost.wall,
                cost.audio
            );
            if let (Some(cache), Some(key)) = (cache, &key)
                && let Err(e) = cache.put(key, &found)
            {
                log::warn!("waveform cache: {e}");
            }
            publish(WaveformState::Ready(Arc::new(found)), false, Some(cost));
        }
        Err(e) => {
            if !cancel.load(Ordering::Relaxed) {
                log::info!("{}: no waveform: {e}", voice.track.video_id);
            }
            publish(WaveformState::Unavailable(e.to_string()), false, None);
        }
    }
}

impl Engine {
    fn new(
        state: Arc<Mutex<PlaybackState>>,
        eq: Arc<EqControl>,
        resolver: Arc<dyn StreamResolver>,
        deck: Arc<Deck>,
        output: Output,
    ) -> Self {
        Self {
            state,
            config: VoiceConfig {
                out_rate: deck.rate(),
                eq,
                resolver,
                quality: Quality::default(),
                settings: Settings::default(),
            },
            deck,
            output,
            current: None,
            next: None,
            replacement: None,
            next_track: None,
            last_voice_id: 0,
            seen_advances: 0,
            reported_failure: None,
            auto_skip: false,
            consecutive_errors: 0,
            sleep: SleepClock::default(),
            held_at_end: false,
            waveform: WaveformJobs::default(),
        }
    }

    fn state(&self) -> MutexGuard<'_, PlaybackState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn run(mut self, rx: Receiver<Command>) {
        loop {
            match rx.recv_timeout(TICK) {
                Ok(cmd) => {
                    self.handle(cmd);
                    while let Ok(cmd) = rx.try_recv() {
                        self.handle(cmd);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.tick();
        }
        self.waveform.cancel();
        self.drop_replacement();
        for voice in [self.current.take(), self.next.take()]
            .into_iter()
            .flatten()
        {
            voice.stop();
        }
        self.deck.set_current(None);
        self.deck.cancel_fades();
    }

    fn spawn_voice(&mut self, track: Track) -> Arc<Voice> {
        self.last_voice_id += 1;
        Voice::spawn(self.last_voice_id, track, self.config.clone())
    }

    fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Play(track) => self.play(track),
            Command::TogglePause => {
                let status = self.state().status.clone();
                match status {
                    PlaybackStatus::Playing | PlaybackStatus::Loading => self.pause(),
                    _ => self.resume(),
                }
            }
            Command::Pause => self.pause(),
            Command::Resume => self.resume(),
            Command::Seek(position) => {
                if let Some(voice) = &self.current {
                    self.deck.cancel_fades();
                    voice.seek(position);
                    self.held_at_end = false;
                }
                if let Some(replacement) = &self.replacement {
                    replacement.voice.seek(position);
                }
            }
            Command::SetVolume(volume) => {
                self.state().volume = volume;
                self.deck.set_volume(volume);
            }
            Command::SetNormalization(enabled) => self.deck.set_normalization(enabled),
            Command::SetNext(track) => self.set_next(track),
            Command::SetCrossfade(d) => self.deck.set_crossfade(d),
            Command::SetSkipSilence(config) => {
                self.config.settings.silence = config;
                self.push_settings();
            }
            Command::SetSpeed(speed) => {
                self.config.settings.speed = if speed.is_finite() {
                    speed.clamp(*crate::SPEED_RANGE.start(), *crate::SPEED_RANGE.end())
                } else {
                    1.0
                };
                self.push_settings();
            }
            Command::SetPitch(semitones) => {
                self.config.settings.semitones = if semitones.is_finite() {
                    semitones.clamp(*crate::PITCH_RANGE.start(), *crate::PITCH_RANGE.end())
                } else {
                    0.0
                };
                self.push_settings();
            }
            Command::SetQuality(quality) => self.config.quality = quality,
            Command::ReloadStream => self.reload_stream(),
            Command::SetAutoSkip(enabled) => self.auto_skip = enabled,
            Command::SetSleepTimer(timer) => {
                self.sleep.start(timer, Instant::now());
                self.deck.set_hold_at_end(self.sleep.pause_at_end());
            }
            Command::SetOutputDevice(id) => {
                if let Output::Device(out) = &mut self.output {
                    out.wanted = id;
                    let wanted = out.wanted.clone();
                    // The current stream keeps playing until the new one is open.
                    out.send(|request| DeviceJob::Open { request, wanted });
                }
            }
            Command::SetWaveform(enabled) => self.waveform.enabled = enabled,
            Command::SetWaveformCache(dir) => {
                self.waveform.cache = dir.map(|d| Arc::new(WaveformCache::new(d)));
            }
            Command::DeviceLost(reason) => {
                if let Output::Device(out) = &mut self.output {
                    log::warn!("audio output lost ({reason}); reopening");
                    if let Some(dead) = out.sink.take() {
                        output::close(dead);
                    }
                    out.name = None;
                    let wanted = out.wanted.clone();
                    out.send(|request| DeviceJob::Open { request, wanted });
                }
            }
            Command::DeviceOpened { request, result } => self.device_opened(request, result),
            #[cfg(test)]
            Command::Debug(reply) => {
                let mut lines = vec![self.deck.debug_state()];
                for (what, voice) in [("current", &self.current), ("next", &self.next)] {
                    if let Some(v) = voice {
                        lines.push(format!("{what}: {}", v.debug_state()));
                    }
                }
                if let Some(r) = &self.replacement {
                    lines.push(format!(
                        "replacement (armed={}): {}",
                        r.armed,
                        r.voice.debug_state()
                    ));
                }
                let _ = reply.send(lines.join("\n"));
            }
            Command::DeviceChecked { request, present } => {
                if let Output::Device(out) = &mut self.output
                    && request == out.request
                {
                    out.busy = false;
                    let returned = present && out.wanted_missing;
                    out.wanted_missing = !present;
                    if returned {
                        let wanted = out.wanted.clone();
                        out.send(|request| DeviceJob::Open { request, wanted });
                    }
                }
            }
        }
    }

    fn push_settings(&mut self) {
        let settings = self.config.settings;
        let replacement = self.replacement.as_ref().map(|r| &r.voice);
        for voice in self
            .current
            .iter()
            .chain(self.next.iter())
            .chain(replacement)
        {
            voice.set_settings(settings);
        }
        let mut st = self.state();
        st.speed = settings.speed;
        st.pitch_semitones = settings.semitones;
    }

    fn play(&mut self, track: Track) {
        self.held_at_end = false;
        self.consecutive_errors = 0;
        // The preloaded next track starts instantly.
        let reuse = self.next.take_if(|v| {
            v.track.video_id == track.video_id && !matches!(v.phase(), Phase::Failed(_))
        });
        let voice = match reuse {
            Some(voice) => {
                self.deck.set_next(None);
                voice
            }
            None => self.spawn_voice(track.clone()),
        };
        if self
            .next_track
            .as_ref()
            .is_some_and(|t| t.video_id == track.video_id)
        {
            self.next_track = None;
            if let Some(stale) = self.next.take() {
                stale.stop();
            }
            if let Some(stale) = self.deck.set_next(None) {
                stale.stop();
            }
            self.state().next_track = None;
        }
        self.make_current(voice, track);
        self.deck.set_paused(false);
    }

    /// Puts `voice` on the deck and resets the state for it.
    fn make_current(&mut self, voice: Arc<Voice>, track: Track) {
        self.drop_replacement();
        self.deck.set_current(Some(voice.clone()));
        self.current = Some(voice);
        let mut st = self.state();
        st.status = PlaybackStatus::Loading;
        st.position = Duration::ZERO;
        st.preview_only = false;
        st.buffering = false;
        st.duration = track.duration_secs.map(|s| Duration::from_secs(s.into()));
        st.track = Some(track);
    }

    fn pause(&mut self) {
        let status = self.state().status.clone();
        if matches!(status, PlaybackStatus::Playing | PlaybackStatus::Loading) {
            self.deck.set_paused(true);
            if status == PlaybackStatus::Playing {
                self.state().status = PlaybackStatus::Paused;
            }
        }
    }

    fn resume(&mut self) {
        let status = self.state().status.clone();
        match status {
            PlaybackStatus::Paused if self.held_at_end => {
                // The sleep timer stopped us at the end; let the queue move on.
                self.held_at_end = false;
                self.deck.set_paused(false);
                self.state().status = PlaybackStatus::Ended;
            }
            PlaybackStatus::Paused => {
                self.deck.set_paused(false);
                self.state().status = PlaybackStatus::Playing;
            }
            PlaybackStatus::Ended => {
                if let Some(voice) = &self.current {
                    voice.seek(Duration::ZERO);
                    self.deck.set_paused(false);
                    let mut st = self.state();
                    st.status = PlaybackStatus::Playing;
                    st.position = Duration::ZERO;
                }
            }
            _ => {}
        }
    }

    fn set_next(&mut self, track: Option<Track>) {
        let same = match (&self.next_track, &track) {
            (Some(a), Some(b)) => a.video_id == b.video_id,
            (None, None) => true,
            _ => false,
        };
        self.state().next_track = track.clone();
        self.next_track = track;
        if same {
            return;
        }
        if let Some(old) = self.next.take() {
            old.stop();
        }
        if let Some(old) = self.deck.set_next(None) {
            old.stop();
        }
        // Its stream URL is resolved now (in the background), so the preload
        // near the end of this song, or a skip, needs no resolve.
        if let Some(next) = &self.next_track {
            self.config
                .resolver
                .prefetch_track(next, self.config.quality);
        }
    }

    /// Moves to `next_track` right away (auto-skip after an error).
    fn skip_to_next(&mut self) -> bool {
        let Some(track) = self.next_track.take() else {
            return false;
        };
        let preloaded = self
            .next
            .take()
            .filter(|v| v.track.video_id == track.video_id);
        if let Some(old) = self.deck.set_next(None)
            && preloaded.as_ref().is_none_or(|p| p.id != old.id)
        {
            old.stop();
        }
        let voice = preloaded.unwrap_or_else(|| self.spawn_voice(track.clone()));
        self.make_current(voice, track);
        let mut st = self.state();
        st.advance_count += 1;
        st.next_track = None;
        true
    }

    fn tick(&mut self) {
        let now = Instant::now();
        self.check_output(now);

        if self.sleep.take_expired(now) {
            self.deck.set_paused(true);
            self.deck.cancel_fades();
        }
        self.deck.set_hold_at_end(self.sleep.pause_at_end());

        // Did the deck move on by itself (gapless / crossfade)?
        let snap = self.deck.snapshot();
        if snap.advances != self.seen_advances {
            self.seen_advances = snap.advances;
            let next = self.next.take();
            match next {
                Some(next) if snap.current == Some(next.id) => {
                    // The old voice is stopped by the deck once its fade ends.
                    self.current = Some(next.clone());
                    self.next_track = None;
                    self.consecutive_errors = 0;
                    let track = next.track.clone();
                    let mut st = self.state();
                    st.advance_count += 1;
                    st.next_track = None;
                    st.position = Duration::ZERO;
                    st.preview_only = false;
                    st.duration = track.duration_secs.map(|s| Duration::from_secs(s.into()));
                    st.track = Some(track);
                }
                other => self.next = other,
            }
            if self.deck.take_held() {
                self.sleep.take_track_end();
                self.deck.set_hold_at_end(false);
            }
        }

        self.check_replacement(&snap);
        self.update_status(&snap);
        self.maybe_preload();
        self.waveform.follow(self.current.as_ref());

        let mut st = self.state();
        st.sleep_timer = self.sleep.state(now);
        st.switching_stream = self.replacement.is_some();
        st.crossfading = snap.crossfading;
        st.next_preloaded = self.next.as_ref().is_some_and(|v| v.has_audio());
        if let Output::Device(out) = &self.output {
            st.output_device = out.name.clone();
        }
    }

    fn update_status(&mut self, snap: &crate::deck::DeckSnapshot) {
        let Some(voice) = self.current.clone() else {
            return;
        };
        let phase = voice.phase();
        let exhausted = voice.is_played_out();
        let failure = match &phase {
            Phase::Failed(msg) => Some(msg.clone()),
            Phase::Ready if exhausted && !snap.paused => voice.stream_error(),
            _ => None,
        };
        if let Some(message) = failure {
            if self.reported_failure != Some(voice.id) {
                self.reported_failure = Some(voice.id);
                self.consecutive_errors += 1;
                let mut st = self.state();
                st.error_count += 1;
                st.failed_track_id = Some(voice.track.video_id.clone());
                st.status = PlaybackStatus::Error(message.clone());
                st.buffering = false;
            }
            if self.auto_skip
                && self.consecutive_errors <= MAX_CONSECUTIVE_SKIPS
                && self.skip_to_next()
            {
                return;
            }
            let mut st = self.state();
            st.status = PlaybackStatus::Error(message);
            st.buffering = false;
            return;
        }

        if exhausted && !snap.paused && self.sleep.take_track_end() {
            // Sleep timer "end of track" with nothing queued.
            self.deck.set_paused(true);
            self.held_at_end = true;
        }
        let paused = snap.paused || self.held_at_end;
        // The deck switches to a preloaded next track by itself; until the
        // next tick notices, don't flash `Ended` (a UI would skip ahead).
        let switching = self.next.is_some() && !self.sleep.pause_at_end();
        let status = match phase {
            Phase::Loading => PlaybackStatus::Loading,
            _ if self.held_at_end => PlaybackStatus::Paused,
            _ if exhausted && switching && !paused => PlaybackStatus::Playing,
            _ if exhausted => PlaybackStatus::Ended,
            _ if paused => PlaybackStatus::Paused,
            _ => PlaybackStatus::Playing,
        };
        let pending_seek = voice.pending_seek();
        let position = pending_seek.unwrap_or_else(|| voice.played());
        if status == PlaybackStatus::Playing && position > Duration::ZERO {
            self.consecutive_errors = 0;
        }
        let mut st = self.state();
        st.buffering = status == PlaybackStatus::Loading
            || (status == PlaybackStatus::Playing
                && (pending_seek.is_some() || voice.is_underrun()));
        st.status = status;
        st.position = position;
        if let Some(d) = voice.duration() {
            st.duration = Some(d);
        }
        st.preview_only = voice.is_preview();
        st.source = voice.source();
    }

    /// [`Player::reload_stream`].
    fn reload_stream(&mut self) {
        let Some(current) = self.current.clone() else {
            return;
        };
        if current.track.is_local() {
            return;
        }
        let status = self.state().status.clone();
        match current.phase() {
            Phase::Failed(_) => return,
            // Nothing played yet: start over with the new settings.
            Phase::Loading => {
                let pending = current.pending_seek();
                let voice = self.spawn_voice(current.track.clone());
                if let Some(at) = pending {
                    voice.seek(at);
                }
                let track = current.track.clone();
                self.make_current(voice, track);
                return;
            }
            Phase::Ready => {}
        }
        if !matches!(status, PlaybackStatus::Playing | PlaybackStatus::Paused)
            || current
                .remaining()
                .is_some_and(|r| r < RELOAD_MIN_REMAINING)
        {
            return;
        }
        self.drop_replacement();
        let voice = self.spawn_voice(current.track.clone());
        voice.seek(current.pending_seek().unwrap_or_else(|| current.played()));
        self.replacement = Some(Replacement {
            voice,
            armed: false,
        });
        self.state().switching_stream = true;
        // A preloaded next track was resolved with the old settings.
        if current.remaining().is_none_or(|r| r > RELOAD_KEEP_NEXT) {
            if let Some(old) = self.next.take() {
                old.stop();
            }
            if let Some(old) = self.deck.set_next(None) {
                old.stop();
            }
        }
    }

    fn drop_replacement(&mut self) {
        if let Some(old) = self.replacement.take() {
            old.voice.stop();
            if old.armed {
                self.deck.set_handoff(None, 0);
            }
        }
        self.state().switching_stream = false;
    }

    /// Follows the replacement stream: hands it to the deck once it is open
    /// (unless it is the very same format), adopts it once the deck switched.
    fn check_replacement(&mut self, snap: &crate::deck::DeckSnapshot) {
        let Some(replacement) = &self.replacement else {
            return;
        };
        let on_deck = snap.current;
        let voice = replacement.voice.clone();
        if on_deck == Some(voice.id) {
            log::debug!("{}: switched to the reloaded stream", voice.track.video_id);
            self.replacement = None;
            self.current = Some(voice);
            self.state().switching_stream = false;
            return;
        }
        let Some(current) = self.current.clone() else {
            self.drop_replacement();
            return;
        };
        if current.track.video_id != voice.track.video_id || on_deck != Some(current.id) {
            self.drop_replacement();
            return;
        }
        match voice.phase() {
            Phase::Loading => {}
            Phase::Failed(e) => {
                log::warn!("{}: reloading the stream failed: {e}", voice.track.video_id);
                self.drop_replacement();
            }
            Phase::Ready if !replacement.armed => {
                if voice.source().is_some() && voice.source() == current.source() {
                    log::debug!(
                        "{}: reloaded stream is the same format",
                        voice.track.video_id
                    );
                    self.drop_replacement();
                    return;
                }
                if let Some(old) = self.deck.set_handoff(Some(voice.clone()), current.id) {
                    old.stop();
                }
                if let Some(r) = &mut self.replacement {
                    r.armed = true;
                }
            }
            Phase::Ready => {
                // Paused, the deck mixes nothing: switch now, so the source
                // shown is right before playback resumes.
                if snap.paused {
                    self.deck.try_hand_off();
                }
                // Far behind (a rejected seek): try again a little ahead.
                let at = current.played();
                if voice.pending_seek().is_none()
                    && voice
                        .queued_start()
                        .is_some_and(|start| start + RELOAD_RESEEK_BEHIND < at)
                {
                    voice.seek(at + Duration::from_millis(500));
                }
            }
        }
    }

    fn maybe_preload(&mut self) {
        if self.next.is_some() {
            return;
        }
        let (Some(track), Some(current)) = (&self.next_track, &self.current) else {
            return;
        };
        let due = match current.phase() {
            Phase::Ready => {
                let speed = current.speed().max(0.01);
                current.is_played_out()
                    || current
                        .remaining()
                        .is_none_or(|r| r.div_f32(speed) <= PRELOAD_AHEAD + MAX_CROSSFADE)
            }
            Phase::Loading | Phase::Failed(_) => false,
        };
        if !due {
            return;
        }
        let voice = self.spawn_voice(track.clone());
        if let Some(old) = self.deck.set_next(Some(voice.clone())) {
            old.stop();
        }
        self.next = Some(voice);
    }

    // ---- output device ----

    fn attach_output(&mut self) {
        if let Output::Device(out) = &self.output
            && let Some(sink) = &out.sink
        {
            // The deck keeps its rate; the mixer converts if the device differs.
            sink.mixer().add(HeadlessOutput::new(self.deck.clone()));
        }
    }

    fn device_opened(&mut self, request: u64, result: Result<Opened>) {
        let Output::Device(out) = &mut self.output else {
            return;
        };
        if request != out.request {
            if let Ok(stale) = result {
                output::close(stale.sink);
            }
            return;
        }
        out.busy = false;
        match result {
            Ok(opened) => {
                if let Some(old) = out.sink.take() {
                    output::close(old);
                }
                out.sink = Some(opened.sink);
                out.name = Some(opened.name);
                out.on_wanted = opened.on_wanted;
                out.wanted_missing = opened.wanted_missing;
                self.attach_output();
            }
            Err(e) => log::warn!("could not open audio output: {e}"),
        }
    }

    /// Retries when no device is open, and returns to the wanted device when
    /// it reappears (not while it is present but failing to open).
    fn check_output(&mut self, now: Instant) {
        let Output::Device(out) = &mut self.output else {
            return;
        };
        if out.busy || now.duration_since(out.last_check) < DEVICE_RETRY {
            return;
        }
        if out.sink.is_none() {
            let wanted = out.wanted.clone();
            out.send(|request| DeviceJob::Open { request, wanted });
        } else if !out.on_wanted
            && let Some(wanted) = out.wanted.clone()
        {
            out.send(|request| DeviceJob::Check { request, wanted });
        }
    }
}

#[cfg(test)]
mod tests;
