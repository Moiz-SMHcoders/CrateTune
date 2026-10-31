//! The in-app log (Settings → About → Logs; idea from Zuno, Apache-2.0).
//!
//! The app logs with `eprintln!("music-gpui: …")` (and `music-core` through the `log`
//! crate). [`install`] puts a pipe behind the process's stderr: a thread reads it line
//! by line, [`redact`]s each line, writes it on to the real stderr (terminal, console),
//! keeps the last [`CAPACITY`] lines in memory ([`Ring`]) and, if enabled, appends them
//! to a log file in the logs folder ([`folder`]: `logs/` in `MUSIC_CONFIG_DIR` when
//! that is set, else in the platform cache folder), rotated at [`FILE_LIMIT`] with one
//! older file kept. The `log` crate's records go to the same place with their level.
//!
//! Lines have no level of their own: [`classify`] guesses one (errors, warnings,
//! `MUSIC_TRACE` lines, the rest is info). Nothing that looks like a cookie, token,
//! key, signature or password reaches the ring, the file or the terminal ([`redact`]).

use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

/// Lines kept in memory.
pub const CAPACITY: usize = 2000;
/// The log file is rotated past this size (one older file kept).
pub const FILE_LIMIT: u64 = 1024 * 1024;
const FILE_NAME: &str = "app.log";
const OLD_FILE_NAME: &str = "app.log.1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Error,
    Warn,
    Info,
    Trace,
}

impl Level {
    /// Fixed-width tag for the file and copies (not translated: it is a log).
    pub fn tag(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN ",
            Level::Info => "INFO ",
            Level::Trace => "TRACE",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// Unix time, ms.
    pub at_ms: u64,
    pub level: Level,
    /// Already redacted.
    pub text: String,
}

impl Line {
    /// `2026-10-31 12:34:56 WARN  text` (UTC).
    pub fn format(&self) -> String {
        format!("{} {} {}", stamp(self.at_ms), self.level.tag(), self.text)
    }
}

/// `HH:MM:SS` of a unix time in ms, UTC (the viewer's column).
pub fn clock(at_ms: u64) -> String {
    let secs = at_ms / 1000;
    format!(
        "{:02}:{:02}:{:02}",
        (secs / 3600) % 24,
        (secs / 60) % 60,
        secs % 60
    )
}

/// `YYYY-MM-DD HH:MM:SS` UTC.
fn stamp(at_ms: u64) -> String {
    let days = (at_ms / 1000 / 86_400) as i64;
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {}", clock(at_ms))
}

/// The last lines, oldest first.
pub struct Ring {
    lines: VecDeque<Line>,
    cap: usize,
}

impl Ring {
    pub fn new(cap: usize) -> Self {
        Self {
            lines: VecDeque::with_capacity(cap.min(256)),
            cap: cap.max(1),
        }
    }

    pub fn push(&mut self, line: Line) {
        if self.lines.len() == self.cap {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Lines at `max` or more severe, the newest `limit` of them, oldest first.
    pub fn filtered(&self, max: Level, limit: usize) -> Vec<Line> {
        let mut out: Vec<Line> = self
            .lines
            .iter()
            .rev()
            .filter(|l| l.level <= max)
            .take(limit)
            .cloned()
            .collect();
        out.reverse();
        out
    }
}

/// A line's level, guessed from its text (the app's lines carry none).
pub fn classify(text: &str) -> Level {
    let lower = text.to_ascii_lowercase();
    // `MUSIC_TRACE` lines: "music-gpui: [  1.23s] …", "… autoplay: …", "radio: +N".
    let body = lower.split_once(": ").map_or(lower.as_str(), |(_, b)| b);
    if body.starts_with('[') && body.contains("s]") {
        return Level::Trace;
    }
    const ERRORS: &[&str] = &[
        "error",
        "failed",
        "panicked",
        "can't",
        "cannot",
        "could not", // i18n-ignore
        "couldn't",
        "unable",
    ];
    const WARNINGS: &[&str] = &["warn", "retry", "retrying", "falling back", "fallback"]; // i18n-ignore
    if ERRORS.iter().any(|w| lower.contains(w)) {
        Level::Error
    } else if WARNINGS.iter().any(|w| lower.contains(w)) {
        Level::Warn
    } else {
        Level::Info
    }
}

// ---- redaction --------------------------------------------------------------------------

/// Keys whose value is a secret (lower case, `-` as `_`).
const SECRET_KEYS: &[&str] = &[
    "token",
    "access_token",
    "refresh_token",
    "id_token",
    "auth",
    "auth_token",
    "authuser_token",
    "api_key",
    "apikey",
    "key",
    "secret",
    "client_secret",
    "password",
    "passwd",
    "pass",
    "session_key",
    "sessionkey",
    "session_token",
    "sessiontoken",
    "sk",
    "potoken",
    "po_token",
    "pot",
    "visitor_data",
    "visitordata",
    "x_goog_visitor_id",
    "x_youtube_identity_token",
    "sig",
    "lsig",
    "signature",
    "login_info",
    "relay_secret",
    "relaysecret",
    "secure_3papisid",
];
/// Endings that make a key secret: cookie names (`SAPISID`, `__Secure-1PSID`, `HSID`,
/// `__Secure-3PSIDTS`, …), `…_token`, `…_key`, `…_secret`, `…password`.
const SECRET_SUFFIXES: &[&str] = &[
    "apisid", "psid", "sid", "psidts", "psidcc", "_token", "token", "_key", "_secret", "password",
];
/// Header-like keys whose value runs to the end of the line (it has spaces).
const LINE_KEYS: &[&str] = &[
    "cookie",
    "set_cookie",
    "cookies",
    "authorization",
    "proxy_authorization",
];
const REDACTED: &str = "<redacted>";

fn normalize_key(key: &str) -> String {
    key.trim_start_matches('_')
        .to_ascii_lowercase()
        .replace(['-', '.'], "_")
}

fn is_secret_key(key: &str) -> bool {
    let key = normalize_key(key);
    !key.is_empty()
        && (SECRET_KEYS.contains(&key.as_str())
            || SECRET_SUFFIXES.iter().any(|s| key.ends_with(s))
            || LINE_KEYS.contains(&key.as_str()))
}

fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')
}

fn ends_value(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '"' | '\'' | '&' | ';' | ',' | ')' | ']' | '}' | '<' | '>'
        )
}

/// The line with secrets replaced by `<redacted>`: values of secret keys (`key=…`,
/// `"sessionKey":"…"`, `SAPISID=…; HSID=…`, URL parameters such as `key`, `pot`,
/// `sig`), whole `Cookie:` / `Authorization:` values, `Bearer …` / `SAPISIDHASH …`
/// credentials, Google API keys (`AIza…`) and `sk-…` API keys. Borrowed when there is
/// nothing to hide.
pub fn redact(line: &str) -> Cow<'_, str> {
    let mut out = String::new();
    let mut last = 0; // copied up to here
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < line.len() {
        let c = bytes[i];
        // `key=value`, `key: value`, `"key":"value"`.
        if c == b'=' || c == b':' {
            // The key: back over a closing quote and spaces, then key characters.
            let before = line[..i].trim_end();
            let before = before.strip_suffix(['"', '\'']).unwrap_or(before);
            let start = before
                .char_indices()
                .rev()
                .take_while(|(_, ch)| is_key_char(*ch))
                .last()
                .map_or(before.len(), |(at, _)| at);
            let key = &before[start..];
            // `https://…` and the like are not keys.
            let url = c == b':' && line[i + 1..].starts_with("//");
            // A stream URL's `ip=` is the listener's address (a plain `ip:` in a
            // Together line is the LAN host, which helps debugging).
            let ip_param = c == b'=' && normalize_key(key) == "ip";
            if !url && (is_secret_key(key) || ip_param) {
                let whole_line = LINE_KEYS.contains(&normalize_key(key).as_str());
                let rest = &line[i + 1..];
                let skip = rest.len() - rest.trim_start().len();
                let mut v = i + 1 + skip;
                let quote = line[v..].chars().next().filter(|q| *q == '"' || *q == '\'');
                if let Some(q) = quote {
                    v += q.len_utf8();
                }
                let end = match (whole_line, quote) {
                    (_, Some(q)) => line[v..].find(q).map_or(line.len(), |e| v + e),
                    (true, None) => line.len(),
                    (false, None) => line[v..]
                        .char_indices()
                        .find(|(_, ch)| ends_value(*ch))
                        .map_or(line.len(), |(e, _)| v + e),
                };
                if end > v && &line[v..end] != REDACTED {
                    out.push_str(&line[last..v]);
                    out.push_str(REDACTED);
                    last = end;
                    i = end;
                    continue;
                }
            }
        }
        // `Bearer xyz`, `SAPISIDHASH 123_abc`, `SAPISID1PHASH …`.
        if c.is_ascii_alphabetic()
            && (i == 0 || !is_key_char(line[..i].chars().last().unwrap_or(' ')))
        {
            let word_end = line[i..]
                .char_indices()
                .find(|(_, ch)| !is_key_char(*ch))
                .map_or(line.len(), |(e, _)| i + e);
            let word = line[i..word_end].to_ascii_lowercase();
            let scheme =
                word == "bearer" || (word.starts_with("sapisid") && word.ends_with("hash"));
            if scheme && line[word_end..].starts_with(' ') {
                let v = word_end + 1;
                let end = line[v..]
                    .char_indices()
                    .find(|(_, ch)| ends_value(*ch) && *ch != ' ' || ch.is_whitespace())
                    .map_or(line.len(), |(e, _)| v + e);
                if end > v && &line[v..end] != REDACTED {
                    out.push_str(&line[last..v]);
                    out.push_str(REDACTED);
                    last = end;
                    i = end;
                    continue;
                }
            }
            // Google API keys and `sk-…` keys anywhere.
            let token_end = line[i..]
                .char_indices()
                .find(|(_, ch)| !(ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-'))
                .map_or(line.len(), |(e, _)| i + e);
            let token = &line[i..token_end];
            let google = token.starts_with("AIza") && token.len() >= 35;
            let openai = token.starts_with("sk-") && token.len() >= 23;
            if google || openai {
                out.push_str(&line[last..i]);
                out.push_str(REDACTED);
                last = token_end;
                i = token_end;
                continue;
            }
            i = word_end.max(i + 1);
            continue;
        }
        i += line[i..].chars().next().map_or(1, char::len_utf8);
    }
    if last == 0 {
        Cow::Borrowed(line)
    } else {
        out.push_str(&line[last..]);
        Cow::Owned(out)
    }
}

// ---- the process-wide log -----------------------------------------------------------------

static RING: LazyLock<Mutex<Ring>> = LazyLock::new(|| Mutex::new(Ring::new(CAPACITY)));
/// Changes with every line (the viewer re-renders when it moved).
static GENERATION: AtomicU64 = AtomicU64::new(0);
static FILE_ENABLED: AtomicBool = AtomicBool::new(true);
static FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Records a line (redacted here as well, for callers other than the stderr pipe).
pub fn push(level: Level, text: &str) {
    let text = redact(text.trim_end()).into_owned();
    if text.is_empty() {
        return;
    }
    let line = Line {
        at_ms: now_ms(),
        level,
        text,
    };
    if FILE_ENABLED.load(Ordering::Relaxed) {
        write_file(&line);
    }
    RING.lock().unwrap_or_else(|e| e.into_inner()).push(line);
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// The newest `limit` lines at `max` or more severe, oldest first.
pub fn lines(max: Level, limit: usize) -> Vec<Line> {
    RING.lock()
        .unwrap_or_else(|e| e.into_inner())
        .filtered(max, limit)
}

pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// Where log files go: `logs/` in `MUSIC_CONFIG_DIR` when set (tests and portable
/// setups stay self-contained), else in the platform cache folder.
pub fn folder() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("MUSIC_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir).join("logs"));
    }
    crate::identity::project_dirs().map(|d| d.cache_dir().join("logs"))
}

/// The log file is written (Settings → About → Logs). Off closes it.
pub fn set_file_enabled(on: bool) {
    FILE_ENABLED.store(on, Ordering::Relaxed);
    if !on {
        *FILE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

fn write_file(line: &Line) {
    let mut file = FILE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(dir) = folder() else {
        return;
    };
    let path = dir.join(FILE_NAME);
    let too_big = file
        .as_ref()
        .and_then(|f| f.metadata().ok())
        .is_some_and(|m| m.len() > FILE_LIMIT);
    if too_big {
        *file = None;
        let _ = std::fs::rename(&path, dir.join(OLD_FILE_NAME));
    }
    if file.is_none() {
        let _ = std::fs::create_dir_all(&dir);
        *file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();
    }
    if let Some(f) = file.as_mut() {
        let _ = writeln!(f, "{}", line.format());
    }
}

/// Everything in memory as text (copy, "Save logs…").
pub fn dump(max: Level) -> String {
    let mut out = String::new();
    for line in lines(max, CAPACITY) {
        out.push_str(&line.format());
        out.push('\n');
    }
    out
}

/// `log` crate records (`music-core`, GPUI) into the same log: ours from info up
/// (trace with `MUSIC_TRACE`), other crates' warnings and errors.
struct Logger {
    trace: bool,
}

impl log::Log for Logger {
    fn enabled(&self, meta: &log::Metadata) -> bool {
        let ours = meta.target().starts_with("music");
        match meta.level() {
            log::Level::Error | log::Level::Warn => true,
            log::Level::Info => ours,
            log::Level::Debug | log::Level::Trace => ours && self.trace,
        }
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let level = match record.level() {
            log::Level::Error => Level::Error,
            log::Level::Warn => Level::Warn,
            log::Level::Info => Level::Info,
            _ => Level::Trace,
        };
        let crate_name = record.target().split("::").next().unwrap_or("");
        push(level, &format!("{crate_name}: {}", record.args()));
    }

    fn flush(&self) {}
}

/// Starts capturing: the stderr pipe and the `log` crate. Once, early in `main`.
pub fn install(file: bool) {
    set_file_enabled(file);
    let trace = std::env::var_os("MUSIC_TRACE").is_some();
    if log::set_boxed_logger(Box::new(Logger { trace })).is_ok() {
        log::set_max_level(if trace {
            log::LevelFilter::Trace
        } else {
            log::LevelFilter::Info
        });
    }
    if let Some((reader, console)) = capture::redirect_stderr() {
        let _ = std::thread::Builder::new()
            .name("app-log".into())
            .spawn(move || pump(reader, console));
    }
}

/// Reads stderr's pipe: each line goes on to the real stderr and into the log.
fn pump(reader: std::fs::File, mut console: Option<std::fs::File>) {
    use std::io::BufRead as _;
    let mut reader = std::io::BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&buf);
        let text = text.trim_end_matches(['\n', '\r']);
        let clean = redact(text);
        if let Some(out) = console.as_mut()
            && writeln!(out, "{clean}").is_err()
        {
            console = None;
        }
        push(classify(&clean), &clean);
    }
}

#[cfg(unix)]
mod capture {
    use std::fs::File;
    use std::os::fd::FromRawFd as _;

    /// Points fd 2 at a pipe. Returns its read end and the original stderr.
    pub fn redirect_stderr() -> Option<(File, Option<File>)> {
        let mut fds = [0; 2];
        // SAFETY: plain POSIX calls on fds this function owns; fd 2 is replaced
        // atomically by dup2, and the copies are wrapped in `File`s exactly once.
        unsafe {
            if libc::pipe(fds.as_mut_ptr()) != 0 {
                return None;
            }
            let original = libc::dup(2);
            if libc::dup2(fds[1], 2) < 0 {
                libc::close(fds[0]);
                libc::close(fds[1]);
                if original >= 0 {
                    libc::close(original);
                }
                return None;
            }
            libc::close(fds[1]);
            // Children (the updater helper) don't need the read end.
            libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC);
            let console = (original >= 0).then(|| {
                libc::fcntl(original, libc::F_SETFD, libc::FD_CLOEXEC);
                File::from_raw_fd(original)
            });
            Some((File::from_raw_fd(fds[0]), console))
        }
    }
}

#[cfg(windows)]
mod capture {
    use std::fs::File;
    use std::os::windows::io::FromRawHandle as _;

    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, SetStdHandle};
    use windows::Win32::System::Pipes::CreatePipe;

    /// Points the std error handle at a pipe (Rust's stderr looks the handle up on
    /// every write). Returns its read end and the original handle, if there was one.
    pub fn redirect_stderr() -> Option<(File, Option<File>)> {
        // SAFETY: plain Win32 calls on handles this function creates; the original
        // std handle is only written to by the log thread from now on.
        unsafe {
            let (mut read, mut write) = (HANDLE::default(), HANDLE::default());
            CreatePipe(&mut read, &mut write, None, 0).ok()?;
            let original = GetStdHandle(STD_ERROR_HANDLE)
                .ok()
                .filter(|h| !h.is_invalid() && !h.0.is_null());
            if SetStdHandle(STD_ERROR_HANDLE, write).is_err() {
                return None;
            }
            let console = original.map(|h| File::from_raw_handle(h.0));
            Some((File::from_raw_handle(read.0), console))
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod capture {
    pub fn redirect_stderr() -> Option<(std::fs::File, Option<std::fs::File>)> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(level: Level, text: &str) -> Line {
        Line {
            at_ms: 0,
            level,
            text: text.into(),
        }
    }

    #[test]
    fn the_ring_keeps_the_newest_lines() {
        let mut ring = Ring::new(3);
        for i in 0..5 {
            ring.push(line(Level::Info, &format!("l{i}")));
        }
        assert_eq!(ring.len(), 3);
        let texts = |v: Vec<Line>| v.into_iter().map(|l| l.text).collect::<Vec<_>>();
        assert_eq!(texts(ring.filtered(Level::Trace, 10)), ["l2", "l3", "l4"]);
        assert_eq!(texts(ring.filtered(Level::Trace, 2)), ["l3", "l4"]);
        ring.push(line(Level::Error, "bad"));
        ring.push(line(Level::Warn, "hmm"));
        ring.push(line(Level::Trace, "t"));
        assert_eq!(texts(ring.filtered(Level::Error, 10)), ["bad"]);
        assert_eq!(texts(ring.filtered(Level::Warn, 10)), ["bad", "hmm"]);
        assert_eq!(texts(ring.filtered(Level::Info, 10)), ["bad", "hmm"]);
        assert_eq!(ring.filtered(Level::Trace, 10).len(), 3);
    }

    #[test]
    fn levels_are_guessed() {
        assert_eq!(
            classify("music-gpui: saving the queue failed: disk full"),
            Level::Error
        );
        assert_eq!(classify("music-core: stream: retrying (2)"), Level::Warn);
        assert_eq!(classify("music-gpui: [  1.23s] route Home"), Level::Trace);
        assert_eq!(classify("music-gpui: moved a to b"), Level::Info);
        assert_eq!(stamp(0), "1970-01-01 00:00:00");
        assert_eq!(stamp(1_793_448_000_000), "2026-10-31 12:00:00");
    }

    #[track_caller]
    fn hidden(input: &str, secrets: &[&str]) {
        let out = redact(input);
        for s in secrets {
            assert!(!out.contains(s), "{s:?} leaked: {out}");
        }
        assert!(out.contains(REDACTED), "{out}");
        // Redacting twice changes nothing.
        assert_eq!(redact(&out), out);
    }

    #[test]
    fn secrets_never_reach_the_log() {
        hidden(
            "request headers: Cookie: SAPISID=abc123; __Secure-3PAPISID=def456",
            &["abc123", "def456"],
        );
        hidden(
            "authorization: SAPISIDHASH 1700000000_0123abcd",
            &["0123abcd", "1700000000"],
        );
        hidden("sent Authorization=Bearer ya29.a0Abc-def", &["ya29"]);
        hidden("token Bearer ya29.a0Abc-def ok", &["ya29"]);
        hidden(
            "GET https://music.youtube.com/youtubei/v1/browse?key=AIzaSyC9XL3ZjWddXya6X74dJoCTL-WEYFDNX30&prettyPrint=false",
            &["AIzaSyC9XL3ZjWddXya6X74dJoCTL-WEYFDNX30"],
        );
        hidden(
            "stream https://rr1.googlevideo.com/videoplayback?expire=1&ip=203.0.113.9&sig=AOq0QJ8w&lsig=XYZ1&pot=MnPoToken&n=abc",
            &["AOq0QJ8w", "XYZ1", "MnPoToken", "203.0.113.9"],
        );
        hidden(
            r#"{"sessionKey":"s3cr3t","visitorData":"CgtWaXNpdG9y","name":"Ann"}"#,
            &["s3cr3t", "CgtWaXNpdG9y"],
        );
        hidden("lastfm: sk=0123456789abcdef failed", &["0123456789abcdef"]);
        hidden(
            "ai filter: key sk-proj-abcdefghijklmnopqrstuvwx rejected",
            &["abcdefghijklmnop"],
        );
        hidden("refresh_token: 1//0gAbCdEf", &["0gAbCdEf"]);
        hidden("listenbrainz token=abc-def-123", &["abc-def-123"]);
        hidden("proxy password='hunter2'", &["hunter2"]);
        hidden(
            "HSID=AYQEVn; SSID=Aq1lIi; SID=g.a000",
            &["AYQEVn", "Aq1lIi", "g.a000"],
        );
        // Ordinary lines are untouched (and not copied).
        for plain in [
            "music-gpui: [  1.23s] route Album: Abbey Road",
            "video_id=dQw4w9WgXcQ playlist_id=PL123",
            "music-gpui: radio: +25 tracks (replace=false, more=true)",
            "https://music.youtube.com/watch?v=dQw4w9WgXcQ",
            "the session key is wrong",
            "monkey=3 turkey: 4",
            "Ünïcödé — “quotes”: fine",
        ] {
            assert!(matches!(redact(plain), Cow::Borrowed(_)), "{plain}");
        }
    }
}
