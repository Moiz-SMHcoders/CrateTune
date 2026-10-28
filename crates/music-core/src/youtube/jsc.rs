//! YouTube's JavaScript challenges (`n` throttling parameter and `s` signature),
//! solved with yt-dlp's challenge solver ([yt-dlp/ejs], public domain, bundling
//! meriyah (ISC) and astring (MIT)) running in an embedded QuickJS.
//!
//! The solver parses the player script once ("preprocessing", a few seconds in
//! QuickJS) and returns a much smaller script holding just the two functions; that
//! is cached per player id in memory and on disk, so later solves take milliseconds.
//!
//! [yt-dlp/ejs]: https://github.com/yt-dlp/ejs

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
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

/// Runs the solver on one dedicated thread (QuickJS runtimes aren't `Send`).
pub(crate) struct ChallengeSolver {
    jobs: Mutex<Option<Sender<Job>>>,
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
            .send(job)
            .map_err(|_| Error::Audio("challenge solver stopped".into()))?;
        rx.recv()
            .map_err(|_| Error::Audio("challenge solver crashed".into()))?
    }

    fn sender(&self) -> Result<Sender<Job>> {
        let mut slot = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(tx) = slot.as_ref() {
            return Ok(tx.clone());
        }
        let (tx, rx) = mpsc::channel::<Job>();
        let cache_dir = self.cache_dir.clone();
        let known = self.known.clone();
        thread::Builder::new()
            .name("music-core-jsc".into())
            .stack_size(THREAD_STACK)
            .spawn(move || {
                let mut engine = None;
                for job in rx {
                    let result = solve_job(&mut engine, cache_dir.as_deref(), &job);
                    if result.is_ok() {
                        known
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .insert(job.player_id.clone());
                    }
                    if result.is_err() {
                        // A failed run may leave the runtime half-initialized.
                        engine = None;
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
    // Field order matters: the context must drop before its runtime.
    context: Context,
    _runtime: Runtime,
    /// Preprocessed players already in memory, by player id.
    preprocessed: HashMap<String, String>,
}

fn new_engine() -> Result<Engine> {
    let fail = |what: &str| Error::Audio(format!("challenge solver: {what}"));
    let runtime = Runtime::new().map_err(|_| fail("runtime"))?;
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
        preprocessed: HashMap::new(),
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
    cache_dir: Option<&std::path::Path>,
    job: &Job,
) -> Result<Solutions> {
    if engine.is_none() {
        *engine = Some(new_engine()?);
    }
    let engine = engine.as_mut().expect("engine was just created");

    let cached = engine
        .preprocessed
        .get(&job.player_id)
        .cloned()
        .or_else(|| {
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
        if let Some(dir) = cache_dir {
            let _ = std::fs::create_dir_all(dir);
            let _ = std::fs::write(cache_path(dir, &job.player_id), pre);
        }
        engine
            .preprocessed
            .insert(job.player_id.clone(), pre.to_owned());
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
