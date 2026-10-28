//! Parser for the translation catalogs in `locales/<locale>/*.lang`, shared by
//! `build.rs` (which embeds them) and the catalog tests in `src/i18n`.
//!
//! Format, one entry per line:
//!
//! ```text
//! # comment
//! nav.home = Home
//! library.songs.one = {count} song
//! library.songs.other = {count} songs
//! ```
//!
//! * Keys are `[A-Za-z0-9_.-]+`. A key whose last segment is a CLDR plural category
//!   (`zero`, `one`, `two`, `few`, `many`, `other`) belongs to the plural group named
//!   by the rest of the key; English defines `one` + `other`, other locales the
//!   categories their rules use (missing ones fall back to `other`).
//! * Values are trimmed; `\n` is a line break, `\t` a tab, `\s` a space (for leading /
//!   trailing spaces), `\\` a backslash. An empty value means "not translated".
//! * `{name}` is a placeholder filled at runtime (`{{` / `}}` are literal braces);
//!   `{app}` is replaced with the app's name when the catalog is embedded.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::Path;

pub const CATEGORIES: [&str; 6] = ["zero", "one", "two", "few", "many", "other"];

/// One locale's catalog as written in its files.
#[derive(Debug, Default, Clone)]
pub struct Parsed {
    /// Simple key → value.
    pub simple: BTreeMap<String, String>,
    /// Plural group → (category index in [`CATEGORIES`] → value).
    pub plural: BTreeMap<String, BTreeMap<usize, String>>,
    /// Where each key was defined (`file:line`), for error messages.
    pub origin: BTreeMap<String, String>,
}

/// Splits `key` into (group, category) if its last segment is a plural category.
pub fn plural_split(key: &str) -> Option<(&str, usize)> {
    let (group, last) = key.rsplit_once('.')?;
    let category = CATEGORIES.iter().position(|c| *c == last)?;
    Some((group, category))
}

pub fn unescape(raw: &str) -> Result<String, String> {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('s') => out.push(' '),
            Some('\\') => out.push('\\'),
            other => return Err(format!("unknown escape \\{}", other.unwrap_or(' '))),
        }
    }
    Ok(out)
}

/// Placeholder names in a template (`{name}`), in order, without duplicates.
pub fn placeholders(template: &str) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    let mut rest = template;
    while let Some(i) = rest.find(['{', '}']) {
        let c = rest.as_bytes()[i];
        let after = &rest[i + 1..];
        if after.as_bytes().first() == Some(&c) {
            rest = &after[1..];
            continue;
        }
        if c == b'}' {
            return Err(format!("unmatched '}}' in {template:?}"));
        }
        let Some(end) = after.find('}') else {
            return Err(format!("unclosed '{{' in {template:?}"));
        };
        let name = &after[..end];
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!("bad placeholder {{{name}}} in {template:?}"));
        }
        if !names.iter().any(|n| n == name) {
            names.push(name.to_owned());
        }
        rest = &after[end + 1..];
    }
    Ok(names)
}

/// Parses one file's text into `into`.
pub fn parse_into(into: &mut Parsed, file: &str, text: &str) -> Result<(), String> {
    for (n, line) in text.lines().enumerate() {
        let at = format!("{file}:{}", n + 1);
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("{at}: expected `key = value`"));
        };
        let key = key.trim();
        if key.is_empty()
            || !key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
        {
            return Err(format!("{at}: bad key {key:?}"));
        }
        let value = unescape(value.trim()).map_err(|e| format!("{at}: {e}"))?;
        placeholders(&value).map_err(|e| format!("{at}: {e}"))?;
        if let Some(prev) = into.origin.get(key) {
            return Err(format!("{at}: {key} is already defined at {prev}"));
        }
        into.origin.insert(key.to_owned(), at.clone());
        if value.is_empty() {
            continue;
        }
        match plural_split(key) {
            Some((group, category)) => {
                into.plural
                    .entry(group.to_owned())
                    .or_default()
                    .insert(category, value);
            }
            None => {
                into.simple.insert(key.to_owned(), value);
            }
        }
    }
    Ok(())
}

/// Reads every `*.lang` file of a locale directory (sorted by name).
pub fn parse_dir(dir: &Path) -> Result<Parsed, String> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "lang"))
        .collect();
    files.sort();
    let mut parsed = Parsed::default();
    for path in files {
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let name = format!(
            "{}/{}",
            dir.file_name().unwrap_or_default().to_string_lossy(),
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        parse_into(&mut parsed, &name, &text)?;
    }
    for group in parsed.plural.keys() {
        if parsed.simple.contains_key(group) {
            return Err(format!("{group} is both a plain key and a plural group"));
        }
    }
    Ok(parsed)
}

/// Locale directories under `root` (`en` first, the rest sorted by code).
pub fn locale_dirs(root: &Path) -> Result<Vec<(String, std::path::PathBuf)>, String> {
    let mut dirs: Vec<_> = std::fs::read_dir(root)
        .map_err(|e| format!("{}: {e}", root.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), p))
        .collect();
    dirs.sort_by(|a, b| (a.0 != "en", &a.0).cmp(&(b.0 != "en", &b.0)));
    if dirs.first().map(|d| d.0.as_str()) != Some("en") {
        return Err("locales/en is missing".into());
    }
    Ok(dirs)
}
