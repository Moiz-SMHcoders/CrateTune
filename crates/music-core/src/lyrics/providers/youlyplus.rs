// Ported from ArchiveTune lyrics/youlyplus/src/main/kotlin/moe/rukamori/archivetune/youlyplus/YouLyPlus.kt — © Rukamori and ArchiveTune contributors, GPL-3.0
//! YouLyPlus / lyricsplus mirrors: `v1/ttml/get` (TTML), then `v2/lyrics/get`
//! (JSON lines/syllables, converted to enhanced LRC).

use std::fmt::Write as _;

use serde_json::Value;

use super::{LyricsProvider, http_get, require_title_artist};
use crate::lyrics::{LyricsQuery, LyricsResult};
use crate::{Error, Result};

const TTML_PATH: &str = "v1/ttml/get";
const LYRICS_PATH: &str = "v2/lyrics/get";
const MIRRORS: [&str; 5] = [
    "https://lyricsplus.binimum.org/",
    "https://lyricsplus.prjktla.my.id/",
    "https://lyricsplus.prjktla.workers.dev/",
    "https://lyricsplus.atomix.one/",
    "https://lyricsplus-seven.vercel.app/",
];

#[derive(Debug, Clone)]
pub struct YouLyPlus {
    /// Base URLs tried in order (each ends with `/`).
    pub mirrors: Vec<String>,
}

impl Default for YouLyPlus {
    fn default() -> Self {
        Self {
            mirrors: MIRRORS.iter().map(|m| (*m).to_owned()).collect(),
        }
    }
}

impl LyricsProvider for YouLyPlus {
    fn name(&self) -> &str {
        "YouLyPlus"
    }

    fn fetch(&self, query: &LyricsQuery) -> Result<Option<LyricsResult>> {
        if require_title_artist(query).is_none() {
            return Ok(None);
        }
        let mut last_err = None;
        for (path, decode) in [
            (TTML_PATH, decode_ttml as fn(&str) -> Option<String>),
            (LYRICS_PATH, decode_lines),
        ] {
            match self.query_mirrors(path, query, decode) {
                Ok(Some(raw)) => {
                    if let Some(r) = LyricsResult::parse(self.name(), raw, query.duration_secs) {
                        return Ok(Some(r));
                    }
                }
                Ok(None) => {}
                Err(e) => last_err = Some(e),
            }
        }
        last_err.map_or(Ok(None), Err)
    }
}

impl YouLyPlus {
    /// First mirror that answers with usable lyrics; `Err` only when every mirror failed at the network level.
    fn query_mirrors(
        &self,
        path: &str,
        query: &LyricsQuery,
        decode: fn(&str) -> Option<String>,
    ) -> Result<Option<String>> {
        let duration = query
            .duration_secs
            .filter(|&d| d > 0)
            .map(|d| d.to_string());
        let mut params = vec![
            ("title", query.title.trim()),
            ("artist", query.artist.trim()),
        ];
        if let Some(album) = query
            .album
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
        {
            params.push(("album", album));
        }
        if let Some(d) = duration.as_deref() {
            params.push(("duration", d));
        }
        let headers = [
            ("Accept", "application/json"),
            ("User-Agent", "ArchiveTune"),
        ];
        let mut errors = 0;
        for base in &self.mirrors {
            match http_get(&format!("{base}{path}"), &params, &headers, query.timeout) {
                Ok(resp) if resp.ok() => {
                    if let Some(raw) = decode(&resp.body) {
                        return Ok(Some(raw));
                    }
                }
                // A working mirror's "no lyrics": the others (mostly down, ~0.5–3 s
                // each) would only say the same.
                Ok(resp) if resp.status == 404 => return Ok(None),
                Ok(resp) => log::debug!("YouLyPlus {base}{path}: HTTP {}", resp.status),
                Err(e) => {
                    log::debug!("YouLyPlus {base}{path}: {e}");
                    errors += 1;
                }
            }
        }
        if errors == self.mirrors.len() && errors > 0 {
            return Err(Error::Parse("YouLyPlus: every mirror failed".into()));
        }
        Ok(None)
    }
}

pub(crate) fn decode_ttml(body: &str) -> Option<String> {
    let trimmed = body.trim();
    let ttml = if trimmed.starts_with('<') {
        trimmed.to_owned()
    } else {
        let v: Value = serde_json::from_str(body).ok()?;
        v.get("ttml")?.as_str()?.trim().to_owned()
    };
    (ttml.starts_with('<') && !ttml.is_empty()).then_some(ttml)
}

/// `v2/lyrics/get` JSON → (enhanced) LRC.
pub(crate) fn decode_lines(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    let lines = v.get("lyrics")?.as_array()?;
    if lines.is_empty() {
        return None;
    }
    let word_type = v
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| t.eq_ignore_ascii_case("word"));
    let timed: Vec<&Value> = lines
        .iter()
        .filter(|l| l.get("time").is_some_and(Value::is_number))
        .collect();
    if timed.is_empty() {
        let text = lines
            .iter()
            .filter_map(|l| l.get("text")?.as_str())
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        return (!text.is_empty()).then_some(text);
    }
    let mut out = String::new();
    for line in timed {
        let time = line.get("time").and_then(Value::as_i64).unwrap_or(0);
        out.push_str(&lrc_stamp(time, true));
        let syllables: Vec<&Value> = line
            .get("syllabus")
            .and_then(Value::as_array)
            .map(|s| {
                s.iter()
                    .filter(|s| {
                        s.get("time").is_some_and(Value::is_number)
                            && s.get("text")
                                .and_then(Value::as_str)
                                .is_some_and(|t| !t.trim().is_empty())
                            && !s
                                .get("isBackground")
                                .and_then(Value::as_bool)
                                .unwrap_or(false)
                    })
                    .collect()
            })
            .unwrap_or_default();
        if word_type && !syllables.is_empty() {
            for s in &syllables {
                out.push_str(&lrc_stamp(s["time"].as_i64().unwrap_or(0), false));
                out.push_str(s["text"].as_str().unwrap_or_default());
            }
            // Close the last syllable.
            if let Some(last) = syllables.last() {
                let end = last["time"].as_i64().unwrap_or(0)
                    + last.get("duration").and_then(Value::as_i64).unwrap_or(0);
                out.push_str(&lrc_stamp(end, false));
            }
        } else {
            out.push_str(line.get("text").and_then(Value::as_str).unwrap_or_default());
        }
        out.push('\n');
    }
    (!out.trim().is_empty()).then_some(out)
}

fn lrc_stamp(ms: i64, line: bool) -> String {
    let ms = ms.max(0);
    let mut s = String::new();
    let (open, close) = if line { ('[', ']') } else { ('<', '>') };
    let _ = write!(
        s,
        "{open}{:02}:{:02}.{:03}{close}",
        ms / 60_000,
        (ms % 60_000) / 1000,
        ms % 1000
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::SyncKind;
    use crate::lyrics::providers::fixture;

    #[test]
    fn ttml_response() {
        let raw = decode_ttml(&fixture("youlyplus_idol_ttml.json")).unwrap();
        let r = LyricsResult::parse("YouLyPlus", raw, Some(213)).unwrap();
        assert_eq!(r.sync_kind(), SyncKind::Word);
        let l = r.synced.unwrap();
        assert_eq!(l.lines[0].text, "無敵の笑顔で荒らすメディア");
        assert_eq!(l.lines[0].words.len(), 12);
    }

    #[test]
    fn mirrors_fail_over_but_stop_at_not_found() {
        use crate::testutil::{Reply, TestServer};
        let server = TestServer::start(|path, _, _| {
            if path.starts_with("/down/") {
                Reply::Status(502)
            } else if path.starts_with("/v1/ttml/get") || path.starts_with("/lyrics/v1/ttml") {
                Reply::Status(404)
            } else {
                Reply::Status(500)
            }
        });
        let p = YouLyPlus {
            mirrors: vec![
                format!("{}/down/", server.base),
                format!("{}/", server.base),
                format!("{}/never/", server.base),
            ],
        };
        let q = LyricsQuery::new("Double Talk", "Jaakko Eino Kalevi");
        assert!(
            p.query_mirrors(TTML_PATH, &q, decode_ttml)
                .unwrap()
                .is_none()
        );
        let paths: Vec<String> = server
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.0.clone())
            .collect();
        assert_eq!(paths.len(), 2, "{paths:?}");
        assert!(paths.iter().all(|p| !p.starts_with("/never/")));
    }

    #[test]
    fn v2_lines() {
        let lrc = decode_lines(&fixture("youlyplus_v2_instant_crush.json")).unwrap();
        assert!(lrc.starts_with("[00:26.491]I didn't want"), "{lrc}");
        let r = LyricsResult::parse("YouLyPlus", lrc, None).unwrap();
        assert_eq!(r.sync_kind(), SyncKind::Line);

        let word = serde_json::json!({
            "type": "Word",
            "lyrics": [{ "time": 1000, "duration": 1000, "text": "Hi there", "syllabus": [
                { "time": 1000, "duration": 400, "text": "Hi " },
                { "time": 1400, "duration": 600, "text": "there" },
                { "time": 1500, "duration": 300, "text": "(bg)", "isBackground": true }
            ]}]
        });
        let lrc = decode_lines(&word.to_string()).unwrap();
        assert_eq!(
            lrc,
            "[00:01.000]<00:01.000>Hi <00:01.400>there<00:02.000>\n"
        );
        let r = LyricsResult::parse("YouLyPlus", lrc, None).unwrap();
        assert_eq!(r.sync_kind(), SyncKind::Word);
    }
}
