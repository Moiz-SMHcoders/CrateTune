//! Catalog checks (keys, placeholders, plural forms) and a best-effort lint that
//! keeps English UI literals out of the code.

#[path = "../../i18n_parse.rs"]
mod parse;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::*;

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn catalogs() -> Vec<(String, parse::Parsed)> {
    parse::locale_dirs(&crate_dir().join("locales"))
        .unwrap()
        .into_iter()
        .map(|(code, dir)| {
            let parsed = parse::parse_dir(&dir).unwrap_or_else(|e| panic!("{e}"));
            (code, parsed)
        })
        .collect()
}

/// Placeholders a translation may use although English doesn't, or may drop
/// (`date.full`: numeric month instead of the name).
fn optional_placeholders(key: &str) -> &'static [&'static str] {
    match key {
        "date.full" => &["month", "month_num"],
        _ => &[],
    }
}

fn names(template: &str) -> BTreeSet<String> {
    parse::placeholders(template).unwrap().into_iter().collect()
}

#[test]
fn every_locale_matches_english() {
    let all = catalogs();
    let english = &all[0].1;
    let mut errors = Vec::new();
    for (group, forms) in &english.plural {
        if !forms.contains_key(&1) || !forms.contains_key(&5) {
            errors.push(format!("en: {group} needs `.one` and `.other`"));
        }
    }
    for (code, parsed) in &all[1..] {
        let language = code.split('-').next().unwrap();
        let at = |key: &str| {
            parsed
                .origin
                .get(key)
                .cloned()
                .unwrap_or_else(|| code.clone())
        };
        for (key, value) in &parsed.simple {
            let Some(source) = english.simple.get(key) else {
                let kind = if english.plural.contains_key(key) {
                    "a plural group in English"
                } else {
                    "not an English key"
                };
                errors.push(format!("{}: {key} is {kind}", at(key)));
                continue;
            };
            let optional: BTreeSet<String> = optional_placeholders(key)
                .iter()
                .map(|s| s.to_string())
                .collect();
            let (mine, theirs) = (names(value), names(source));
            let extra: Vec<_> = mine
                .difference(&theirs)
                .filter(|n| !optional.contains(*n))
                .collect();
            let missing: Vec<_> = theirs
                .difference(&mine)
                .filter(|n| !optional.contains(*n))
                .collect();
            if !extra.is_empty() || !missing.is_empty() {
                errors.push(format!(
                    "{}: {key}: placeholders differ from English (extra {extra:?}, missing {missing:?})",
                    at(key)
                ));
            }
        }
        for (group, forms) in &parsed.plural {
            let Some(source) = english.plural.get(group) else {
                errors.push(format!("{}: {group} is not an English plural group", code));
                continue;
            };
            let theirs = names(&source[&5]);
            let mut used = BTreeSet::new();
            for (category, value) in forms {
                let mine = names(value);
                if let Some(extra) = mine.difference(&theirs).next() {
                    errors.push(format!(
                        "{code}: {group}.{}: unknown placeholder {{{extra}}}",
                        parse::CATEGORIES[*category]
                    ));
                }
                used.extend(mine);
            }
            for name in theirs.iter().filter(|n| *n != "count") {
                if !used.contains(name) {
                    errors.push(format!("{code}: {group}: no form uses {{{name}}}"));
                }
            }
            if !forms.contains_key(&5) {
                errors.push(format!("{code}: {group} has no `.other` form"));
            }
            // Every category the language's rules give a whole number must exist.
            let needed: BTreeSet<usize> = (0..=1000u64)
                .map(|n| plural::category(language, n) as usize)
                .collect();
            for category in needed {
                if !forms.contains_key(&category) {
                    errors.push(format!(
                        "{code}: {group} lacks the `.{}` form ({language} uses it)",
                        parse::CATEGORIES[category]
                    ));
                }
            }
        }
    }
    // The name comes from `{app}` (packaging/app.env), never typed into a catalog.
    for (code, parsed) in &all {
        let values = parsed
            .simple
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .chain(
                parsed
                    .plural
                    .iter()
                    .flat_map(|(k, forms)| forms.values().map(move |v| (k.clone(), v.clone()))),
            );
        for (key, value) in values {
            for name in [crate::identity::APP_NAME, "ArchiveTune"] {
                if value.contains(name) {
                    errors.push(format!("{code}: {key} spells out {name:?}; use {{app}}"));
                }
            }
        }
    }
    assert!(
        errors.is_empty(),
        "{} catalog problems:\n{}",
        errors.len(),
        errors.join("\n")
    );
}

#[test]
fn every_locale_names_itself_and_embeds() {
    let mut codes = BTreeSet::new();
    for locale in Locale::all() {
        assert!(codes.insert(locale.code()), "duplicate {}", locale.code());
        assert!(!locale.native_name().is_empty());
        assert_eq!(Locale::from_code(locale.code()), Some(locale));
    }
    assert_eq!(Locale::ENGLISH.code(), "en");
    assert_eq!(Locale::ENGLISH.coverage(), 100.);
}

#[test]
fn lookups_fall_back_and_fill() {
    set_locale(Locale::ENGLISH);
    assert_eq!(t!("nav.home"), "Home");
    assert_eq!(text(k!("common.play")), "Play");
    assert_eq!(tp!("common.songs", 1), "1 song");
    assert_eq!(tp!("common.songs", 12345), "12,345 songs");
    assert!(t!("menu.quit").ends_with(crate::identity::APP_NAME));
    assert_eq!(fill("{a} and {b}", &[("a", &1), ("b", &"x")]), "1 and x");
    assert_eq!(fill("{{literal}} {missing}", &[]), "{literal} {missing}");
    assert_eq!(format::iso_date("2026-10-28"), "28 October 2026");
    assert_eq!(format::listening(3 * 3_600_000 + 12 * 60_000), "3 h 12 min");
    assert_eq!(format::bytes(1536), "1.5 KB");
    assert_eq!(format::ago(120), "2 min ago");
    for locale in Locale::all() {
        set_locale(locale);
        // Every locale resolves every key to something (its own text or English).
        for (i, name) in SIMPLE_KEYS.iter().enumerate() {
            assert!(!text(Key(i as u16)).is_empty(), "{}: {name}", locale.code());
        }
        for i in 0..PLURAL_KEYS.len() {
            for n in [0, 1, 2, 5, 11, 21, 101] {
                assert!(!plural_text(PluralKey(i as u16), n).is_empty());
            }
        }
        assert!(!format::iso_date("2026-03-05").is_empty());
    }
    if let Some(de) = Locale::from_code("de") {
        set_locale(de);
        assert_eq!(format::number(1234), "1.234");
    }
    set_locale(Locale::ENGLISH);
}

/// Prints per-locale coverage: `cargo test -p music-gpui coverage -- --nocapture`.
#[test]
fn coverage() {
    for locale in Locale::all() {
        println!(
            "{:6} {:>6.1} %  {}",
            locale.code(),
            locale.coverage(),
            locale.native_name()
        );
    }
}

// ---- English literals in UI code ---------------------------------------------------

/// Files that are not UI (verification scripts, the sign-in page's JavaScript, the
/// translation layer itself).
const SKIP_FILES: &[&str] = &[
    "i18n/",
    "script.rs",
    "integrations/script.rs",
    "library_script.rs",
    "release_script.rs",
    "login_webview.rs",
    "mic_permission.rs",
    "secrets.rs",
    "identity.rs",
];

/// Names that stay as they are in every language (a literal made only of these and
/// punctuation / digits is fine).
const PROPER_NOUNS: &[&str] = &[
    "YouTube",
    "Music",
    "Spotify",
    "Last",
    "fm",
    "ListenBrainz",
    "Discord",
    "Chromecast",
    "Google",
    "Cast",
    "SponsorBlock",
    "Shazam",
    "Apple",
    "LRCLIB",
    "BetterLyrics",
    "Portato",
    "YouLyPlus",
    "Binimum",
    "KuGou",
    "Megalobiz",
    "SimpMusic",
    "Unison",
    "Paxsenix",
    "Musixmatch",
    "OpenAI",
    "OpenRouter",
    "Ollama",
    "LM",
    "Studio",
    "Exportify",
    "GitHub",
    "Cloudflare",
    "Quad9",
    "Hummingbird",
    "Lucide",
    "IPADIC",
    "Weblate",
    "QuickJS",
    "JSON",
    "CSV",
    "HTTP",
    "HTTPS",
    "SOCKS4",
    "SOCKS5",
    "SOCKS5h",
    "DNS",
    "URL",
    "API",
    "ID",
    "AAC",
    "Opus",
    "FLAC",
    "MP3",
    "PNG",
    "Ok",
    "Err",
    "Some",
    "None",
    "Self",
    "Linux",
    "Windows",
    "macOS",
    "AppImage",
    "WebView2",
    "WebKit",
    "WKWebView",
    "PoToken",
    "TV",
    "Web",
    "GPL",
    "Rukamori",
];

fn looks_like_ui_text(literal: &str) -> bool {
    if literal.contains("://") || literal.starts_with('@') || literal.starts_with("MUSIC_") {
        return false;
    }
    let words: Vec<&str> = literal
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|w| w.len() >= 2)
        .collect();
    let capitalized = words.iter().any(|w| {
        let mut chars = w.chars();
        chars.next().is_some_and(|c| c.is_ascii_uppercase())
            && chars.clone().count() >= 1
            && chars.all(|c| c.is_ascii_lowercase())
            && !PROPER_NOUNS.contains(w)
    });
    // Two lowercase words separated by a space ("couldn't load", "end of track").
    let sentence = literal
        .split(' ')
        .filter(|w| {
            w.len() >= 2
                && w.chars()
                    .all(|c| c.is_ascii_lowercase() || c == '’' || c == '\'')
        })
        .count()
        >= 2;
    capitalized || sentence
}

/// String literals on one line of code (`"…"`, escapes skipped; raw strings and
/// byte strings are ignored).
fn literals(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => break,
            b'\'' => {
                // Skip a char literal ('"', '\'' …) or a lifetime.
                if bytes.get(i + 2) == Some(&b'\'') {
                    i += 3;
                } else if bytes.get(i + 1) == Some(&b'\\') && bytes.get(i + 3) == Some(&b'\'') {
                    i += 4;
                } else {
                    i += 1;
                }
            }
            b'"' => {
                let raw_or_byte = i > 0 && matches!(bytes[i - 1], b'r' | b'b' | b'#');
                let mut j = i + 1;
                let mut value = String::new();
                while j < bytes.len() && bytes[j] != b'"' {
                    if bytes[j] == b'\\' {
                        j += 1;
                    }
                    j += 1;
                }
                if let Some(s) = line.get(i + 1..j.min(line.len())) {
                    value.push_str(s);
                }
                if !raw_or_byte {
                    out.push(value);
                }
                i = j + 1;
            }
            _ => i += 1,
        }
    }
    out
}

/// Calls whose string arguments are not UI text.
const NOT_UI: &[&str] = &[
    "eprintln!(",
    "println!(",
    "panic!(",
    "unreachable!(",
    "assert!(",
    "assert_eq!(",
    "debug_assert",
    ".expect(",
    "env::var",
    "env!(",
    "include_str!(",
    "include_bytes!(",
    "concat!(",
    "#[",
    "starts_with(",
    "ends_with(",
    "strip_prefix(",
    "strip_suffix(",
    "contains(",
    "split(",
    "find(",
    "Command::new(",
    ".arg(",
    "msg_send",
    "sel!(",
    "KeyBinding::new(",
    "trace(",
    "trace!(",
    "log(",
    "context(",
    "map_err(",
    "Err(format!(",
    "bail(",
    "replace(",
    "query_pairs",
    "header(",
    "set_header",
    ".get(\"",
    ".insert(\"",
];

/// `path: literal` lines in `src/i18n/lint_allowlist*.txt` (one file per area).
fn allowlist() -> BTreeMap<String, BTreeSet<String>> {
    let mut map: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let dir = crate_dir().join("src/i18n");
    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !(name.starts_with("lint_allowlist") && name.ends_with(".txt")) {
            continue;
        }
        let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((path, literal)) = line.split_once(": ") {
                map.entry(path.to_owned())
                    .or_default()
                    .insert(literal.to_owned());
            }
        }
    }
    map
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// English UI text that bypasses the catalogs: string literals that read like words
/// in non-test code, outside logging / parsing calls, `t!` keys and the allowlist.
/// A line ending in `// i18n-ignore` is skipped (developer-only text).
pub fn untranslated_literals() -> Vec<String> {
    let src = crate_dir().join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    files.sort();
    let allow = allowlist();
    let mut hits = Vec::new();
    for path in files {
        let rel = path
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if SKIP_FILES
            .iter()
            .any(|s| rel == *s || (s.ends_with('/') && rel.starts_with(s)))
        {
            continue;
        }
        let allowed = allow.get(&rel);
        let text = std::fs::read_to_string(&path).unwrap();
        let mut in_not_ui = false;
        let mut depth = 0i32;
        let mut in_block_comment = false;
        let lines: Vec<&str> = text.lines().collect();
        for (n, line) in lines.iter().copied().enumerate() {
            let trimmed = line.trim();
            // The test module at the end of a file.
            if trimmed.starts_with("#[cfg(test)]")
                && lines
                    .get(n + 1)
                    .is_some_and(|l| l.trim_start().starts_with("mod "))
            {
                break;
            }
            if in_block_comment {
                in_block_comment = !trimmed.contains("*/");
                continue;
            }
            if trimmed.starts_with("/*") {
                in_block_comment = !trimmed.contains("*/");
                continue;
            }
            if trimmed.starts_with("//") || trimmed.ends_with("// i18n-ignore") {
                continue;
            }
            // A not-UI call may span lines: skip until its parentheses close.
            if !in_not_ui && NOT_UI.iter().any(|m| line.contains(m)) {
                in_not_ui = true;
                depth = 0;
            }
            let found = literals(line);
            if in_not_ui {
                for c in line.chars() {
                    match c {
                        '(' | '[' | '{' => depth += 1,
                        ')' | ']' | '}' => depth -= 1,
                        _ => {}
                    }
                }
                if depth <= 0 {
                    in_not_ui = false;
                }
                continue;
            }
            for literal in found {
                // `t!("key")`, `tp!(…)`, `k!(…)`: the literal is a key.
                if line.contains(&format!("t!(\"{literal}\""))
                    || line.contains(&format!("k!(\"{literal}\""))
                    || line.contains(&format!("tp!(\"{literal}\""))
                {
                    continue;
                }
                if !looks_like_ui_text(&literal) {
                    continue;
                }
                if allowed.is_some_and(|a| a.contains(&literal)) {
                    continue;
                }
                hits.push(format!("{rel}:{}: {literal:?}", n + 1));
            }
        }
    }
    hits
}

#[test]
fn english_literals_stay_out_of_ui_code() {
    let hits = untranslated_literals();
    assert!(
        hits.is_empty(),
        "{} English literal(s) outside the catalogs (use t!/tp!, or add `path: literal` to \
         src/i18n/lint_allowlist_<area>.txt / end the line with `// i18n-ignore` for developer-only \
         text):\n{}",
        hits.len(),
        hits.join("\n")
    );
}

#[test]
fn lint_heuristics() {
    assert!(looks_like_ui_text("Play"));
    assert!(looks_like_ui_text("Couldn't load"));
    assert!(looks_like_ui_text("end of track"));
    assert!(!looks_like_ui_text("nav-settings"));
    assert!(!looks_like_ui_text("cmd-right"));
    assert!(!looks_like_ui_text("settings.json"));
    assert!(!looks_like_ui_text("YouTube Music"));
    assert!(!looks_like_ui_text("{} · {}"));
    assert!(!looks_like_ui_text("UC"));
    assert_eq!(
        literals(r#"a("Play", 'x', "b\"c") // "no""#),
        vec!["Play", "b\\\"c"]
    );
}
