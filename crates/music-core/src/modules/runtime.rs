// JavaScript runtime ported from ArchiveTune
// modules/src/main/kotlin/moe/rukamori/archivetune/modules/ModuleRuntime.kt (engine setup,
// call envelope, search / stream result normalization) — © Rukamori and ArchiveTune
// contributors, GPL-3.0
//! One QuickJS engine (rquickjs) per installed module, each on its own thread.
//!
//! Sandbox: an engine has the ECMAScript built-ins plus the host bindings of
//! `host.js` (`fetch`, `Headers`, `URL`, `URLSearchParams`, `TextEncoder` /
//! `TextDecoder`, `atob` / `btoa`, `Blob`, `FormData`, `setTimeout`,
//! `AbortController`, `console`, and the extras `storage`, `utils`, `crypto`).
//! There is no filesystem, module loader (`import()` rejects, no `require`),
//! process or OS access. Limits: 32 MiB heap, 1 MiB stack, an interrupt
//! handler that stops runaway scripts at the call's deadline, at most 128
//! timers (≤ 15 s each), 8 concurrent / 64 total requests per call.
//!
//! Calls are blocking for the caller: the job goes to the engine thread, which
//! runs the provider's (possibly async) method and drives its promises, timers
//! and requests until the result settles or the deadline passes. An engine
//! that failed is discarded and recreated on the next call.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use rquickjs::context::EvalOptions;
use rquickjs::function::Opt;
use rquickjs::{CatchResultExt, CaughtError, Context, Ctx, Exception, Function, Runtime};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::error::{Failure, ModuleError, ModuleResult, Stage};
use super::http::{self, MAX_BODY_BYTES, Request, web_url};
use super::json::{opt_str, text};
use super::manifest::KNOWN_CAPABILITIES;
use super::registry::{LogBuffer, ModuleStorage};

const HOST_JS: &str = include_str!("host.js");
const MEMORY_LIMIT: usize = 32 * 1024 * 1024;
const STACK_LIMIT: usize = 1024 * 1024;
/// The engine thread's own stack (QuickJS's limit is measured inside it).
const THREAD_STACK: usize = 8 * 1024 * 1024;
const MAX_TIMERS: usize = 128;
const MAX_ACTIVE_REQUESTS: usize = 8;
const MAX_ADMITTED_REQUESTS: usize = 64;
/// Engines kept alive (least recently used are closed).
const MAX_ENGINES: usize = 4;

/// A search result (MODULES.md "Track search").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModuleTrack {
    pub id: String,
    pub title: String,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub duration_secs: Option<u32>,
    pub explicit: Option<bool>,
    pub isrc: Option<String>,
}

/// A resolved stream (MODULES.md "Stream resolution and lossless audio").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModuleStream {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub mime_type: String,
    pub codecs: String,
    /// Bits per second, 0 = unknown.
    pub bitrate: u32,
    pub sample_rate: Option<u32>,
    /// Bytes, 0 = unknown.
    pub content_length: u64,
    pub expires_at_ms: u64,
    pub variant: String,
    pub upstream_provider: String,
    pub upstream_id: String,
}

impl ModuleStream {
    /// HLS / DASH manifests, which this player cannot stream.
    pub fn is_manifest(&self) -> bool {
        matches!(
            self.mime_type.to_ascii_lowercase().as_str(),
            "application/x-mpegurl" | "application/vnd.apple.mpegurl" | "application/dash+xml"
        )
    }
}

/// What an engine runs: one installed module at one revision.
#[derive(Clone)]
pub(crate) struct EngineSpec {
    /// Installation id + revision; a new revision gets a new engine.
    pub key: String,
    pub installation: String,
    pub script: Arc<str>,
    pub metadata: Value,
    /// Base for relative URLs (the import URL; empty for file imports).
    pub origin: String,
    pub storage: Arc<ModuleStorage>,
    pub log: Arc<LogBuffer>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Job {
    function: String,
    args: Value,
    deadline: Instant,
    reply: Sender<ModuleResult<Value>>,
}

struct Handle {
    key: String,
    installation: String,
    jobs: Sender<Job>,
    cancel: Arc<AtomicBool>,
}

/// The engines of every module, created on demand.
#[derive(Default)]
pub(crate) struct Engines {
    handles: Mutex<Vec<Handle>>,
}

impl Engines {
    /// Calls `function(...args)` on the module's engine; blocks until the
    /// result settles or `deadline`.
    pub(crate) fn call(
        &self,
        spec: &EngineSpec,
        function: &str,
        args: Value,
        deadline: Instant,
    ) -> ModuleResult<Value> {
        let (reply_tx, reply_rx) = mpsc::channel();
        let job = Job {
            function: function.to_owned(),
            args,
            deadline,
            reply: reply_tx,
        };
        let (jobs, cancel) = {
            let mut handles = lock(&self.handles);
            let index = match handles.iter().position(|h| h.key == spec.key) {
                Some(i) => i,
                None => {
                    let handle = spawn(spec.clone())?;
                    handles.push(handle);
                    while handles.len() > MAX_ENGINES {
                        let old = handles.remove(0);
                        old.cancel.store(true, Ordering::Relaxed);
                    }
                    handles.len() - 1
                }
            };
            // Most recently used last.
            let handle = handles.remove(index);
            let pair = (handle.jobs.clone(), handle.cancel.clone());
            handles.push(handle);
            pair
        };
        if jobs.send(job).is_err() {
            self.discard(&spec.key);
            return Err(ModuleError::at(
                Failure::Execution,
                Stage::Engine,
                "ENGINE_CLOSED",
            ));
        }
        let wait = deadline.saturating_duration_since(Instant::now()) + Duration::from_secs(2);
        let result = match reply_rx.recv_timeout(wait) {
            Ok(result) => result,
            Err(_) => {
                cancel.store(true, Ordering::Relaxed);
                Err(ModuleError::at(
                    Failure::Timeout,
                    Stage::Provider,
                    "RESOLUTION_TIMEOUT",
                ))
            }
        };
        if result.is_err() {
            self.discard(&spec.key);
        }
        result
    }

    /// Creates an engine just to check the package (exports match the
    /// declared capabilities) and closes it again.
    pub(crate) fn inspect(spec: &EngineSpec, deadline: Instant) -> ModuleResult<()> {
        let (tx, rx) = mpsc::channel();
        let spec = spec.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        thread::Builder::new()
            .name("music-core-module-inspect".into())
            .stack_size(THREAD_STACK)
            .spawn(move || {
                let result = Engine::create(&spec, flag, deadline).map(drop);
                let _ = tx.send(result);
            })
            .map_err(|_| ModuleError::at(Failure::Execution, Stage::Engine, "THREAD"))?;
        let wait = deadline.saturating_duration_since(Instant::now()) + Duration::from_secs(2);
        rx.recv_timeout(wait).unwrap_or_else(|_| {
            cancel.store(true, Ordering::Relaxed);
            Err(ModuleError::at(
                Failure::Timeout,
                Stage::Provider,
                "INIT_TIMEOUT",
            ))
        })
    }

    fn discard(&self, key: &str) {
        lock(&self.handles).retain(|h| {
            let keep = h.key != key;
            if !keep {
                h.cancel.store(true, Ordering::Relaxed);
            }
            keep
        });
    }

    /// Closes the engines of an installation (settings changed, updated, removed).
    pub(crate) fn unload(&self, installation: &str) {
        lock(&self.handles).retain(|h| {
            let keep = h.installation != installation;
            if !keep {
                h.cancel.store(true, Ordering::Relaxed);
            }
            keep
        });
    }
}

fn spawn(spec: EngineSpec) -> ModuleResult<Handle> {
    let (tx, rx) = mpsc::channel::<Job>();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let (key, installation) = (spec.key.clone(), spec.installation.clone());
    thread::Builder::new()
        .name("music-core-module".into())
        .stack_size(THREAD_STACK)
        .spawn(move || engine_thread(spec, rx, flag))
        .map_err(|_| ModuleError::at(Failure::Execution, Stage::Engine, "THREAD"))?;
    Ok(Handle {
        key,
        installation,
        jobs: tx,
        cancel,
    })
}

fn engine_thread(spec: EngineSpec, jobs: Receiver<Job>, cancel: Arc<AtomicBool>) {
    let mut engine: Option<Engine> = None;
    while let Ok(job) = jobs.recv() {
        if cancel.load(Ordering::Relaxed) {
            let _ = job.reply.send(Err(ModuleError::at(
                Failure::Execution,
                Stage::Engine,
                "ENGINE_CLOSED",
            )));
            return;
        }
        if engine.is_none() {
            match Engine::create(&spec, cancel.clone(), job.deadline) {
                Ok(e) => engine = Some(e),
                Err(e) => {
                    let _ = job.reply.send(Err(e));
                    return;
                }
            }
        }
        let Some(e) = engine.as_mut() else { return };
        let result = e.call(&job.function, &job.args, job.deadline);
        let failed = result.is_err();
        let _ = job.reply.send(result);
        if failed {
            // Like the reference, an engine is not reused after a failure.
            return;
        }
    }
}

/// A request waiting for a free slot.
struct Queued {
    id: String,
    request: Request,
}

enum Completion {
    Done {
        id: String,
        result: ModuleResult<http::Response>,
    },
}

/// State shared by the host functions of one engine.
struct Host {
    base: Instant,
    /// Milliseconds after `base`.
    deadline_ms: AtomicU64,
    cancel: Arc<AtomicBool>,
    origin: String,
    timers: Mutex<BTreeMap<u64, Instant>>,
    queued: Mutex<VecDeque<Queued>>,
    active: Mutex<HashMap<String, Arc<AtomicBool>>>,
    aborted: Mutex<Vec<String>>,
    admitted: AtomicUsize,
    /// The last request problem, used to classify a generic `Error`.
    problem: Mutex<Option<(Failure, String)>>,
    storage: Arc<ModuleStorage>,
    log: Arc<LogBuffer>,
}

impl Host {
    fn deadline(&self) -> Instant {
        self.base + Duration::from_millis(self.deadline_ms.load(Ordering::Relaxed))
    }

    fn set_deadline(&self, deadline: Instant) {
        let ms = deadline.saturating_duration_since(self.base).as_millis() as u64;
        self.deadline_ms.store(ms, Ordering::Relaxed);
    }

    fn interrupted(&self) -> bool {
        self.cancel.load(Ordering::Relaxed) || Instant::now() >= self.deadline()
    }

    fn record_problem(&self, failure: Failure, code: String) {
        *lock(&self.problem) = Some((failure, code));
    }

    /// Cancels everything a finished call left behind.
    fn reset(&self) {
        lock(&self.timers).clear();
        lock(&self.queued).clear();
        for (_, flag) in lock(&self.active).drain() {
            flag.store(true, Ordering::Relaxed);
        }
        lock(&self.aborted).clear();
        self.admitted.store(0, Ordering::Relaxed);
    }
}

struct Engine {
    context: Context,
    runtime: Runtime,
    host: Arc<Host>,
    completions: Receiver<Completion>,
    completion_tx: Sender<Completion>,
}

fn eval_options(filename: &str) -> EvalOptions {
    let mut options = EvalOptions::default();
    options.global = true;
    options.strict = false;
    options.filename = Some(filename.to_owned());
    options
}

/// `Name/code/module-provider.js:12` from a caught exception.
fn describe_exception(ctx: &Ctx<'_>, error: CaughtError<'_>, host: &Host) -> (String, String) {
    let _ = ctx;
    match error {
        CaughtError::Exception(e) => {
            let name: String = e
                .as_object()
                .get::<_, Option<String>>("name")
                .ok()
                .flatten()
                .unwrap_or_else(|| "Error".into());
            let message = e.message().unwrap_or_default();
            let stack = e.stack().unwrap_or_default();
            (name, format!("{message} {stack}"))
        }
        CaughtError::Value(v) => ("Error".into(), format!("{v:?}")),
        CaughtError::Error(e) => {
            let text = e.to_string();
            host.log.push("error", &text);
            ("InternalError".into(), text)
        }
    }
}

fn frame(stack: &str) -> String {
    static FRAME: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"module-(?:host|provider|call)\.js:\d+(?::\d+)?").unwrap()
    });
    FRAME
        .find(stack)
        .map(|m| m.as_str().to_owned())
        .unwrap_or_default()
}

impl Engine {
    fn create(spec: &EngineSpec, cancel: Arc<AtomicBool>, deadline: Instant) -> ModuleResult<Self> {
        let engine_err = |code: &str| ModuleError::at(Failure::Execution, Stage::Engine, code);
        let runtime = Runtime::new().map_err(|_| engine_err("RUNTIME"))?;
        runtime.set_memory_limit(MEMORY_LIMIT);
        runtime.set_max_stack_size(STACK_LIMIT);
        let base = Instant::now();
        let host = Arc::new(Host {
            base,
            deadline_ms: AtomicU64::new(0),
            cancel,
            origin: spec.origin.clone(),
            timers: Mutex::default(),
            queued: Mutex::default(),
            active: Mutex::default(),
            aborted: Mutex::default(),
            admitted: AtomicUsize::new(0),
            problem: Mutex::default(),
            storage: spec.storage.clone(),
            log: spec.log.clone(),
        });
        host.set_deadline(deadline);
        let interrupt = host.clone();
        runtime.set_interrupt_handler(Some(Box::new(move || interrupt.interrupted())));
        let context = Context::full(&runtime).map_err(|_| engine_err("CONTEXT"))?;
        let (completion_tx, completions) = mpsc::channel();
        let engine = Self {
            context,
            runtime,
            host: host.clone(),
            completions,
            completion_tx,
        };
        engine
            .install_bindings()
            .map_err(|_| engine_err("BINDINGS"))?;
        engine.eval(HOST_JS, "module-host.js", Stage::Host)?;
        let metadata = serde_json::to_string(&spec.metadata).unwrap_or_else(|_| "{}".into());
        let known = serde_json::to_string(&KNOWN_CAPABILITIES).unwrap_or_default();
        let wrapper = format!(
            "globalThis.__provider = (function() {{\n\
             const module = {{exports: {{}}}}; const exports = module.exports; const self = globalThis;\n\
             (function() {{\n{script}\n}})();\n\
             const provider = module.exports;\n\
             const metadata = JSON.parse({metadata});\n\
             const known = {known};\n\
             if (!provider || typeof provider !== 'object' || Array.isArray(provider)) throw new TypeError('Invalid provider export');\n\
             const actual = known.filter(key => typeof provider[key] === 'function');\n\
             if (actual.length !== metadata.capabilities.length || metadata.capabilities.some(key => !actual.includes(key))) throw new TypeError('Provider capability mismatch');\n\
             return provider;\n\
             }})();\n\"module-provider-ready\";",
            script = spec.script,
            metadata = serde_json::to_string(&metadata).unwrap_or_default(),
        );
        engine.eval(&wrapper, "module-provider.js", Stage::Provider)?;
        // Promise jobs queued by top-level code (e.g. an async IIFE) run now.
        engine.drain_jobs();
        Ok(engine)
    }

    fn eval(&self, source: &str, filename: &str, stage: Stage) -> ModuleResult<()> {
        let host = self.host.clone();
        let result = self.context.with(|ctx| {
            ctx.eval_with_options::<(), _>(source, eval_options(filename))
                .catch(&ctx)
                .map_err(|e| describe_exception(&ctx, e, &host))
        });
        result.map_err(|(name, detail)| {
            if self.host.interrupted() {
                return ModuleError::at(Failure::Timeout, stage, "INIT_TIMEOUT");
            }
            if detail.contains("out of memory") {
                return ModuleError::at(Failure::Execution, stage, "OUT_OF_MEMORY");
            }
            self.host
                .log
                .push("error", &format!("{name}: {}", detail.trim()));
            let code = [name, frame(&detail)]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("/");
            ModuleError::at(Failure::Execution, stage, code)
        })
    }

    fn install_bindings(&self) -> rquickjs::Result<()> {
        let host = self.host.clone();
        self.context.with(|ctx| {
            let g = ctx.globals();
            let h = host.clone();
            g.set(
                "__moduleFetchStart",
                Function::new(
                    ctx.clone(),
                    move |ctx: Ctx<'_>, json: String| -> rquickjs::Result<()> {
                        let input: Value = serde_json::from_str(&json)
                            .map_err(|_| Exception::throw_type(&ctx, "Invalid request"))?;
                        let id = opt_str(&input, "id").to_owned();
                        let request = Request::from_json(&input, &h.origin).map_err(|e| {
                            h.record_problem(e.failure, "REQUEST_INVALID_DOCUMENT".into());
                            Exception::throw_type(&ctx, "Invalid request")
                        })?;
                        if h.admitted.fetch_add(1, Ordering::Relaxed) >= MAX_ADMITTED_REQUESTS {
                            h.record_problem(Failure::Execution, "REQUEST_LIMIT".into());
                            return Err(Exception::throw_type(&ctx, "Too many requests"));
                        }
                        lock(&h.queued).push_back(Queued { id, request });
                        Ok(())
                    },
                )?,
            )?;
            let h = host.clone();
            g.set(
                "__moduleAbort",
                Function::new(ctx.clone(), move |id: String| {
                    let mut queued = lock(&h.queued);
                    let before = queued.len();
                    queued.retain(|q| q.id != id);
                    let was_queued = queued.len() != before;
                    drop(queued);
                    let active = lock(&h.active).remove(&id);
                    if let Some(flag) = &active {
                        flag.store(true, Ordering::Relaxed);
                    }
                    if was_queued || active.is_some() {
                        lock(&h.aborted).push(id);
                    }
                })?,
            )?;
            let h = host.clone();
            g.set(
                "__moduleTimerStart",
                Function::new(
                    ctx.clone(),
                    move |ctx: Ctx<'_>, id: f64, millis: f64| -> rquickjs::Result<()> {
                        let mut timers = lock(&h.timers);
                        if timers.len() >= MAX_TIMERS {
                            return Err(Exception::throw_type(&ctx, "Invalid timer"));
                        }
                        let millis = if millis.is_finite() {
                            millis.clamp(0.0, 15_000.0)
                        } else {
                            0.0
                        };
                        timers.insert(
                            id as u64,
                            Instant::now() + Duration::from_millis(millis as u64),
                        );
                        Ok(())
                    },
                )?,
            )?;
            let h = host.clone();
            g.set(
                "__moduleClearTimer",
                Function::new(ctx.clone(), move |id: f64| {
                    lock(&h.timers).remove(&(id as u64));
                })?,
            )?;
            let h = host.clone();
            g.set(
                "__moduleUrl",
                Function::new(
                    ctx.clone(),
                    move |ctx: Ctx<'_>,
                          input: String,
                          base: Opt<String>|
                          -> rquickjs::Result<String> {
                        let base = base.0.unwrap_or_else(|| h.origin.clone());
                        http::url_parts(&input, Some(&base))
                            .map(|v| v.to_string())
                            .map_err(|_| Exception::throw_type(&ctx, "Invalid URL"))
                    },
                )?,
            )?;
            let h = host.clone();
            g.set(
                "__moduleLog",
                Function::new(ctx.clone(), move |level: String, text: String| {
                    h.log.push(&level, &text);
                })?,
            )?;
            let h = host.clone();
            g.set(
                "__moduleStorage",
                Function::new(
                    ctx.clone(),
                    move |ctx: Ctx<'_>, json: String| -> rquickjs::Result<String> {
                        let input: Value = serde_json::from_str(&json)
                            .map_err(|_| Exception::throw_type(&ctx, "Invalid storage call"))?;
                        h.storage.call(&input).map(|v| v.to_string()).map_err(|e| {
                            Exception::throw_message(&ctx, &format!("Storage failed: {e}"))
                        })
                    },
                )?,
            )?;
            g.set(
                "__moduleDigest",
                Function::new(
                    ctx.clone(),
                    move |ctx: Ctx<'_>,
                          algorithm: String,
                          data: String|
                          -> rquickjs::Result<String> {
                        let bytes = base64::engine::general_purpose::STANDARD
                            .decode(data)
                            .map_err(|_| Exception::throw_type(&ctx, "Invalid data"))?;
                        Ok(match algorithm.as_str() {
                            "sha256" => super::json::sha256_hex(&bytes),
                            "sha1" => sha1_smol::Sha1::from(&bytes).digest().to_string(),
                            "md5" => format!("{:x}", md5::compute(&bytes)),
                            _ => return Err(Exception::throw_type(&ctx, "Unsupported algorithm")),
                        })
                    },
                )?,
            )?;
            g.set(
                "__moduleRandom",
                Function::new(ctx.clone(), move |n: usize| -> String {
                    let mut bytes = vec![0u8; n.min(65_536)];
                    let _ = getrandom::fill(&mut bytes);
                    base64::engine::general_purpose::STANDARD.encode(bytes)
                })?,
            )?;
            g.set(
                "__moduleUuid",
                Function::new(ctx.clone(), super::json::uuid_v4)?,
            )?;
            Ok(())
        })
    }

    /// Runs queued promise jobs.
    fn drain_jobs(&self) {
        while self.runtime.is_job_pending() {
            match self.runtime.execute_pending_job() {
                Ok(true) => {}
                Ok(false) => break,
                Err(_) => {
                    if self.host.interrupted() {
                        break;
                    }
                }
            }
        }
    }

    /// Starts queued requests while slots are free.
    fn start_requests(&self) {
        loop {
            if lock(&self.host.active).len() >= MAX_ACTIVE_REQUESTS {
                return;
            }
            let Some(next) = lock(&self.host.queued).pop_front() else {
                return;
            };
            let flag = Arc::new(AtomicBool::new(false));
            lock(&self.host.active).insert(next.id.clone(), flag.clone());
            let tx = self.completion_tx.clone();
            let deadline = self.host.deadline();
            let id = next.id;
            let request = next.request;
            let spawned = thread::Builder::new()
                .name("music-core-module-fetch".into())
                .spawn(move || {
                    let result = http::execute(request, MAX_BODY_BYTES, Some(deadline), &flag);
                    let _ = tx.send(Completion::Done { id, result });
                });
            if spawned.is_err() {
                return;
            }
        }
    }

    fn settle(&self, id: &str, ok: bool, payload: &str) {
        let _ = self.context.with(|ctx| -> rquickjs::Result<()> {
            let f: Function = ctx.globals().get("__moduleSettle")?;
            f.call::<_, ()>((id, ok, payload))
        });
    }

    fn fire_timer(&self, id: u64) {
        let _ = self.context.with(|ctx| -> rquickjs::Result<()> {
            let f: Function = ctx.globals().get("__moduleTimerFire")?;
            f.call::<_, ()>((id as f64,))
        });
    }

    fn take_result(&self) -> Option<String> {
        self.context.with(|ctx| {
            let g = ctx.globals();
            let value: Option<String> = g.get("__moduleResult").ok().flatten();
            if value.is_some() {
                let _ = g.set("__moduleResult", rquickjs::Null);
            }
            value
        })
    }

    fn call(&mut self, function: &str, args: &Value, deadline: Instant) -> ModuleResult<Value> {
        let host = self.host.clone();
        host.set_deadline(deadline);
        *lock(&host.problem) = None;
        host.reset();
        let timeout = || ModuleError::at(Failure::Timeout, Stage::Provider, "RESOLUTION_TIMEOUT");
        let args_json = serde_json::to_string(&args.to_string()).unwrap_or_default();
        let fix_quality = if function == "getTrackStreamUrl" {
            "if (args[1] === null) args[1] = undefined;"
        } else {
            ""
        };
        let script = format!(
            "globalThis.__moduleResult = null;\n\
             (async () => {{\n\
               try {{\n\
                 const args = JSON.parse({args_json});\n\
                 {fix_quality}\n\
                 const value = await globalThis.__provider[{function}](...args);\n\
                 globalThis.__moduleResult = JSON.stringify({{value: value === undefined ? null : value}});\n\
               }} catch (error) {{\n\
                 const name = ['Error','TypeError','ReferenceError','SyntaxError','RangeError','URIError','EvalError','AbortError','InternalError'].includes(error?.name) ? error.name : 'Error';\n\
                 const frame = String(error?.stack || '').match(/module-(?:host|provider|call)\\.js:\\d+(?::\\d+)?/);\n\
                 const code = ['RESOLUTION_TIMEOUT','VERIFY_REQUIRED'].includes(error?.code) ? error.code : '';\n\
                 globalThis.__moduleResult = JSON.stringify({{failed: true, name, code, frame: frame ? frame[0] : '', message: String(error?.message ?? error).slice(0, 300)}});\n\
               }} finally {{\n\
                 globalThis.__moduleClearTimers();\n\
               }}\n\
             }})();\n\"module-call-started\";",
            function = serde_json::to_string(function).unwrap_or_default(),
        );
        self.eval(&script, "module-call.js", Stage::Provider)
            .map_err(|e| if host.interrupted() { timeout() } else { e })?;
        let result = loop {
            self.drain_jobs();
            if host.cancel.load(Ordering::Relaxed) {
                break Err(ModuleError::at(
                    Failure::Execution,
                    Stage::Engine,
                    "CANCELLED",
                ));
            }
            if let Some(result) = self.take_result() {
                break Ok(result);
            }
            let now = Instant::now();
            if now >= deadline {
                break Err(timeout());
            }
            // Aborted requests reject right away.
            let aborted: Vec<String> = std::mem::take(&mut *lock(&host.aborted));
            if !aborted.is_empty() {
                for id in aborted {
                    self.settle(&id, false, "Aborted");
                }
                continue;
            }
            self.start_requests();
            // Due timers.
            let due: Vec<u64> = {
                let timers = lock(&host.timers);
                timers
                    .iter()
                    .filter(|(_, at)| **at <= now)
                    .map(|(id, _)| *id)
                    .collect()
            };
            if !due.is_empty() {
                for id in due {
                    lock(&host.timers).remove(&id);
                    self.fire_timer(id);
                }
                continue;
            }
            let idle = lock(&host.active).is_empty()
                && lock(&host.queued).is_empty()
                && lock(&host.timers).is_empty()
                && !self.runtime.is_job_pending();
            if idle {
                break Err(ModuleError::at(
                    Failure::Execution,
                    Stage::Provider,
                    "UNSETTLED",
                ));
            }
            let next_timer = lock(&host.timers).values().min().copied();
            let wake = next_timer.map_or(deadline, |t| t.min(deadline));
            match self
                .completions
                .recv_timeout(wake.saturating_duration_since(Instant::now()))
            {
                Ok(Completion::Done { id, result }) => {
                    if lock(&host.active).remove(&id).is_none() {
                        continue; // aborted meanwhile
                    }
                    match result {
                        Ok(response) => {
                            if !response.ok() {
                                host.record_problem(
                                    Failure::Network,
                                    format!("HTTP_{}", response.status),
                                );
                            }
                            self.settle(&id, true, &response.to_json().to_string());
                        }
                        Err(e) => {
                            let code = e.code.clone().unwrap_or_else(|| {
                                format!("REQUEST_{:?}", e.failure).to_ascii_uppercase()
                            });
                            host.record_problem(e.failure, code.clone());
                            self.settle(&id, false, &code);
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {}
            }
        };
        // Clear whatever the call left running.
        let _ = self.context.with(|ctx| -> rquickjs::Result<()> {
            let f: Function = ctx.globals().get("__moduleClearTimers")?;
            f.call::<_, ()>(())
        });
        host.reset();
        let raw = result?;
        if raw.is_empty() || raw.len() > MAX_BODY_BYTES {
            return Err(ModuleError::at(
                Failure::Execution,
                Stage::Provider,
                "INVALID_RESULT",
            ));
        }
        let envelope: Value = serde_json::from_str(&raw)
            .map_err(|_| ModuleError::at(Failure::Execution, Stage::Provider, "INVALID_RESULT"))?;
        let problem = lock(&host.problem).clone();
        if envelope.get("failed").and_then(Value::as_bool) == Some(true) {
            let name = opt_str(&envelope, "name");
            let code = opt_str(&envelope, "code");
            let message = opt_str(&envelope, "message");
            host.log
                .push("error", &format!("{function} failed: {name}: {message}"));
            let failure = match code {
                "RESOLUTION_TIMEOUT" => Failure::Timeout,
                "VERIFY_REQUIRED" => Failure::InvalidConfiguration,
                _ if matches!(name, "Error" | "AbortError") => {
                    problem.as_ref().map_or(Failure::Execution, |p| p.0)
                }
                _ => Failure::Execution,
            };
            let detail = [
                name.to_owned(),
                code.to_owned(),
                opt_str(&envelope, "frame").to_owned(),
                problem.map(|p| p.1).unwrap_or_default(),
            ]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("/");
            return Err(ModuleError::at(failure, Stage::Provider, detail));
        }
        let value = match envelope.get("value") {
            Some(v) if !v.is_null() => v.clone(),
            _ => {
                return Err(ModuleError::at(
                    Failure::Execution,
                    Stage::Provider,
                    "INVALID_RESULT",
                ));
            }
        };
        if function == "searchTracks" {
            let tracks = value
                .get("tracks")
                .or(Some(&value))
                .and_then(Value::as_array);
            if tracks.is_some_and(Vec::is_empty)
                && let Some((failure, code)) = problem
            {
                return Err(ModuleError::at(failure, Stage::Provider, code));
            }
        }
        Ok(value)
    }
}

/// The `context` argument: typed settings, plus the selected track for streams.
pub(crate) fn call_context(
    settings: &BTreeMap<String, Value>,
    track: Option<&ModuleTrack>,
) -> Value {
    let settings: serde_json::Map<String, Value> = settings
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::json!({"value": v})))
        .collect();
    let mut context = serde_json::json!({"settings": settings});
    if let Some(t) = track {
        let metadata = serde_json::json!({
            "id": t.id, "title": t.title, "artist": t.artists.join(", "), "artists": t.artists,
            "album": t.album, "duration": t.duration_secs, "explicit": t.explicit, "isrc": t.isrc,
        });
        context["track"] = metadata.clone();
        context["metadata"] = metadata.clone();
        context["item"] = metadata;
    }
    context
}

/// Normalizes `searchTracks` output (at most 100 results; invalid rows skipped).
pub(crate) fn parse_search(value: &Value) -> ModuleResult<Vec<ModuleTrack>> {
    let tracks = match value {
        Value::Array(items) => items,
        Value::Object(o) => match o.get("tracks") {
            Some(Value::Array(items)) => items,
            _ => {
                return Err(ModuleError::at(
                    Failure::Execution,
                    Stage::Provider,
                    "INVALID_SEARCH_RESULT",
                ));
            }
        },
        _ => {
            return Err(ModuleError::at(
                Failure::Execution,
                Stage::Provider,
                "INVALID_SEARCH_RESULT",
            ));
        }
    };
    Ok(tracks
        .iter()
        .take(100)
        .filter_map(|track| {
            if !track.is_object() {
                return None;
            }
            let id = text(track, "id");
            if id.trim().is_empty() {
                return None;
            }
            let artist = match track.get("artists") {
                Some(v) if !v.is_null() => Some(v),
                _ => track.get("artist"),
            };
            let name_of = |item: &Value| match item {
                Value::Object(_) => Some(text(item, "name")),
                Value::Null => None,
                Value::String(s) => Some(s.clone()),
                other => Some(other.to_string()),
            };
            let artists: Vec<String> = match artist {
                Some(Value::Array(items)) => items.iter().filter_map(name_of).collect(),
                Some(other) => name_of(other).into_iter().collect(),
                None => Vec::new(),
            }
            .into_iter()
            .filter(|a| !a.trim().is_empty())
            .collect();
            let title = match text(track, "title") {
                t if t.trim().is_empty() => text(track, "name"),
                t => t,
            };
            let album = match track.get("album") {
                Some(album @ Value::Object(_)) => match text(album, "title") {
                    t if t.trim().is_empty() => text(album, "name"),
                    t => t,
                },
                _ => text(track, "album"),
            };
            let duration = if track.get("durationMs").is_some() {
                track
                    .get("durationMs")
                    .and_then(Value::as_f64)
                    .map(|ms| (ms / 1000.0) as i64)
            } else {
                track
                    .get("duration")
                    .and_then(Value::as_f64)
                    .map(|s| s as i64)
            }
            .filter(|d| *d > 0)
            .map(|d| d.min(u32::MAX as i64) as u32);
            Some(ModuleTrack {
                id,
                title,
                artists,
                album: (!album.trim().is_empty()).then_some(album),
                duration_secs: duration,
                explicit: track.get("explicit").and_then(Value::as_bool),
                isrc: Some(text(track, "isrc")).filter(|s| !s.trim().is_empty()),
            })
        })
        .collect())
}

/// Normalizes `getTrackStreamUrl` output.
pub(crate) fn parse_stream(value: &Value, track_id: &str) -> ModuleResult<ModuleStream> {
    let invalid = || ModuleError::at(Failure::Execution, Stage::Provider, "INVALID_STREAM_RESULT");
    if !value.is_object() {
        return Err(invalid());
    }
    let raw_url = match text(value, "streamUrl") {
        u if u.trim().is_empty() => text(value, "url"),
        u => u,
    };
    let url = web_url(&raw_url).map_err(|_| invalid())?;
    let empty = Value::Object(Default::default());
    let track = value
        .get("track")
        .filter(|t| t.is_object())
        .unwrap_or(&empty);
    let mut headers: Vec<(String, String)> = Vec::new();
    for declared in [
        track.get("headers"),
        value.get("streamHeaders"),
        value.get("requestHeaders"),
        value.get("headers"),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(map) = declared.as_object() {
            for (k, v) in map {
                let Some(v) = v.as_str() else { continue };
                if k.contains(['\r', '\n', ':']) || v.contains(['\r', '\n']) {
                    return Err(invalid());
                }
                headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(k));
                headers.push((k.clone(), v.to_owned()));
            }
        }
    }
    let ua = text(value, "userAgent");
    if !ua.trim().is_empty()
        && !headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("user-agent"))
    {
        headers.push(("User-Agent".into(), ua));
    }
    let first = |items: &[String]| {
        items
            .iter()
            .find(|s| !s.trim().is_empty())
            .cloned()
            .unwrap_or_default()
    };
    let mime = first(&[text(value, "mimeType"), text(track, "mimeType")]);
    let mime = if !mime.is_empty() {
        mime
    } else {
        match text(value, "streamType").to_ascii_lowercase().as_str() {
            "hls" => "application/x-mpegURL".into(),
            "dash" => "application/dash+xml".into(),
            _ => {
                let path = url::Url::parse(&url)
                    .map(|u| u.path().to_owned())
                    .unwrap_or_default();
                let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
                match ext.as_str() {
                    "m3u8" => "application/x-mpegURL",
                    "mpd" => "application/dash+xml",
                    "flac" => "audio/flac",
                    "m4a" | "mp4" => "audio/mp4",
                    "opus" | "ogg" => "audio/ogg",
                    "wav" => "audio/wav",
                    "aac" => "audio/aac",
                    "mp3" => "audio/mpeg",
                    _ => "application/octet-stream",
                }
                .into()
            }
        }
    };
    let now = now_ms();
    let int = |v: &Value, k: &str| v.get(k).and_then(Value::as_f64).filter(|f| f.is_finite());
    let lifetime = int(value, "expiresInSeconds")
        .or_else(|| int(value, "expiresIn"))
        .unwrap_or(300.0)
        .clamp(1.0, 86_400.0) as u64;
    let expiry = int(value, "expiresAtMs")
        .or_else(|| int(value, "expiresAt"))
        .unwrap_or(0.0);
    let expiry = if expiry <= 0.0 {
        now + lifetime * 1000
    } else if expiry < 10_000_000_000.0 {
        (expiry * 1000.0) as u64
    } else {
        expiry as u64
    };
    if expiry <= now {
        return Err(ModuleError::at(
            Failure::Execution,
            Stage::Provider,
            "EXPIRED_STREAM",
        ));
    }
    let sample_rate = int(track, "sampleRate")
        .or_else(|| int(value, "sampleRate"))
        .filter(|r| *r > 0.0)
        .map(|r| {
            if r < 1000.0 {
                (r * 1000.0) as u32
            } else {
                r as u32
            }
        });
    let bitrate = int(track, "bitrate")
        .or_else(|| int(value, "bitrate"))
        .unwrap_or(0.0)
        .max(0.0) as u32;
    let content_length = int(value, "contentLength")
        .or_else(|| int(track, "contentLength"))
        .unwrap_or(0.0)
        .max(0.0) as u64;
    let variant = first(&[
        text(value, "variant"),
        text(value, "audioQuality"),
        text(track, "audioQuality"),
        text(track, "formatId"),
        text(track, "format"),
        mime.clone(),
    ]);
    Ok(ModuleStream {
        url,
        headers,
        codecs: first(&[
            text(value, "codecs"),
            text(value, "codec"),
            text(track, "codecs"),
            text(track, "codec"),
        ]),
        mime_type: mime,
        bitrate,
        sample_rate,
        content_length,
        expires_at_ms: expiry,
        variant,
        upstream_provider: first(&[text(value, "provider"), text(track, "provider")]),
        upstream_id: first(&[
            text(track, "sourceId"),
            text(track, "id"),
            track_id.to_owned(),
        ]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn search_results_are_normalized() {
        let value = json!({"tracks": [
            {"id": 7, "name": "T", "artist": {"name": "A"}, "album": {"name": "Al"}, "durationMs": 61500},
            {"id": "", "title": "skipped"},
            {"id": "x", "title": "U", "artists": ["B", {"name": "C"}, null], "duration": 30, "explicit": true, "isrc": "US1"},
            "junk"
        ]});
        let tracks = parse_search(&value).unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].id, "7");
        assert_eq!(tracks[0].artists, ["A"]);
        assert_eq!(tracks[0].album.as_deref(), Some("Al"));
        assert_eq!(tracks[0].duration_secs, Some(61));
        assert_eq!(tracks[1].artists, ["B", "C"]);
        assert_eq!(tracks[1].explicit, Some(true));
        assert!(parse_search(&json!("nope")).is_err());
    }

    #[test]
    fn stream_results_are_normalized() {
        let v = json!({"streamUrl": "https://cdn.example.org/a.flac?sig=1",
            "headers": {"X-Token": "t"}, "requestHeaders": {"x-token": "old", "Referer": "r"},
            "sampleRate": 44.1, "bitrate": 900000, "track": {"codec": "flac"}});
        let s = parse_stream(&v, "id1").unwrap();
        assert_eq!(s.mime_type, "audio/flac");
        assert_eq!(s.sample_rate, Some(44_100));
        assert_eq!(s.codecs, "flac");
        assert_eq!(s.upstream_id, "id1");
        assert!(s.headers.contains(&("X-Token".into(), "t".into())));
        assert!(s.headers.iter().any(|(k, _)| k == "Referer"));
        assert_eq!(s.headers.len(), 2);
        assert!(s.expires_at_ms > now_ms());
        assert!(parse_stream(&json!({"url": "file:///etc/passwd"}), "x").is_err());
        assert!(
            parse_stream(
                &json!({"url": "https://a.example/x", "expiresAt": 1000}),
                "x"
            )
            .is_err()
        );
        let hls = parse_stream(&json!({"url": "https://a.example/x.m3u8"}), "x").unwrap();
        assert!(hls.is_manifest());
    }
}
