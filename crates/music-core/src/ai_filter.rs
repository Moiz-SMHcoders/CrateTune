// Inspired by ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/aicontentfilter/
// (© Rukamori and ArchiveTune contributors, GPL-3.0), which hides AI-generated music
// by channel blocklists; this one asks a language model of the user's choosing.
//! AI content filter: classifies songs / albums with any OpenAI-compatible
//! chat-completions API (OpenAI, OpenRouter, a local Ollama / LM Studio server, …)
//! into categories the user wants hidden ([`AiCategory`] plus free-text ones),
//! caches the verdict per video / album id (in memory and optionally in a JSON
//! file), and answers "hide this?" from the cache without blocking.
//!
//! Nothing is sent anywhere unless the caller builds an [`AiContentFilter`] and
//! calls [`AiContentFilter::classify`] (the app only does so when the user turned
//! the filter on and configured an endpoint and a model). What is sent per item:
//! its id, title, artists and album name — nothing about the user.
//!
//! Blocking (ureq on the caller's thread); no async runtime.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::http::{agent, read_body};
use crate::{Error, Result};

/// Built-in categories (the user picks which to hide).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiCategory {
    /// Explicit lyrics (profanity, sexual or violent content), even without a badge.
    Explicit,
    /// Music made by generative AI (Suno / Udio style channels, "AI cover"s).
    AiGenerated,
    /// Christmas and other holiday songs.
    Holiday,
    /// Songs for children, nursery rhymes.
    Children,
    /// Sleep sounds, white noise, ambient "focus" loops.
    Noise,
    /// Sped-up / slowed + reverb / nightcore edits.
    SpedUp,
    /// Covers, karaoke and tribute versions.
    Covers,
    /// Live recordings.
    Live,
}

impl AiCategory {
    pub const ALL: [Self; 8] = [
        Self::Explicit,
        Self::AiGenerated,
        Self::Holiday,
        Self::Children,
        Self::Noise,
        Self::SpedUp,
        Self::Covers,
        Self::Live,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::AiGenerated => "ai_generated",
            Self::Holiday => "holiday",
            Self::Children => "children",
            Self::Noise => "noise",
            Self::SpedUp => "sped_up",
            Self::Covers => "covers",
            Self::Live => "live",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.key() == key)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Explicit => "Explicit lyrics",
            Self::AiGenerated => "AI-generated music",
            Self::Holiday => "Holiday music",
            Self::Children => "Children's music",
            Self::Noise => "Sleep sounds & white noise",
            Self::SpedUp => "Sped-up / slowed edits",
            Self::Covers => "Covers & karaoke",
            Self::Live => "Live recordings",
        }
    }

    /// What the model is told the category means.
    fn definition(self) -> &'static str {
        match self {
            Self::Explicit => {
                "explicit lyrics: profanity, sexual or graphic violent content, even if not \
                 labelled explicit"
            }
            Self::AiGenerated => {
                "music generated with AI tools (Suno, Udio and similar), AI voice covers, \
                 AI 'artists'"
            }
            Self::Holiday => "Christmas and other holiday songs",
            Self::Children => "songs made for children, nursery rhymes, kids' TV songs",
            Self::Noise => "sleep sounds, white / brown noise, rain sounds, ambient focus loops",
            Self::SpedUp => "sped-up, slowed + reverb, nightcore or similar re-edits",
            Self::Covers => "covers, karaoke, instrumental and tribute versions by others",
            Self::Live => "live recordings and concert performances",
        }
    }
}

/// What gets classified.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Subject {
    /// Video id or album browse id (the cache key).
    pub id: String,
    pub title: String,
    pub artists: String,
    pub album: Option<String>,
    /// Already badged explicit by YouTube Music (a hint for the model).
    pub explicit_badge: bool,
}

impl Subject {
    pub fn from_track(track: &crate::Track) -> Self {
        Self {
            id: track.video_id.clone(),
            title: track.title.clone(),
            artists: track.artists.clone(),
            album: track.album.clone().filter(|a| !a.is_empty()),
            explicit_badge: track.explicit,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiFilterConfig {
    /// Full chat-completions URL, an API root ending in `/v1`, or a bare host
    /// (`https://host` → `https://host/v1/chat/completions`).
    pub endpoint: String,
    /// Sent as `Authorization: Bearer …` when not empty (local servers need none).
    pub api_key: String,
    pub model: String,
    pub categories: Vec<AiCategory>,
    /// Extra categories described by the user ("songs about cars").
    pub custom: Vec<String>,
    pub timeout: Duration,
    /// Items per request.
    pub batch_size: usize,
}

impl Default for AiFilterConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            api_key: String::new(),
            model: String::new(),
            categories: vec![AiCategory::Explicit],
            custom: Vec::new(),
            timeout: Duration::from_secs(60),
            batch_size: 25,
        }
    }
}

impl AiFilterConfig {
    /// The category keys asked about: built-ins, then `custom:<text>`.
    pub fn keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.categories.iter().map(|c| c.key().into()).collect();
        keys.extend(
            self.custom
                .iter()
                .map(|c| c.trim())
                .filter(|c| !c.is_empty())
                .map(|c| format!("custom:{}", c.chars().take(80).collect::<String>())),
        );
        keys.sort();
        keys.dedup();
        keys
    }

    pub fn is_usable(&self) -> bool {
        !self.model.trim().is_empty()
            && crate::lyrics::translate::chat_completion_endpoint(&self.endpoint).is_ok()
            && !self.keys().is_empty()
    }

    fn system_prompt(&self, keys: &[String]) -> String {
        let mut lines = vec![
            "You classify music for a content filter.".to_owned(),
            "For every item, decide which of these categories apply:".to_owned(),
        ];
        for key in keys {
            let definition = match AiCategory::from_key(key) {
                Some(c) => c.definition().to_owned(),
                None => key.trim_start_matches("custom:").to_owned(),
            };
            lines.push(format!("- \"{key}\": {definition}"));
        }
        lines.push(
            "Use only these category names. Judge from the title, artists and album and from \
             what you know about the song; when unsure, leave the category out."
                .into(),
        );
        lines.push(
            "Reply with one JSON object and nothing else: keys are the item ids, values are \
             arrays of the categories that apply (an empty array when none do)."
                .into(),
        );
        lines.join("\n")
    }

    /// The request body for one batch (the model, the prompt, the items as JSON).
    pub fn request_body(&self, subjects: &[Subject]) -> Result<Value> {
        let model = self.model.trim();
        if model.is_empty() {
            return Err(Error::Parse("no AI model configured".into()));
        }
        let keys = self.keys();
        let items: Vec<Value> = subjects
            .iter()
            .map(|s| {
                let mut item = json!({
                    "id": s.id,
                    "title": s.title,
                    "artists": s.artists,
                });
                if let Some(album) = &s.album {
                    item["album"] = json!(album);
                }
                if s.explicit_badge {
                    item["markedExplicit"] = json!(true);
                }
                item
            })
            .collect();
        Ok(json!({
            "model": model,
            "temperature": 0,
            "messages": [
                { "role": "system", "content": self.system_prompt(&keys) },
                { "role": "user", "content": Value::Array(items).to_string() },
            ],
        }))
    }
}

/// The verdicts in a chat-completion response: id → categories (only the asked
/// ones, only for the asked ids). Tolerates Markdown fences and prose around the
/// JSON object.
pub fn parse_response(
    body: &str,
    subjects: &[Subject],
    keys: &[String],
) -> Result<HashMap<String, Vec<String>>> {
    let value: Value = serde_json::from_str(body)
        .map_err(|e| Error::Parse(format!("AI filter: not JSON: {e}")))?;
    if let Some(message) = value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| value.get("error").and_then(Value::as_str))
    {
        return Err(Error::Parse(format!("AI filter: {message}")));
    }
    let content = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Parse("AI filter: no message in the response".into()))?;
    let start = content.find('{');
    let end = content.rfind('}');
    let object = match (start, end) {
        (Some(s), Some(e)) if e > s => &content[s..=e],
        _ => {
            return Err(Error::Parse(
                "AI filter: no JSON object in the answer".into(),
            ));
        }
    };
    let verdicts: serde_json::Map<String, Value> = serde_json::from_str(object)
        .map_err(|e| Error::Parse(format!("AI filter: bad JSON in the answer: {e}")))?;
    let asked: HashSet<&str> = subjects.iter().map(|s| s.id.as_str()).collect();
    let mut out = HashMap::new();
    for (id, labels) in verdicts {
        if !asked.contains(id.as_str()) {
            continue;
        }
        let labels: Vec<String> = match labels {
            Value::Array(items) => items
                .iter()
                .filter_map(Value::as_str)
                .filter(|l| keys.iter().any(|k| k == l))
                .map(str::to_owned)
                .collect(),
            // `"id": "explicit"` or `"id": null`.
            Value::String(l) if keys.contains(&l) => vec![l],
            _ => Vec::new(),
        };
        out.insert(id, labels);
    }
    Ok(out)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Verdict {
    /// The categories that apply.
    labels: Vec<String>,
    /// The categories the model was asked about.
    asked: Vec<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct Cache {
    entries: HashMap<String, Verdict>,
    #[serde(skip)]
    order: VecDeque<String>,
    #[serde(skip)]
    dirty: bool,
}

const MAX_CACHED: usize = 20_000;

impl Cache {
    fn insert(&mut self, id: String, verdict: Verdict) {
        if self.entries.insert(id.clone(), verdict).is_none() {
            self.order.push_back(id);
        }
        while self.order.len() > MAX_CACHED {
            if let Some(old) = self.order.pop_front() {
                self.entries.remove(&old);
            }
        }
        self.dirty = true;
    }
}

/// The classifier + its cache. `Send + Sync`; share it behind an `Arc`.
pub struct AiContentFilter {
    config: Mutex<AiFilterConfig>,
    cache: Mutex<Cache>,
    path: Option<PathBuf>,
    /// One classification at a time (pages loading together would ask twice).
    busy: Mutex<()>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl AiContentFilter {
    /// `cache_path`: a JSON file the verdicts are loaded from and saved to.
    pub fn new(config: AiFilterConfig, cache_path: Option<PathBuf>) -> Self {
        let mut cache: Cache = cache_path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        cache.order = cache.entries.keys().cloned().collect();
        Self {
            config: Mutex::new(config),
            cache: Mutex::new(cache),
            path: cache_path,
            busy: Mutex::new(()),
        }
    }

    pub fn config(&self) -> AiFilterConfig {
        lock(&self.config).clone()
    }

    pub fn set_config(&self, config: AiFilterConfig) {
        *lock(&self.config) = config;
    }

    /// From the cache only: `Some(categories that apply and are selected)` when
    /// the item was classified for every selected category, `None` otherwise.
    pub fn cached(&self, id: &str) -> Option<Vec<String>> {
        let keys = lock(&self.config).keys();
        let cache = lock(&self.cache);
        let verdict = cache.entries.get(id)?;
        keys.iter().all(|k| verdict.asked.contains(k)).then(|| {
            verdict
                .labels
                .iter()
                .filter(|l| keys.contains(l))
                .cloned()
                .collect()
        })
    }

    /// Whether the cache says to hide it (unknown = keep).
    pub fn hidden(&self, id: &str) -> bool {
        self.cached(id).is_some_and(|labels| !labels.is_empty())
    }

    /// Classified items in the cache.
    pub fn cached_count(&self) -> usize {
        lock(&self.cache).entries.len()
    }

    pub fn clear_cache(&self) {
        let mut cache = lock(&self.cache);
        *cache = Cache::default();
        cache.dirty = true;
        drop(cache);
        self.save();
    }

    /// Classifies the subjects the cache can't answer for (in batches), stores
    /// the verdicts and saves the cache file. Blocking (one request per batch).
    /// Returns how many items were classified now.
    pub fn classify(&self, subjects: &[Subject]) -> Result<usize> {
        let config = self.config();
        if !config.is_usable() {
            return Ok(0);
        }
        let _busy = lock(&self.busy);
        let mut seen = HashSet::new();
        let todo: Vec<Subject> = subjects
            .iter()
            .filter(|s| !s.id.is_empty() && seen.insert(s.id.clone()))
            .filter(|s| self.cached(&s.id).is_none())
            .cloned()
            .collect();
        let keys = config.keys();
        let mut done = 0;
        let mut first_error = None;
        for batch in todo.chunks(config.batch_size.max(1)) {
            match self.request(&config, batch, &keys) {
                Ok(verdicts) => {
                    let mut cache = lock(&self.cache);
                    for subject in batch {
                        // Items the model skipped are left unknown (asked again later).
                        if let Some(labels) = verdicts.get(&subject.id) {
                            cache.insert(
                                subject.id.clone(),
                                Verdict {
                                    labels: labels.clone(),
                                    asked: keys.clone(),
                                },
                            );
                            done += 1;
                        }
                    }
                }
                Err(e) => {
                    first_error.get_or_insert(e);
                    break;
                }
            }
        }
        self.save();
        match first_error {
            Some(e) if done == 0 => Err(e),
            _ => Ok(done),
        }
    }

    fn request(
        &self,
        config: &AiFilterConfig,
        batch: &[Subject],
        keys: &[String],
    ) -> Result<HashMap<String, Vec<String>>> {
        let url = crate::lyrics::translate::chat_completion_endpoint(&config.endpoint)?;
        let body = config.request_body(batch)?;
        let mut req = agent()
            .post(&url)
            .config()
            .http_status_as_error(false)
            .timeout_global(Some(config.timeout))
            .build();
        let key = config.api_key.trim();
        let auth = format!("Bearer {key}");
        if !key.is_empty() {
            req = req.header("Authorization", &auth);
        }
        let mut resp = req.send_json(&body)?;
        let status = resp.status();
        let text = String::from_utf8_lossy(&read_body(&mut resp)?).into_owned();
        if !status.is_success() {
            let detail = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| {
                    v.pointer("/error/message")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| text.chars().take(200).collect());
            return Err(Error::Parse(format!("AI filter: HTTP {status}: {detail}")));
        }
        parse_response(&text, batch, keys)
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let mut cache = lock(&self.cache);
        if !cache.dirty {
            return;
        }
        cache.dirty = false;
        let Ok(bytes) = serde_json::to_vec(&*cache) else {
            return;
        };
        drop(cache);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex as StdMutex};

    use super::*;

    fn subject(id: &str, title: &str) -> Subject {
        Subject {
            id: id.into(),
            title: title.into(),
            artists: "Someone".into(),
            album: Some("Album".into()),
            explicit_badge: false,
        }
    }

    #[test]
    fn request_body_and_prompt() {
        let config = AiFilterConfig {
            endpoint: "http://localhost:11434/v1".into(),
            model: "llama3".into(),
            categories: vec![AiCategory::Explicit, AiCategory::Holiday],
            custom: vec!["  songs about cars ".into(), String::new()],
            ..AiFilterConfig::default()
        };
        assert_eq!(
            config.keys(),
            vec!["custom:songs about cars", "explicit", "holiday"]
        );
        assert!(config.is_usable());
        let body = config
            .request_body(&[subject("aaaaaaaaaaa", "Last Christmas")])
            .unwrap();
        assert_eq!(body["model"], "llama3");
        assert_eq!(body["temperature"], 0);
        let system = body["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains(r#"- "holiday": Christmas"#), "{system}");
        assert!(system.contains(r#"- "custom:songs about cars": songs about cars"#));
        let user: Value =
            serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(user[0]["id"], "aaaaaaaaaaa");
        assert_eq!(user[0]["album"], "Album");
        assert!(user[0].get("markedExplicit").is_none());
        // Not usable without a model or a valid endpoint.
        for bad in [
            AiFilterConfig {
                model: String::new(),
                ..config.clone()
            },
            AiFilterConfig {
                endpoint: "ftp://x".into(),
                ..config.clone()
            },
            AiFilterConfig {
                categories: Vec::new(),
                custom: Vec::new(),
                ..config
            },
        ] {
            assert!(!bad.is_usable());
        }
    }

    #[test]
    fn parses_answers_leniently() {
        let subjects = [subject("a", "A"), subject("b", "B"), subject("c", "C")];
        let keys = vec!["explicit".to_owned(), "holiday".to_owned()];
        let wrap = |content: &str| {
            json!({ "choices": [{ "message": { "role": "assistant", "content": content } }] })
                .to_string()
        };
        let got = parse_response(
            &wrap(
                "Sure! ```json\n{\"a\": [\"explicit\", \"made_up\"], \"b\": [], \"z\": [\"explicit\"], \"c\": \"holiday\"}\n```",
            ),
            &subjects,
            &keys,
        )
        .unwrap();
        assert_eq!(got["a"], vec!["explicit"]);
        assert!(got["b"].is_empty());
        assert_eq!(got["c"], vec!["holiday"]);
        assert!(!got.contains_key("z"), "not asked");
        assert!(parse_response(&wrap("no idea"), &subjects, &keys).is_err());
        let err = parse_response(
            r#"{"error":{"message":"invalid api key"}}"#,
            &subjects,
            &keys,
        )
        .unwrap_err();
        assert!(err.to_string().contains("invalid api key"));
    }

    /// ("path auth", body) of each request the stub saw.
    type Requests = Arc<StdMutex<Vec<(String, Value)>>>;

    /// A local stand-in for the API: records requests, answers from `reply`.
    fn stub(reply: impl Fn(&Value) -> String + Send + 'static) -> (String, Requests) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
                let (mut length, mut auth) = (0usize, String::new());
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header.trim().is_empty() {
                        break;
                    }
                    let lower = header.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap();
                    }
                    if lower.starts_with("authorization:") {
                        auth = header["authorization:".len()..].trim().to_owned();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let body: Value = serde_json::from_slice(&body).unwrap();
                let answer = reply(&body);
                log.lock().unwrap().push((format!("{path} {auth}"), body));
                let response = json!({
                    "choices": [{ "message": { "role": "assistant", "content": answer } }]
                })
                .to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                )
                .unwrap();
            }
        });
        (url, seen)
    }

    #[test]
    fn classifies_through_a_local_stub_and_caches() {
        // Titles with "Christmas" are holiday songs, "(Explicit)" ones explicit.
        let (url, seen) = stub(|body| {
            let items: Value =
                serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
            let mut out = serde_json::Map::new();
            for item in items.as_array().unwrap() {
                let title = item["title"].as_str().unwrap();
                let mut labels = Vec::new();
                if title.contains("Christmas") {
                    labels.push("holiday");
                }
                if title.contains("(Explicit)") {
                    labels.push("explicit");
                }
                out.insert(item["id"].as_str().unwrap().into(), json!(labels));
            }
            Value::Object(out).to_string()
        });
        let dir = std::env::temp_dir().join(format!("ai-filter-test-{}", std::process::id()));
        let path = dir.join("ai_filter.json");
        let _ = std::fs::remove_dir_all(&dir);
        let config = AiFilterConfig {
            endpoint: url,
            api_key: "sk-test".into(),
            model: "test-model".into(),
            categories: vec![AiCategory::Holiday],
            batch_size: 2,
            ..AiFilterConfig::default()
        };
        let filter = AiContentFilter::new(config.clone(), Some(path.clone()));
        let subjects = vec![
            subject("aaaaaaaaaaa", "Last Christmas"),
            subject("bbbbbbbbbbb", "Song (Explicit)"),
            subject("ccccccccccc", "Plain song"),
            subject("aaaaaaaaaaa", "Last Christmas"),
        ];
        assert!(!filter.hidden("aaaaaaaaaaa"), "unknown = keep");
        assert_eq!(filter.classify(&subjects).unwrap(), 3);
        {
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 2, "3 unique items in batches of 2");
            assert_eq!(seen[0].0, "/v1/chat/completions Bearer sk-test");
            assert_eq!(seen[0].1["model"], "test-model");
        }
        assert!(filter.hidden("aaaaaaaaaaa"));
        assert!(!filter.hidden("bbbbbbbbbbb"), "explicit wasn't selected");
        assert!(!filter.hidden("ccccccccccc"));
        // Cached: no new request.
        assert_eq!(filter.classify(&subjects).unwrap(), 0);
        assert_eq!(seen.lock().unwrap().len(), 2);
        // A newly selected category needs asking again; the cache file persists.
        filter.set_config(AiFilterConfig {
            categories: vec![AiCategory::Holiday, AiCategory::Explicit],
            ..config.clone()
        });
        assert_eq!(filter.cached("bbbbbbbbbbb"), None);
        assert_eq!(filter.classify(&subjects).unwrap(), 3);
        assert!(filter.hidden("bbbbbbbbbbb"));
        let reloaded = AiContentFilter::new(
            AiFilterConfig {
                categories: vec![AiCategory::Explicit],
                ..config
            },
            Some(path.clone()),
        );
        assert_eq!(reloaded.cached_count(), 3);
        assert!(reloaded.hidden("bbbbbbbbbbb"));
        assert!(
            !reloaded.hidden("aaaaaaaaaaa"),
            "holiday no longer selected"
        );
        reloaded.clear_cache();
        assert_eq!(reloaded.cached_count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_is_sent_when_not_configured() {
        let filter = AiContentFilter::new(AiFilterConfig::default(), None);
        assert_eq!(filter.classify(&[subject("a", "A")]).unwrap(), 0);
    }
}
