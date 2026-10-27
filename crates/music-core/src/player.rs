use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use rodio::{DeviceSinkBuilder, MixerDeviceSink, Source};

use crate::equalizer::EqControl;
use crate::{EqualizerSettings, Error, Result, StreamResolver, Track, TrackDecoder};

const TICK: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, PartialEq, Default)]
pub enum PlaybackStatus {
    #[default]
    Idle,
    /// Resolving + downloading the current track.
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
    pub position: Duration,
    pub duration: Option<Duration>,
    /// User volume, 0.0..=1.0 (loudness normalization is applied on top).
    pub volume: f32,
    /// Only the start of the track could be fetched (no PoToken); playback ends early.
    pub preview_only: bool,
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
    Loaded {
        generation: u64,
        audio: Vec<u8>,
        complete: bool,
        loudness_db: Option<f32>,
    },
    LoadFailed {
        generation: u64,
        message: String,
    },
}

/// Background audio engine. Cloning shares the same engine.
#[derive(Clone)]
pub struct Player {
    tx: Sender<Command>,
    state: Arc<Mutex<PlaybackState>>,
    eq: Arc<EqControl>,
}

impl Player {
    /// Opens the default output device on a dedicated audio thread.
    pub fn new(resolver: Arc<dyn StreamResolver>) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let state = Arc::new(Mutex::new(PlaybackState::default()));
        let eq = Arc::new(EqControl::default());
        let (ready_tx, ready_rx) = mpsc::channel();
        let engine = Engine {
            tx: tx.clone(),
            state: state.clone(),
            eq: eq.clone(),
            resolver,
            generation: 0,
            gain: 1.0,
            sink: None,
            player: None,
            current: None,
        };
        thread::Builder::new()
            .name("music-core-audio".into())
            .spawn(move || engine.run(rx, ready_tx))
            .map_err(|e| Error::Audio(e.to_string()))?;
        ready_rx
            .recv()
            .map_err(|_| Error::Audio("audio thread died".into()))??;
        Ok(Self { tx, state, eq })
    }

    /// Replaces whatever is playing with `track`.
    pub fn play(&self, track: Track) {
        let _ = self.tx.send(Command::Play(track));
    }

    pub fn toggle_pause(&self) {
        let _ = self.tx.send(Command::TogglePause);
    }

    /// Pauses if playing; otherwise does nothing.
    pub fn pause(&self) {
        let _ = self.tx.send(Command::Pause);
    }

    /// Resumes if paused, or replays the track if it ended; otherwise does nothing.
    pub fn resume(&self) {
        let _ = self.tx.send(Command::Resume);
    }

    pub fn seek(&self, position: Duration) {
        let _ = self.tx.send(Command::Seek(position));
    }

    pub fn set_volume(&self, volume: f32) {
        let _ = self.tx.send(Command::SetVolume(volume.clamp(0.0, 1.0)));
    }

    pub fn state(&self) -> PlaybackState {
        self.state.lock().unwrap().clone()
    }

    /// Applies `settings` to the playing track within a few milliseconds
    /// (smoothed, no restart) and to every track after it.
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
}

#[derive(Clone)]
struct Loaded {
    audio: Arc<[u8]>,
    loudness_db: Option<f32>,
}

struct Engine {
    tx: Sender<Command>,
    state: Arc<Mutex<PlaybackState>>,
    eq: Arc<EqControl>,
    resolver: Arc<dyn StreamResolver>,
    /// Bumped on every Play so late downloads for skipped tracks are dropped.
    generation: u64,
    /// Loudness-normalization multiplier for the current track.
    gain: f32,
    sink: Option<MixerDeviceSink>,
    player: Option<rodio::Player>,
    /// Downloaded audio of the current track, kept so "play again" needs no refetch.
    current: Option<Loaded>,
}

impl Engine {
    fn run(mut self, rx: Receiver<Command>, ready: Sender<Result<()>>) {
        match DeviceSinkBuilder::open_default_sink() {
            Ok(mut sink) => {
                sink.log_on_drop(false);
                self.sink = Some(sink);
                let _ = ready.send(Ok(()));
            }
            Err(e) => {
                let _ = ready.send(Err(Error::Audio(e.to_string())));
                return;
            }
        }
        loop {
            match rx.recv_timeout(TICK) {
                Ok(cmd) => self.handle(cmd),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            self.tick();
        }
    }

    fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Play(track) => self.start_loading(track),
            Command::TogglePause | Command::Pause | Command::Resume => {
                let status = self.state.lock().unwrap().status.clone();
                match (status, &self.player) {
                    (PlaybackStatus::Playing, _) if matches!(cmd, Command::Resume) => {}
                    (PlaybackStatus::Paused | PlaybackStatus::Ended, _)
                        if matches!(cmd, Command::Pause) => {}
                    (PlaybackStatus::Playing, Some(player)) => {
                        player.pause();
                        self.state.lock().unwrap().status = PlaybackStatus::Paused;
                    }
                    (PlaybackStatus::Paused, Some(player)) => {
                        player.play();
                        self.state.lock().unwrap().status = PlaybackStatus::Playing;
                    }
                    (PlaybackStatus::Ended, _) => {
                        if let Some(loaded) = self.current.clone() {
                            let result = self.start_playback(loaded);
                            self.report(result);
                        }
                    }
                    _ => {}
                }
            }
            Command::Seek(position) => {
                if let Some(player) = &self.player
                    && let Err(e) = player.try_seek(position)
                {
                    log::warn!("seek failed: {e}");
                }
            }
            Command::SetVolume(volume) => {
                self.state.lock().unwrap().volume = volume;
                self.apply_volume();
            }
            Command::Loaded {
                generation,
                audio,
                complete,
                loudness_db,
            } if generation == self.generation => {
                self.state.lock().unwrap().preview_only = !complete;
                let loaded = Loaded {
                    audio: audio.into(),
                    loudness_db,
                };
                self.current = Some(loaded.clone());
                let result = self.start_playback(loaded);
                self.report(result);
            }
            Command::LoadFailed {
                generation,
                message,
            } if generation == self.generation => {
                self.state.lock().unwrap().status = PlaybackStatus::Error(message);
            }
            Command::Loaded { .. } | Command::LoadFailed { .. } => {}
        }
    }

    fn start_loading(&mut self, track: Track) {
        self.generation += 1;
        // Dropping the rodio player stops the previous track immediately.
        self.player = None;
        self.current = None;
        {
            let mut state = self.state.lock().unwrap();
            state.track = Some(track.clone());
            state.status = PlaybackStatus::Loading;
            state.position = Duration::ZERO;
            state.preview_only = false;
            state.duration = track.duration_secs.map(|s| Duration::from_secs(s.into()));
        }
        let (tx, resolver, generation) = (self.tx.clone(), self.resolver.clone(), self.generation);
        thread::spawn(move || {
            let result = resolver.resolve(&track.video_id).and_then(|stream| {
                let download = stream.download()?;
                Ok((download, stream.loudness_db))
            });
            let _ = tx.send(match result {
                Ok((download, loudness_db)) => Command::Loaded {
                    generation,
                    audio: download.bytes,
                    complete: download.complete,
                    loudness_db,
                },
                Err(e) => Command::LoadFailed {
                    generation,
                    message: e.to_string(),
                },
            });
        });
    }

    fn report(&self, result: Result<()>) {
        if let Err(e) = result {
            self.state.lock().unwrap().status = PlaybackStatus::Error(e.to_string());
        }
    }

    fn start_playback(&mut self, loaded: Loaded) -> Result<()> {
        let Loaded { audio, loudness_db } = loaded;
        // Symphonia 0.6 opens truncated previews seekably, so seeking works in them too.
        let decoder = TrackDecoder::new(audio)?.with_equalizer(self.eq.clone());
        let duration = decoder.total_duration();

        let sink = self
            .sink
            .as_ref()
            .ok_or_else(|| Error::Audio("no output device".into()))?;
        let player = rodio::Player::connect_new(sink.mixer());
        player.append(decoder);
        // Only attenuate tracks louder than YouTube's reference; boosting quiet ones would clip.
        self.gain = loudness_db
            .filter(|db| *db > 0.0)
            .map_or(1.0, |db| 10f32.powf(-db / 20.0));
        self.player = Some(player);
        self.apply_volume();

        let mut state = self.state.lock().unwrap();
        state.status = PlaybackStatus::Playing;
        if duration.is_some() {
            state.duration = duration;
        }
        Ok(())
    }

    fn apply_volume(&self) {
        if let Some(player) = &self.player {
            let volume = self.state.lock().unwrap().volume;
            player.set_volume(volume * self.gain);
        }
    }

    fn tick(&mut self) {
        let Some(player) = &self.player else { return };
        let mut state = self.state.lock().unwrap();
        state.position = player.get_pos();
        if state.status == PlaybackStatus::Playing && player.empty() {
            state.status = PlaybackStatus::Ended;
        }
    }
}
