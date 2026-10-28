//! YouTube's JavaScript challenges (`n` throttling parameter and `s` signature),
//! solved with yt-dlp's challenge solver ([yt-dlp/ejs], public domain, bundling
//! meriyah (ISC) and astring (MIT)) running in an embedded QuickJS.
//!
//! The solver parses the player script once ("preprocessing", a few seconds in
//! QuickJS) and returns a script exposing just the two functions; that is cached
//! per player id in memory and on disk. Loading it into a fresh engine still takes
//! a few hundred ms (it is ~4 MB), so [`ChallengeSolver::prepare`] does that ahead
//! of time — while the player request is in flight — and the solve itself then
//! only calls the functions.
//!
//! [yt-dlp/ejs]: https://github.com/yt-dlp/ejs

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use rquickjs::context::EvalOptions;
use rquickjs::{CatchResultExt, Context, Runtime};
use serde_json::{Value, json};

use crate::{Error, Result};

const LIB_JS: &str = include_str!("../../js/ejs/lib.min.js");
const CORE_JS: &str = include_str!("../../js/ejs/core.min.js");
/// Version of the vendored solver; part of the on-disk cache key.
pub(crate) const SOLVER_VERSION: &str = "ejs-0.8.0";

/// Parsing a ~3 MB player script with meriyah needs room.
const MEMORY_LIMIT: usize = 768 * 1024 * 1024;
const JS_STACK: usize = 16 * 1024 * 1024;
const THREAD_STACK: usize = 64 * 1024 * 1024;
/// A prepared engine nobody used within this time is dropped (memory).
const PREPARED_IDLE: std::time::Duration = std::time::Duration::from_secs(30);

/// Solutions for one player, keyed by challenge.
#[derive(Debug, Default, Clone)]
pub(crate) struct Solutions {
    pub n: HashMap<String, String>,
    pub sig: HashMap<String, String>,
}

struct Job {
    player_id: String,
    /// Full player script; only needed when no preprocessed copy is cached.
    player: Option<Arc<str>>,
    n: Vec<String>,
    sig: Vec<String>,
    reply: Sender<Result<Solutions>>,
}

enum Message {
    Solve(Job),
    /// Load this (preprocessed) player into an engine for the next solve.
    Prepare(String),
}

/// An engine with a preprocessed player loaded (`globalThis.__ejs.n` / `.sig`).
struct Prepared {
    player_id: String,
    engine: Engine,
    at: std::time::Instant,
}

/// Runs the solver on one dedicated thread (QuickJS runtimes aren't `Send`).
pub(crate) struct ChallengeSolver {
    jobs: Mutex<Option<Sender<Message>>>,
    cache_dir: Option<PathBuf>,
    /// Player ids preprocessed in this process.
    known: Arc<Mutex<HashSet<String>>>,
}

impl ChallengeSolver {
    /// `cache_dir`: where preprocessed players are kept between runs (optional).
    pub(crate) fn new(cache_dir: Option<PathBuf>) -> Self {
        Self {
            jobs: Mutex::new(None),
            cache_dir,
            known: Arc::default(),
        }
    }

    /// Whether `player_id` can be solved without its full script.
    pub(crate) fn has_preprocessed(&self, player_id: &str) -> bool {
        self.known
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(player_id)
            || self
                .cache_dir
                .as_deref()
                .is_some_and(|dir| cache_path(dir, player_id).is_file())
    }

    pub(crate) fn solve(
        &self,
        player_id: &str,
        player: Option<Arc<str>>,
        n: Vec<String>,
        sig: Vec<String>,
    ) -> Result<Solutions> {
        if n.is_empty() && sig.is_empty() {
            return Ok(Solutions::default());
        }
        let (reply, rx) = mpsc::channel();
        let job = Job {
            player_id: player_id.to_owned(),
            player,
            n,
            sig,
            reply,
        };
        self.sender()?
            .send(Message::Solve(job))
            .map_err(|_| Error::Audio("challenge solver stopped".into()))?;
        rx.recv()
            .map_err(|_| Error::Audio("challenge solver crashed".into()))?
    }

    /// Starts loading `player_id`'s preprocessed player into an engine on the
    /// solver thread, so that a [`Self::solve`] soon after only runs the
    /// challenge functions. Returns at once; a no-op if it isn't preprocessed.
    pub(crate) fn prepare(&self, player_id: &str) {
        if !self.has_preprocessed(player_id) {
            return;
        }
        if let Ok(tx) = self.sender() {
            let _ = tx.send(Message::Prepare(player_id.to_owned()));
        }
    }

    fn sender(&self) -> Result<Sender<Message>> {
        let mut slot = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(tx) = slot.as_ref() {
            return Ok(tx.clone());
        }
        let (tx, rx) = mpsc::channel::<Message>();
        let cache_dir = self.cache_dir.clone();
        let known = self.known.clone();
        thread::Builder::new()
            .name("music-core-jsc".into())
            .stack_size(THREAD_STACK)
            .spawn(move || {
                // Preprocessed players outlive the engine, which is dropped after every
                // job: parsing a full player script leaves ~300 MB in QuickJS's heap,
                // and recreating the engine for the next song takes only milliseconds.
                let mut preprocessed = HashMap::new();
                let mut prepared: Option<Prepared> = None;
                loop {
                    let message = match rx.recv_timeout(PREPARED_IDLE) {
                        Ok(message) => message,
                        Err(RecvTimeoutError::Timeout) => {
                            prepared = prepared.filter(|p| p.at.elapsed() < PREPARED_IDLE);
                            continue;
                        }
                        Err(RecvTimeoutError::Disconnected) => return,
                    };
                    let job = match message {
                        Message::Prepare(player_id) => {
                            if prepared.as_ref().is_none_or(|p| p.player_id != player_id) {
                                prepared = None;
                                let started = std::time::Instant::now();
                                match prepare_engine(
                                    &preprocessed,
                                    cache_dir.as_deref(),
                                    &player_id,
                                ) {
                                    Ok(engine) => {
                                        log::debug!(
                                            "jsc: player {player_id} loaded in {:?}",
                                            started.elapsed()
                                        );
                                        prepared = Some(Prepared {
                                            player_id,
                                            engine,
                                            at: std::time::Instant::now(),
                                        });
                                    }
                                    Err(e) => log::debug!("jsc: preparing {player_id} failed: {e}"),
                                }
                            }
                            continue;
                        }
                        Message::Solve(job) => job,
                    };
                    // A prepared engine serves one solve, then goes (memory).
                    let fast = prepared
                        .take()
                        .filter(|p| p.player_id == job.player_id)
                        .and_then(|p| match solve_prepared(&p.engine, &job) {
                            Ok(solutions) => Some(solutions),
                            Err(e) => {
                                log::debug!("jsc: prepared solve failed, solving afresh: {e}");
                                None
                            }
                        });
                    let result = match fast {
                        Some(solutions) => Ok(solutions),
                        None => {
                            let mut engine = None;
                            let result = solve_job(
                                &mut engine,
                                &mut preprocessed,
                                cache_dir.as_deref(),
                                &job,
                            );
                            if let Some(engine) = &engine {
                                engine._runtime.run_gc();
                            }
                            result
                        }
                    };
                    if result.is_ok() {
                        known
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .insert(job.player_id.clone());
                    }
                    let _ = job.reply.send(result);
                }
            })
            .map_err(|e| Error::Audio(format!("challenge solver thread: {e}")))?;
        *slot = Some(tx.clone());
        Ok(tx)
    }
}

struct Engine {
    // Field order matters: the context drops before its runtime, and the runtime
    // before the memory zone it allocates from.
    context: Context,
    _runtime: Runtime,
    _zone: zone::Zone,
}

/// An engine with `player_id`'s preprocessed player evaluated, the way yt-dlp's
/// solver runs one (`Function("_result", player)(result)` → `result.n` / `.sig`),
/// without loading the solver itself.
fn prepare_engine(
    preprocessed: &HashMap<String, String>,
    cache_dir: Option<&std::path::Path>,
    player_id: &str,
) -> Result<Engine> {
    // Read from the cache file each time (a few ms) rather than keeping ~4 MB
    // of script text in memory; only players that couldn't be cached stay in
    // `preprocessed`.
    let source = match preprocessed.get(player_id) {
        Some(source) => source.clone(),
        None => {
            let path = cache_path(
                cache_dir.ok_or_else(|| Error::Parse("no solver cache".into()))?,
                player_id,
            );
            std::fs::read_to_string(path)
                .map_err(|e| Error::Parse(format!("preprocessed player: {e}")))?
        }
    };
    let engine = bare_engine()?;
    engine.context.with(|ctx| -> Result<()> {
        ctx.globals()
            .set("__ejsPlayer", source)
            .map_err(|e| Error::Audio(format!("challenge solver: {e}")))?;
        ctx.eval_with_options::<(), _>(
            "globalThis.__ejs = { n: null, sig: null };\n\
             Function('_result', globalThis.__ejsPlayer)(globalThis.__ejs);\n\
             delete globalThis.__ejsPlayer;",
            eval_options("prepare.js"),
        )
        .catch(&ctx)
        .map_err(|e| Error::Audio(format!("challenge solver: loading the player: {e}")))
    })?;
    Ok(engine)
}

/// Runs a job's challenges through a [`prepare_engine`] engine.
fn solve_prepared(engine: &Engine, job: &Job) -> Result<Solutions> {
    let input = json!({ "n": job.n, "sig": job.sig }).to_string();
    let output: String = engine.context.with(|ctx| {
        ctx.globals()
            .set("__jscInput", input)
            .map_err(|e| Error::Audio(format!("challenge solver: {e}")))?;
        ctx.eval_with_options::<String, _>(
            "(() => {\n\
               const input = JSON.parse(globalThis.__jscInput);\n\
               const out = { n: {}, sig: {} };\n\
               for (const kind of ['n', 'sig']) {\n\
                 if (!input[kind].length) continue;\n\
                 const f = globalThis.__ejs[kind];\n\
                 if (!f) throw new Error(`no ${kind} function`);\n\
                 for (const c of input[kind]) out[kind][c] = f(c);\n\
               }\n\
               return JSON.stringify(out);\n\
             })()",
            eval_options("solve-prepared.js"),
        )
        .catch(&ctx)
        .map_err(|e| Error::Audio(format!("challenge solver: {e}")))
    })?;
    let output: Value = serde_json::from_str(&output)
        .map_err(|e| Error::Parse(format!("challenge solver output: {e}")))?;
    let mut solutions = Solutions::default();
    for (kind, target) in [("n", &mut solutions.n), ("sig", &mut solutions.sig)] {
        for (challenge, solution) in output[kind].as_object().into_iter().flatten() {
            let solution = solution
                .as_str()
                .ok_or_else(|| Error::Parse(format!("challenge solver: no {kind} solution")))?;
            target.insert(challenge.clone(), solution.to_owned());
        }
    }
    Ok(solutions)
}

/// A QuickJS engine without the solver loaded.
fn bare_engine() -> Result<Engine> {
    let fail = |what: &str| Error::Audio(format!("challenge solver: {what}"));
    let zone = zone::Zone::new();
    let runtime = match zone.allocator() {
        Some(allocator) => Runtime::new_with_alloc(allocator),
        None => Runtime::new(),
    }
    .map_err(|_| fail("runtime"))?;
    runtime.set_memory_limit(MEMORY_LIMIT);
    runtime.set_max_stack_size(JS_STACK);
    let context = Context::full(&runtime).map_err(|_| fail("context"))?;
    Ok(Engine {
        context,
        _runtime: runtime,
        _zone: zone,
    })
}

fn new_engine() -> Result<Engine> {
    let fail = |what: &str| Error::Audio(format!("challenge solver: {what}"));
    let zone = zone::Zone::new();
    let runtime = match zone.allocator() {
        Some(allocator) => Runtime::new_with_alloc(allocator),
        None => Runtime::new(),
    }
    .map_err(|_| fail("runtime"))?;
    runtime.set_memory_limit(MEMORY_LIMIT);
    runtime.set_max_stack_size(JS_STACK);
    let context = Context::full(&runtime).map_err(|_| fail("context"))?;
    context.with(|ctx| -> Result<()> {
        let source = format!("{LIB_JS}\nObject.assign(globalThis, lib);\n{CORE_JS}\n;void 0;");
        ctx.eval_with_options::<(), _>(source, eval_options("ejs.js"))
            .catch(&ctx)
            .map_err(|e| fail(&format!("loading the solver: {e}")))
    })?;
    Ok(Engine {
        context,
        _runtime: runtime,
        _zone: zone,
    })
}

fn eval_options(filename: &str) -> EvalOptions {
    let mut options = EvalOptions::default();
    options.global = true;
    options.strict = false;
    options.filename = Some(filename.to_owned());
    options
}

fn cache_path(dir: &std::path::Path, player_id: &str) -> PathBuf {
    let safe: String = player_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    dir.join(format!("player-{safe}-{SOLVER_VERSION}.js"))
}

fn solve_job(
    engine: &mut Option<Engine>,
    preprocessed: &mut HashMap<String, String>,
    cache_dir: Option<&std::path::Path>,
    job: &Job,
) -> Result<Solutions> {
    if engine.is_none() {
        *engine = Some(new_engine()?);
    }
    let engine = engine.as_mut().expect("engine was just created");

    let cached = preprocessed.get(&job.player_id).cloned().or_else(|| {
        let path = cache_path(cache_dir?, &job.player_id);
        std::fs::read_to_string(path).ok()
    });
    let mut requests = Vec::new();
    if !job.n.is_empty() {
        requests.push(json!({ "type": "n", "challenges": job.n }));
    }
    if !job.sig.is_empty() {
        requests.push(json!({ "type": "sig", "challenges": job.sig }));
    }
    let input = match (&cached, &job.player) {
        (Some(pre), _) => {
            json!({ "type": "preprocessed", "preprocessed_player": pre, "requests": requests })
        }
        (None, Some(player)) => json!({
            "type": "player",
            "player": &**player,
            "requests": requests,
            "output_preprocessed": true,
        }),
        (None, None) => {
            return Err(Error::Parse(format!(
                "challenge solver: player {} is not preprocessed and no script was given",
                job.player_id
            )));
        }
    };
    let input = input.to_string();
    let output: String = engine.context.with(|ctx| {
        ctx.globals()
            .set("__jscInput", input)
            .map_err(|e| Error::Audio(format!("challenge solver: {e}")))?;
        ctx.eval_with_options::<String, _>(
            "JSON.stringify(jsc(JSON.parse(globalThis.__jscInput)))",
            eval_options("solve.js"),
        )
        .catch(&ctx)
        .map_err(|e| Error::Audio(format!("challenge solver: {e}")))
    })?;
    let output: Value = serde_json::from_str(&output)
        .map_err(|e| Error::Parse(format!("challenge solver output: {e}")))?;
    if output["type"] != "result" {
        return Err(Error::Parse(format!(
            "challenge solver: {}",
            output["error"].as_str().unwrap_or("unknown error")
        )));
    }
    if cached.is_none()
        && let Some(pre) = output["preprocessed_player"].as_str()
    {
        // Kept in memory only if it can't be cached on disk (see `prepare_engine`).
        let stored = cache_dir.is_some_and(|dir| {
            let _ = std::fs::create_dir_all(dir);
            std::fs::write(cache_path(dir, &job.player_id), pre).is_ok()
        });
        if !stored {
            preprocessed.insert(job.player_id.clone(), pre.to_owned());
        }
    }

    let mut solutions = Solutions::default();
    let responses = output["responses"].as_array().cloned().unwrap_or_default();
    for (request, response) in requests.iter().zip(responses) {
        if response["type"] != "result" {
            return Err(Error::Parse(format!(
                "challenge solver ({}): {}",
                request["type"].as_str().unwrap_or("?"),
                response["error"].as_str().unwrap_or("unknown error")
            )));
        }
        let target = match request["type"].as_str() {
            Some("n") => &mut solutions.n,
            _ => &mut solutions.sig,
        };
        for (challenge, solution) in response["data"].as_object().into_iter().flatten() {
            if let Some(solution) = solution.as_str() {
                target.insert(challenge.clone(), solution.to_owned());
            }
        }
    }
    Ok(solutions)
}

/// A private malloc zone for one QuickJS engine (macOS), destroyed with it.
///
/// Parsing a ~3 MB player script churns through hundreds of MB; in the default
/// zone that leaves ~100 MB of fragmented pages behind that macOS never reclaims.
/// Destroying a dedicated zone returns all of it at once. Elsewhere the engine
/// uses the system allocator.
mod zone {
    #[cfg(target_os = "macos")]
    mod imp {
        use std::ffi::c_void;

        use rquickjs::allocator::Allocator;

        unsafe extern "C" {
            fn malloc_create_zone(start_size: usize, flags: u32) -> *mut c_void;
            fn malloc_destroy_zone(zone: *mut c_void);
            fn malloc_zone_malloc(zone: *mut c_void, size: usize) -> *mut c_void;
            fn malloc_zone_calloc(zone: *mut c_void, count: usize, size: usize) -> *mut c_void;
            fn malloc_zone_realloc(zone: *mut c_void, ptr: *mut c_void, size: usize)
            -> *mut c_void;
            fn malloc_zone_free(zone: *mut c_void, ptr: *mut c_void);
            fn malloc_size(ptr: *const c_void) -> usize;
        }

        /// Owns the zone; must outlive every runtime using its [`ZoneAllocator`].
        pub struct Zone(*mut c_void);

        impl Zone {
            pub fn new() -> Self {
                // SAFETY: plain zone creation; null on failure (then the system allocator is used).
                Self(unsafe { malloc_create_zone(0, 0) })
            }

            pub fn allocator(&self) -> Option<ZoneAllocator> {
                (!self.0.is_null()).then_some(ZoneAllocator(self.0))
            }
        }

        impl Drop for Zone {
            fn drop(&mut self) {
                if !self.0.is_null() {
                    // SAFETY: the runtime that used this zone has been dropped
                    // (field order in `Engine`), so nothing points into it.
                    unsafe { malloc_destroy_zone(self.0) };
                }
            }
        }

        pub struct ZoneAllocator(*mut c_void);

        // SAFETY: plain malloc-zone calls; QuickJS frees only what it allocated here.
        unsafe impl Allocator for ZoneAllocator {
            fn alloc(&mut self, size: usize) -> *mut u8 {
                unsafe { malloc_zone_malloc(self.0, size.max(1)).cast() }
            }

            fn calloc(&mut self, count: usize, size: usize) -> *mut u8 {
                unsafe { malloc_zone_calloc(self.0, count.max(1), size.max(1)).cast() }
            }

            unsafe fn dealloc(&mut self, ptr: *mut u8) {
                if !ptr.is_null() {
                    unsafe { malloc_zone_free(self.0, ptr.cast()) }
                }
            }

            unsafe fn realloc(&mut self, ptr: *mut u8, new_size: usize) -> *mut u8 {
                unsafe { malloc_zone_realloc(self.0, ptr.cast(), new_size.max(1)).cast() }
            }

            unsafe fn usable_size(ptr: *mut u8) -> usize {
                if ptr.is_null() {
                    0
                } else {
                    unsafe { malloc_size(ptr.cast()) }
                }
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    mod imp {
        pub struct Zone;

        impl Zone {
            pub fn new() -> Self {
                Self
            }

            pub fn allocator(&self) -> Option<rquickjs::allocator::RustAllocator> {
                None
            }
        }
    }

    pub use imp::Zone;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for a preprocessed player: what yt-dlp's solver outputs
    /// defines `_result.n` and `_result.sig`.
    const PLAYER: &str = "_result.n = c => c.split('').reverse().join('');\n\
                          _result.sig = c => c + '!';";

    #[test]
    fn prepared_and_fresh_solves_agree() {
        let dir = std::env::temp_dir().join(format!("music-core-jsc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(cache_path(&dir, "test"), PLAYER).unwrap();
        let solver = ChallengeSolver::new(Some(dir.clone()));
        assert!(solver.has_preprocessed("test"));
        let solve = || {
            solver
                .solve("test", None, vec!["abc".into()], vec!["x".into()])
                .unwrap()
        };
        // Without preparing: the full solver.
        let fresh = solve();
        assert_eq!(fresh.n["abc"], "cba");
        assert_eq!(fresh.sig["x"], "x!");
        // Prepared ahead (as during the player request): the same answers.
        solver.prepare("test");
        let prepared = solve();
        assert_eq!((prepared.n, prepared.sig), (fresh.n, fresh.sig));
        // The prepared path on its own.
        let engine = prepare_engine(&HashMap::new(), Some(&dir), "test").unwrap();
        let (reply, _rx) = mpsc::channel();
        let job = Job {
            player_id: "test".into(),
            player: None,
            n: vec!["xyz".into()],
            sig: vec![],
            reply,
        };
        let direct = solve_prepared(&engine, &job).unwrap();
        assert_eq!(direct.n["xyz"], "zyx");
        assert!(direct.sig.is_empty());
        // A player that isn't preprocessed is not prepared (and can't be solved).
        solver.prepare("unknown");
        assert!(
            solver
                .solve("unknown", None, vec!["a".into()], vec![])
                .is_err()
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
