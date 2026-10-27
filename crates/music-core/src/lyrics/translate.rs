// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/lyrics/translation/{StandardLyricsTranslationRepository,TranslateLyricsUseCase}.kt and ai/{AiTextService,AiLyricsTranslator,AiJsonUtils,AiRequestParameters}.kt — © Rukamori and ArchiveTune contributors, GPL-3.0
//! Lyrics translation, aligned to the original lines.
//!
//! * [`GoogleTranslator`] — Google Translate's free `translate_a/single` endpoint
//!   (no key), batched as newline-joined lines; a batch whose line count comes back
//!   different is split in halves until it matches.
//! * [`AiTranslator`] — any OpenAI-compatible chat-completions API (OpenAI,
//!   OpenRouter, a local server, …) with ArchiveTune's prompt and JSON handling.
//!
//! Both return one `Option<String>` per input line: `None` for blank lines and
//! for lines the translation left unchanged. Repeated lines are sent once.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::providers::{http_post_form, http_post_json};
use super::types::Lyrics;
use crate::{Error, Result};

/// Something that translates lines of lyrics.
pub trait LyricsTranslator: Send + Sync {
    /// One entry per input line (see the module docs).
    fn translate_lines(&self, lines: &[&str], target_language: &str)
    -> Result<Vec<Option<String>>>;

    /// Translations aligned with `lyrics.lines`.
    fn translate_lyrics(
        &self,
        lyrics: &Lyrics,
        target_language: &str,
    ) -> Result<Vec<Option<String>>> {
        let lines: Vec<&str> = lyrics.lines.iter().map(|l| l.text.as_str()).collect();
        self.translate_lines(&lines, target_language)
    }
}

/// Unique non-blank lines (trimmed) and, per input line, the index into them.
fn dedupe<'a>(lines: &[&'a str]) -> (Vec<&'a str>, Vec<Option<usize>>) {
    let mut unique: Vec<&str> = Vec::new();
    let mut seen: HashMap<&str, usize> = HashMap::new();
    let map = lines
        .iter()
        .map(|l| {
            let t = l.trim();
            (!t.is_empty()).then(|| {
                *seen.entry(t).or_insert_with(|| {
                    unique.push(t);
                    unique.len() - 1
                })
            })
        })
        .collect();
    (unique, map)
}

/// Spreads translations of the unique lines back over the input lines.
fn realign(unique: &[&str], map: &[Option<usize>], translated: &[String]) -> Vec<Option<String>> {
    debug_assert_eq!(unique.len(), translated.len());
    map.iter()
        .map(|idx| {
            let i = (*idx)?;
            let t = translated[i].trim();
            (!t.is_empty() && !t.eq_ignore_ascii_case(unique[i])).then(|| t.to_owned())
        })
        .collect()
}

/// Groups `texts` into batches of at most `max_items` / about `max_chars`.
fn batches(
    texts: &[&str],
    max_items: usize,
    max_chars: usize,
    sep_len: usize,
) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let (mut start, mut chars) = (0, 0);
    for (i, t) in texts.iter().enumerate() {
        let extra = if i > start { sep_len } else { 0 } + t.len();
        if i > start && (i - start >= max_items || chars + extra > max_chars) {
            out.push(start..i);
            start = i;
            chars = t.len();
        } else {
            chars += extra;
        }
    }
    if start < texts.len() {
        out.push(start..texts.len());
    }
    out
}

// --- Google Translate ------------------------------------------------------------------

/// Google Translate's free endpoint. No key; rate limits apply.
#[derive(Debug, Clone)]
pub struct GoogleTranslator {
    pub endpoint: String,
    /// The `client` form field (ArchiveTune uses `at`).
    pub client: String,
    pub user_agent: String,
    pub timeout: Duration,
    pub max_items_per_batch: usize,
    pub max_chars_per_batch: usize,
}

impl Default for GoogleTranslator {
    fn default() -> Self {
        Self {
            endpoint: "https://translate.googleapis.com/translate_a/single".into(),
            client: "at".into(),
            user_agent: "music-macos".into(),
            timeout: Duration::from_secs(30),
            max_items_per_batch: 50,
            max_chars_per_batch: 4000,
        }
    }
}

impl GoogleTranslator {
    /// Translates one block of text (`target`: a language code such as `en`, `ja`, `zh-CN`).
    pub fn translate_text(&self, text: &str, target: &str) -> Result<String> {
        let target = validate_language_code(target)?;
        let resp = http_post_form(
            &self.endpoint,
            &[
                ("client", &self.client),
                ("dt", "t"),
                ("ie", "UTF-8"),
                ("oe", "UTF-8"),
                ("sl", "auto"),
                ("tl", target),
                ("q", text),
            ],
            &[("User-Agent", &self.user_agent)],
            Some(self.timeout),
        )?;
        if !resp.ok() {
            return Err(Error::Parse(format!(
                "translation service returned HTTP {}",
                resp.status
            )));
        }
        parse_google_response(&resp.body)
    }
}

impl GoogleTranslator {
    /// Translates `batch` (single lines) keeping one result per line: newline-joined
    /// first, halving the batch whenever the service merges or splits lines.
    fn translate_batch(&self, batch: &[&str], target: &str, out: &mut Vec<String>) -> Result<()> {
        if let [single] = batch {
            out.push(self.translate_text(single, target)?.replace('\n', " "));
            return Ok(());
        }
        let joined = batch.join("\n");
        let parts = split_lines(&self.translate_text(&joined, target)?);
        if parts.len() == batch.len() {
            out.extend(parts);
            return Ok(());
        }
        log::debug!(
            "google translate returned {} lines for {}; splitting the batch",
            parts.len(),
            batch.len()
        );
        let (a, b) = batch.split_at(batch.len() / 2);
        self.translate_batch(a, target, out)?;
        self.translate_batch(b, target, out)
    }
}

impl LyricsTranslator for GoogleTranslator {
    fn translate_lines(&self, lines: &[&str], target: &str) -> Result<Vec<Option<String>>> {
        validate_language_code(target)?;
        let (unique, map) = dedupe(lines);
        // A line must not contain the batch separator itself.
        let flattened: Vec<String> = unique
            .iter()
            .map(|t| t.replace(['\n', '\r'], " "))
            .collect();
        let flat: Vec<&str> = flattened.iter().map(String::as_str).collect();
        let mut translated = Vec::with_capacity(unique.len());
        for range in batches(&flat, self.max_items_per_batch, self.max_chars_per_batch, 1) {
            self.translate_batch(&flat[range], target, &mut translated)?;
        }
        Ok(realign(&unique, &map, &translated))
    }
}

/// `[[["translated","original",…],…],…]` → the concatenated translation.
pub(crate) fn parse_google_response(body: &str) -> Result<String> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| Error::Parse(format!("translation service returned malformed data: {e}")))?;
    let segments = v
        .get(0)
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Parse("translation service returned no translated text".into()))?;
    let text: String = segments.iter().filter_map(|s| s.get(0)?.as_str()).collect();
    if text.trim().is_empty() {
        return Err(Error::Parse(
            "translation service returned empty text".into(),
        ));
    }
    Ok(text)
}

fn split_lines(text: &str) -> Vec<String> {
    text.trim_end_matches('\n')
        .split('\n')
        .map(|s| s.trim().to_owned())
        .collect()
}

fn validate_language_code(code: &str) -> Result<&str> {
    let code = code.trim();
    let ok = !code.is_empty()
        && code.len() <= 16
        && code
            .split('-')
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric()));
    if ok {
        Ok(code)
    } else {
        Err(Error::Parse(format!("unsupported language code: {code:?}")))
    }
}

// --- AI (OpenAI-compatible) ------------------------------------------------------------

/// Chat-completions translator. Everything is supplied by the caller.
#[derive(Debug, Clone)]
pub struct AiTranslator {
    /// Full chat-completions URL, an API root ending in `/v1`, or a bare host
    /// (`https://host` → `https://host/v1/chat/completions`).
    pub endpoint: String,
    pub api_key: String,
    pub model: String,
    /// Appended to the system prompt (style, dialect, …).
    pub custom_prompt: String,
    /// Extra request fields as a JSON object (`{"temperature":0.3,"max_tokens":4000}`),
    /// validated like ArchiveTune's request parameters; `null` removes a default field.
    pub request_parameters: String,
    pub timeout: Duration,
    pub max_items_per_batch: usize,
    pub max_chars_per_batch: usize,
}

impl AiTranslator {
    pub fn new(
        endpoint: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            api_key: api_key.into(),
            model: model.into(),
            custom_prompt: String::new(),
            request_parameters: String::new(),
            timeout: Duration::from_secs(90),
            max_items_per_batch: 80,
            max_chars_per_batch: 6000,
        }
    }

    pub fn with_custom_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.custom_prompt = prompt.into();
        self
    }

    /// The system prompt for `count` lines (ArchiveTune's, plus the custom prompt).
    pub fn system_prompt(&self, target_language: &str, count: usize) -> String {
        let target = normalize_target_language(target_language);
        let base = format!(
            "You are an expert song lyrics translator.\n\
             Translate each input string into {target} with natural, accurate lyric phrasing.\n\
             Preserve meaning, tone, profanity level, names, repeated hooks, and line-level intent.\n\
             Do not add timestamps, IDs, XML, markdown, explanations, or extra lines.\n\
             Return only a JSON array of strings with exactly {count} items in the same order.\n\
             The caller will reconstruct the synced LRC lyrics container separately."
        );
        match self.custom_prompt.trim() {
            "" => base,
            custom => format!("{base}\n\n{custom}"),
        }
    }

    /// The JSON request body for one batch.
    pub fn request_body(&self, target_language: &str, lines: &[&str]) -> Result<Value> {
        let model = self.model.trim();
        if model.is_empty() {
            return Err(Error::Parse("no AI model configured".into()));
        }
        let reasoning = is_reasoning_model(model);
        let mut body = json!({
            "model": model,
            "messages": [
                { "role": "system", "content": self.system_prompt(target_language, lines.len()) },
                { "role": "user", "content": serde_json::to_string(lines).unwrap_or_default() },
            ],
        });
        let token_key = if reasoning {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        body[token_key] = json!(8192);
        if !reasoning {
            body["temperature"] = json!(0.15);
        }
        apply_request_parameters(&mut body, &self.request_parameters)?;
        Ok(body)
    }

    fn translate_batch(&self, target_language: &str, lines: &[&str]) -> Result<Vec<String>> {
        if self.api_key.trim().is_empty() {
            return Err(Error::Parse(
                "AI provider is not configured (no API key)".into(),
            ));
        }
        let url = chat_completion_endpoint(&self.endpoint)?;
        let body = self.request_body(target_language, lines)?;
        let auth = format!("Bearer {}", self.api_key.trim());
        let resp = http_post_json(&url, &body, &[("Authorization", &auth)], Some(self.timeout))?;
        if !resp.ok() {
            return Err(api_error(resp.status, &resp.body));
        }
        let content = parse_completion(&resp.body)?;
        parse_translations(&content, lines.len())
    }
}

impl LyricsTranslator for AiTranslator {
    fn translate_lines(
        &self,
        lines: &[&str],
        target_language: &str,
    ) -> Result<Vec<Option<String>>> {
        let (unique, map) = dedupe(lines);
        let mut translated = Vec::with_capacity(unique.len());
        for range in batches(
            &unique,
            self.max_items_per_batch,
            self.max_chars_per_batch,
            0,
        ) {
            translated.extend(self.translate_batch(target_language, &unique[range])?);
        }
        Ok(realign(&unique, &map, &translated))
    }
}

/// `ENGLISH` / `brazilian_portuguese` → `English` / `Brazilian portuguese`.
fn normalize_target_language(language: &str) -> String {
    let l = language.trim();
    let l = if l.is_empty() { "ENGLISH" } else { l };
    let lower = l.replace('_', " ").to_lowercase();
    let mut chars = lower.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// `gpt-5…` and `o1`/`o3…` take `max_completion_tokens` and no temperature.
fn is_reasoning_model(model: &str) -> bool {
    let name = model.trim().rsplit('/').next().unwrap_or("").to_lowercase();
    if let Some(rest) = name.strip_prefix("gpt-") {
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        return digits.parse::<u32>().is_ok_and(|v| v >= 5);
    }
    name.strip_prefix('o')
        .and_then(|r| r.split('-').next())
        .and_then(|v| v.parse::<u32>().ok())
        .is_some_and(|v| v > 0)
}

/// Resolves the chat-completions URL (see [`AiTranslator::endpoint`]).
pub(crate) fn chat_completion_endpoint(endpoint: &str) -> Result<String> {
    let e = endpoint.trim();
    let bad = || Error::Parse("AI endpoint must be an HTTP or HTTPS URL".into());
    let (scheme, rest) = e.split_once("://").ok_or_else(bad)?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err(bad());
    }
    let (authority, path_and_query) = match rest.find(['/', '?']) {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    if authority.is_empty() {
        return Err(bad());
    }
    if authority.contains('@') || e.contains('#') {
        return Err(Error::Parse(
            "AI endpoint cannot contain credentials or a fragment".into(),
        ));
    }
    let (path, query) = match path_and_query.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (path_and_query, None),
    };
    let path = path.trim_end_matches('/');
    let path = if path.is_empty() {
        "/v1/chat/completions".to_owned()
    } else if path.ends_with("/v1") {
        format!("{path}/chat/completions")
    } else {
        path.to_owned()
    };
    Ok(match query {
        Some(q) => format!("{scheme}://{authority}{path}?{q}"),
        None => format!("{scheme}://{authority}{path}"),
    })
}

const RESERVED_PARAMETERS: [&str; 4] = ["model", "messages", "stream", "n"];

/// Validates and merges caller request parameters into `body` (null removes a field).
fn apply_request_parameters(body: &mut Value, raw: &str) -> Result<()> {
    if raw.trim().is_empty() {
        return Ok(());
    }
    let err = |m: &str| Error::Parse(format!("request parameters: {m}"));
    if raw.len() > 16_384 {
        return Err(err("too large"));
    }
    let params: Map<String, Value> = match serde_json::from_str(raw) {
        Ok(Value::Object(m)) => m,
        _ => return Err(err("must be a JSON object")),
    };
    if params
        .keys()
        .any(|k| k.trim().is_empty() || RESERVED_PARAMETERS.contains(&k.as_str()))
    {
        return Err(err("cannot override model, messages, stream, or n"));
    }
    for name in ["max_tokens", "max_completion_tokens"] {
        match params.get(name) {
            None | Some(Value::Null) => {}
            Some(v) if v.as_i64().is_some_and(|n| n > 0) => {}
            Some(_) => {
                return Err(err(&format!("{name} must be a positive integer or null")));
            }
        }
    }
    let set = |k: &str| params.get(k).is_some_and(|v| !v.is_null());
    if set("max_tokens") && set("max_completion_tokens") {
        return Err(err("use only one token limit parameter"));
    }
    for (name, max) in [("temperature", 2.0), ("top_p", 1.0)] {
        match params.get(name) {
            None | Some(Value::Null) => {}
            Some(v)
                if v.as_f64()
                    .is_some_and(|n| n.is_finite() && (0.0..=max).contains(&n)) => {}
            Some(_) => return Err(err(&format!("{name} is outside its supported range"))),
        }
    }
    let (drop_completion, drop_max) = (set("max_tokens"), set("max_completion_tokens"));
    let Some(obj) = body.as_object_mut() else {
        return Err(err("request body is not an object"));
    };
    if drop_completion {
        obj.remove("max_completion_tokens");
    }
    if drop_max {
        obj.remove("max_tokens");
    }
    for (k, v) in params {
        if v.is_null() {
            obj.remove(&k);
        } else {
            obj.insert(k, v);
        }
    }
    Ok(())
}

/// The assistant text of a chat-completions response.
pub(crate) fn parse_completion(raw: &str) -> Result<String> {
    let v: Value =
        serde_json::from_str(raw).map_err(|e| Error::Parse(format!("AI API: bad JSON: {e}")))?;
    let choice = v
        .pointer("/choices/0")
        .ok_or_else(|| Error::Parse("AI API returned no completion choices".into()))?;
    match choice.get("finish_reason").and_then(Value::as_str) {
        Some("length") => {
            return Err(Error::Parse(
                "AI response exceeded the token limit. Increase the token limit in request parameters."
                    .into(),
            ));
        }
        Some("content_filter") => {
            return Err(Error::Parse("AI provider filtered the response".into()));
        }
        _ => {}
    }
    let message = choice
        .get("message")
        .ok_or_else(|| Error::Parse("AI API returned no completion message".into()))?;
    let content = match message.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|p| p.get("text")?.as_str())
            .collect(),
        _ => String::new(),
    };
    if content.trim().is_empty() {
        if message.get("refusal").is_some_and(|r| !r.is_null()) {
            return Err(Error::Parse("AI provider refused the request".into()));
        }
        return Err(Error::Parse(
            "AI API returned no text. Check the model and its token or reasoning parameters."
                .into(),
        ));
    }
    Ok(content)
}

/// The JSON array of strings in an AI reply (tolerates code fences and chatter).
pub(crate) fn parse_translations(content: &str, expected: usize) -> Result<Vec<String>> {
    let no_array = || Error::Parse("AI response did not contain a JSON array".into());
    let trimmed = content.trim();
    let trimmed = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    let trimmed = trimmed.strip_suffix("```").unwrap_or(trimmed).trim();
    let start = trimmed.find('[').ok_or_else(no_array)?;
    let end = trimmed.rfind(']').ok_or_else(no_array)?;
    if end <= start {
        return Err(no_array());
    }
    let items: Vec<Value> = serde_json::from_str(&trimmed[start..=end])
        .map_err(|e| Error::Parse(format!("AI response is not a JSON array: {e}")))?;
    if items.len() != expected {
        return Err(Error::Parse(format!(
            "AI response changed the lyric segment count ({} instead of {expected})",
            items.len()
        )));
    }
    items
        .into_iter()
        .map(|v| match v {
            Value::String(s) => Ok(s),
            _ => Err(Error::Parse(
                "AI response contained a non-text lyric segment".into(),
            )),
        })
        .collect()
}

fn api_error(status: u16, raw: &str) -> Error {
    let message = serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .and_then(Value::as_str)
                .or_else(|| v.get("error").and_then(Value::as_str))
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| {
            if raw.trim().is_empty() {
                format!("HTTP {status}")
            } else {
                raw.chars().take(300).collect()
            }
        });
    Error::Parse(format!("AI API failed ({status}): {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::providers::fixture;

    #[test]
    fn google_batch_response() {
        let text = parse_google_response(&fixture("google_translate_batch.json")).unwrap();
        let parts = split_lines(&text);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], "Media trolls with an invincible smile");
        assert_eq!(parts[2], "Even the missing parts are her area");
        assert!(parse_google_response("[null]").is_err());
        assert!(parse_google_response("<html>").is_err());
    }

    #[test]
    fn dedupe_and_realign() {
        let lines = ["Hello", "", "World", " Hello ", "same"];
        let (unique, map) = dedupe(&lines);
        assert_eq!(unique, ["Hello", "World", "same"]);
        let out = realign(
            &unique,
            &map,
            &["Hallo".into(), "Welt".into(), "SAME".into()],
        );
        assert_eq!(
            out,
            [
                Some("Hallo".into()),
                None,
                Some("Welt".into()),
                Some("Hallo".into()),
                None
            ]
        );
    }

    #[test]
    fn batching() {
        let texts = ["aaaa"; 7];
        assert_eq!(batches(&texts, 3, 10_000, 1), [0..3, 3..6, 6..7]);
        assert_eq!(batches(&texts, 100, 9, 1), [0..2, 2..4, 4..6, 6..7]);
        assert!(batches(&[], 3, 10, 1).is_empty());
        assert!(validate_language_code("zh-CN").is_ok());
        assert!(validate_language_code("en&x=1").is_err());
    }

    #[test]
    fn ai_request_body() {
        let t = AiTranslator::new("https://api.openai.com/v1", "k", "gpt-4o")
            .with_custom_prompt("Keep it casual.");
        let body = t.request_body("JAPANESE", &["Hello", "World"]).unwrap();
        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["max_tokens"], 8192);
        assert_eq!(body["temperature"], 0.15);
        let system = body["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains("into Japanese"));
        assert!(system.contains("exactly 2 items"));
        assert!(system.ends_with("\n\nKeep it casual."));
        assert_eq!(body["messages"][1]["content"], r#"["Hello","World"]"#);

        let reasoning = AiTranslator::new("x", "k", "openai/gpt-5-mini");
        let body = reasoning.request_body("en", &["a"]).unwrap();
        assert_eq!(body["max_completion_tokens"], 8192);
        assert!(body.get("temperature").is_none());
        assert!(is_reasoning_model("o3-mini") && !is_reasoning_model("llama-3"));

        let mut custom = AiTranslator::new("x", "k", "m");
        custom.request_parameters =
            r#"{"max_completion_tokens":100,"top_p":0.5,"temperature":null}"#.into();
        let body = custom.request_body("en", &["a"]).unwrap();
        assert_eq!(body["max_completion_tokens"], 100);
        assert!(body.get("max_tokens").is_none() && body.get("temperature").is_none());
        assert_eq!(body["top_p"], 0.5);
        for bad in [
            r#"{"model":"x"}"#,
            r#"{"temperature":3}"#,
            "[1]",
            r#"{"max_tokens":-1}"#,
        ] {
            custom.request_parameters = bad.into();
            assert!(custom.request_body("en", &["a"]).is_err(), "{bad}");
        }
    }

    #[test]
    fn endpoints() {
        assert_eq!(
            chat_completion_endpoint("https://openrouter.ai/api/v1").unwrap(),
            "https://openrouter.ai/api/v1/chat/completions"
        );
        assert_eq!(
            chat_completion_endpoint("http://localhost:11434/").unwrap(),
            "http://localhost:11434/v1/chat/completions"
        );
        assert_eq!(
            chat_completion_endpoint("https://x.test/custom/path?key=1").unwrap(),
            "https://x.test/custom/path?key=1"
        );
        assert!(chat_completion_endpoint("ftp://x").is_err());
        assert!(chat_completion_endpoint("https://u:p@x.test").is_err());
        assert!(chat_completion_endpoint("https://x.test/#frag").is_err());
    }

    #[test]
    fn ai_responses() {
        let ok = json!({ "choices": [{ "finish_reason": "stop", "message": {
            "content": "Sure!\n```json\n[\"Hola\", \"Mundo\"]\n```" } }] });
        let content = parse_completion(&ok.to_string()).unwrap();
        assert_eq!(parse_translations(&content, 2).unwrap(), ["Hola", "Mundo"]);
        assert!(parse_translations(&content, 3).is_err());
        assert!(parse_translations("[1, 2]", 2).is_err());
        assert!(parse_translations("no array", 1).is_err());

        let parts = json!({ "choices": [{ "message": { "content": [
            { "type": "text", "text": "[\"a\"" }, { "type": "text", "text": "]" } ] } }] });
        assert_eq!(parse_completion(&parts.to_string()).unwrap(), "[\"a\"]");

        let length =
            json!({ "choices": [{ "finish_reason": "length", "message": { "content": "[" } }] });
        assert!(parse_completion(&length.to_string()).is_err());
        let refusal = json!({ "choices": [{ "message": { "content": null, "refusal": "no" } }] });
        assert!(
            parse_completion(&refusal.to_string())
                .unwrap_err()
                .to_string()
                .contains("refused")
        );
        assert!(
            api_error(401, r#"{"error":{"message":"Invalid key"}}"#)
                .to_string()
                .contains("Invalid key")
        );
    }
}
