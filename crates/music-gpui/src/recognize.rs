//! Song recognition session (the model behind the Recognition page and its shortcut).
//!
//! One [`Recognizer`] entity per app (a global): **Listen** checks the microphone
//! permission (`mic_permission.rs`), records [`recognition::DEFAULT_CAPTURE`] on a
//! worker thread (cancellable; the level is polled for the meter), sends the
//! signature, stores a match in the library's recognition history and searches
//! YouTube Music for it so the result can be played or queued. A file can stand in
//! for the microphone (`@recognize-file=PATH` script action, tests, screenshots).
//!
//! Ported in spirit from ArchiveTune's `MusicRecognitionScreen` / use cases (GPL-3.0).

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use gpui::{App, AppContext as _, Context, Entity, Global, Task};
use music_core::Track;
use music_core::library::now_ms;
use music_core::recognition::{
    self, Recognition, RecognitionEntry, RecognitionError, ShazamClient, capture,
};

use crate::app::read_app;
use crate::browse::Load;
use crate::mic_permission::{self, MicPermission};
use crate::util::off_thread;

/// Where a session stands.
pub enum Phase {
    Idle,
    /// Waiting for the OS permission prompt.
    Permission,
    Listening {
        elapsed: f32,
        length: f32,
        level: f32,
    },
    Identifying,
    Found {
        result: Box<Recognition>,
        /// YouTube Music search for the result (best match first).
        matches: Load<Rc<Vec<Track>>>,
        /// From the history list rather than a fresh recognition.
        from_history: bool,
    },
    Failed {
        message: String,
        /// The microphone permission is the problem (show "Open settings").
        permission: bool,
    },
}

impl Phase {
    pub fn busy(&self) -> bool {
        matches!(
            self,
            Phase::Permission | Phase::Listening { .. } | Phase::Identifying
        )
    }
}

pub struct Recognizer {
    pub phase: Phase,
    pub history: Vec<RecognitionEntry>,
    cancel: Arc<AtomicBool>,
    level: Arc<Mutex<Option<capture::CaptureProgress>>>,
    task: Option<Task<()>>,
    search_task: Option<Task<()>>,
    meter: Option<Task<()>>,
}

struct RecognizerHandle(Entity<Recognizer>);
impl Global for RecognizerHandle {}

/// What the session listens to.
enum Source {
    Microphone,
    File(PathBuf),
}

impl Recognizer {
    /// The app-wide recognizer (created on first use).
    pub fn global(cx: &mut App) -> Entity<Recognizer> {
        if let Some(handle) = cx.try_global::<RecognizerHandle>() {
            return handle.0.clone();
        }
        let entity = cx.new(|_| Self {
            phase: Phase::Idle,
            history: Vec::new(),
            cancel: Arc::default(),
            level: Arc::default(),
            task: None,
            search_task: None,
            meter: None,
        });
        cx.set_global(RecognizerHandle(entity.clone()));
        // Later: the first caller may be a page built while the app is being updated,
        // and the history comes from the app's library.
        let weak = entity.downgrade();
        cx.defer(move |cx| {
            weak.update(cx, |this, cx| this.reload_history(cx)).ok();
        });
        entity
    }

    fn library(cx: &App) -> Option<music_core::Library> {
        read_app(cx, |app, _| app.library().cloned()).flatten()
    }

    pub fn reload_history(&mut self, cx: &mut Context<Self>) {
        self.history = Self::library(cx)
            .and_then(|lib| lib.recognition_history().ok())
            .unwrap_or_default();
        cx.notify();
    }

    /// Listen through the microphone.
    pub fn listen(&mut self, cx: &mut Context<Self>) {
        self.start(Source::Microphone, cx);
    }

    /// Recognize the first seconds of a local file (tests / screenshots / "Identify a file").
    pub fn recognize_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.start(Source::File(path), cx);
    }

    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::Relaxed);
        if self.phase.busy() {
            self.task = None;
            self.meter = None;
            self.phase = Phase::Idle;
            cx.notify();
        }
    }

    /// Shows a history entry as the current result (and searches it).
    pub fn show(&mut self, entry: &RecognitionEntry, cx: &mut Context<Self>) {
        if self.phase.busy() {
            return;
        }
        self.found(entry.result.clone(), true, cx);
    }

    pub fn remove(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(lib) = Self::library(cx) {
            let _ = lib.remove_recognition(key);
        }
        self.reload_history(cx);
    }

    pub fn clear_history(&mut self, cx: &mut Context<Self>) {
        if let Some(lib) = Self::library(cx) {
            let _ = lib.clear_recognition_history();
        }
        self.reload_history(cx);
    }

    fn start(&mut self, source: Source, cx: &mut Context<Self>) {
        if self.phase.busy() {
            return;
        }
        self.cancel = Arc::new(AtomicBool::new(false));
        *self.level.lock().unwrap_or_else(PoisonError::into_inner) = None;
        let cancel = self.cancel.clone();
        let level = self.level.clone();
        let length = recognition::DEFAULT_CAPTURE;
        let mic = matches!(source, Source::Microphone);
        self.phase = if mic {
            Phase::Permission
        } else {
            Phase::Identifying
        };
        cx.notify();
        self.task = Some(cx.spawn(async move |this, cx| {
            if mic {
                let permission = off_thread(mic_permission::ensure).await;
                if permission != MicPermission::Granted {
                    this.update(cx, |this, cx| {
                        this.fail(
                            match permission {
                                MicPermission::Restricted => {
                                    t!("recognition.mic.restricted").to_owned()
                                }
                                MicPermission::Undetermined => {
                                    t!("recognition.mic.undetermined").to_owned()
                                }
                                _ => t!("recognition.mic.denied").to_owned(),
                            },
                            true,
                            cx,
                        )
                    })
                    .ok();
                    return;
                }
                this.update(cx, |this, cx| {
                    this.phase = Phase::Listening {
                        elapsed: 0.,
                        length: length.as_secs_f32(),
                        level: 0.,
                    };
                    this.start_meter(cx);
                    cx.notify();
                })
                .ok();
            }
            let result = off_thread(move || -> Result<Recognition, RecognitionError> {
                let pcm = match source {
                    Source::Microphone => capture::record_microphone(length, &cancel, |p| {
                        *level.lock().unwrap_or_else(PoisonError::into_inner) = Some(p);
                    })?,
                    Source::File(path) => recognition::audio::decode_file(
                        &path,
                        Duration::ZERO,
                        Duration::from_secs(12),
                    )
                    .map_err(|e| RecognitionError::Capture(e.to_string()))?,
                };
                if cancel.load(Ordering::Relaxed) {
                    return Err(RecognitionError::Cancelled);
                }
                recognition::recognize_pcm(&ShazamClient::default(), &pcm)
            })
            .await;
            this.update(cx, |this, cx| {
                this.meter = None;
                match result {
                    Ok(song) => {
                        if let Some(lib) = Self::library(cx) {
                            let _ = lib.add_recognition(&song, now_ms());
                        }
                        this.reload_history(cx);
                        this.found(song, false, cx);
                    }
                    Err(RecognitionError::Cancelled) => {
                        this.phase = Phase::Idle;
                        cx.notify();
                    }
                    Err(e) => {
                        let permission = matches!(
                            e,
                            RecognitionError::PermissionDenied | RecognitionError::Silent
                        );
                        this.fail(error_message(&e), permission, cx);
                    }
                }
            })
            .ok();
        }));
    }

    /// Re-renders ~10× a second with the capture progress while listening.
    fn start_meter(&mut self, cx: &mut Context<Self>) {
        let executor = cx.background_executor().clone();
        let level = self.level.clone();
        self.meter = Some(cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(100)).await;
                let progress = *level.lock().unwrap_or_else(PoisonError::into_inner);
                let alive = this.update(cx, |this, cx| {
                    if let (Phase::Listening { elapsed, level, .. }, Some(p)) =
                        (&mut this.phase, progress)
                    {
                        *elapsed = p.elapsed.as_secs_f32();
                        *level = p.level;
                        if p.elapsed >= p.length {
                            this.phase = Phase::Identifying;
                        }
                        cx.notify();
                    }
                    matches!(this.phase, Phase::Listening { .. })
                });
                if !matches!(alive, Ok(true)) {
                    break;
                }
            }
        }));
    }

    fn fail(&mut self, message: String, permission: bool, cx: &mut Context<Self>) {
        self.phase = Phase::Failed {
            message,
            permission,
        };
        cx.notify();
    }

    fn found(&mut self, result: Recognition, from_history: bool, cx: &mut Context<Self>) {
        let query = result.search_query();
        self.phase = Phase::Found {
            result: Box::new(result),
            matches: Load::Loading,
            from_history,
        };
        cx.notify();
        let (client, filter) = crate::browse::client(cx);
        self.search_task = Some(cx.spawn(async move |this, cx| {
            let found = off_thread(move || client.search_songs(&query).map_err(|e| e.to_string()))
                .await
                .map(|tracks| Rc::new(filter.tracks(tracks)));
            this.update(cx, |this, cx| {
                if let Phase::Found { matches, .. } = &mut this.phase {
                    *matches = Load::from_result(found);
                    cx.notify();
                }
            })
            .ok();
        }));
    }
}
/// What went wrong, for the recognition page.
fn error_message(e: &RecognitionError) -> String {
    match e {
        RecognitionError::NoMicrophone => t!("recognition.error.no_microphone").to_owned(),
        RecognitionError::PermissionDenied => t!("recognition.error.permission_denied").to_owned(),
        RecognitionError::Silent => t!("recognition.error.silent").to_owned(),
        RecognitionError::Capture(error) => t!("recognition.error.capture", error = error),
        RecognitionError::Cancelled => t!("recognition.error.cancelled").to_owned(),
        RecognitionError::NoSignature => t!("recognition.error.no_signature").to_owned(),
        RecognitionError::NoMatch => t!("recognition.error.no_match").to_owned(),
        RecognitionError::RateLimited => t!("recognition.error.rate_limited").to_owned(),
        RecognitionError::Service(status) => t!("recognition.error.service", status = status),
        RecognitionError::Network(error) => t!("recognition.error.network", error = error),
        RecognitionError::Parse(error) => t!("recognition.error.parse", error = error),
    }
}
