// Catalog reading ported from ArchiveTune
// modules/src/main/kotlin/moe/rukamori/archivetune/modules/ModuleCatalogReader.kt
// — © Rukamori and ArchiveTune contributors, GPL-3.0
//! Module catalogs (MODULES.md "URLs, catalogs, and updates").
//!
//! A catalog is a JSON document with `modules` (and/or `category:*` /
//! `extensions`) entries and optional `external_sources`. Relative links
//! resolve against the document's final URL. At most 4 levels deep, 32
//! documents and 1,000 entries; repeated URLs are visited once. An HTML page
//! is searched for a linked `index.json` / `catalog.json` / `source.json`.
//! Refreshing only refreshes entries: installing stays an explicit action.

use std::collections::HashSet;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::error::{Failure, ModuleError, ModuleResult};
use super::http::{resolve_url, source, web_url};
use super::json::{is_sha256_hex, opt_str, strings};

/// One module listed in a catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
    /// Package (or descriptor) URL.
    pub download: Option<String>,
    /// An installable module package (type `MODULE` with a download link).
    pub supported: bool,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Expected SHA-256 of the downloaded package, when the catalog declares one.
    #[serde(default)]
    pub sha256: String,
}

/// A subscribed catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedCatalog {
    pub id: String,
    pub name: String,
    pub url: String,
    /// Unix milliseconds.
    pub refreshed_at: u64,
    pub entries: Vec<CatalogEntry>,
}

pub(crate) struct CatalogDocument {
    pub origin: String,
    pub name: String,
    pub entries: Vec<CatalogEntry>,
}

/// Whether a parsed JSON object looks like a catalog.
pub(crate) fn is_catalog(root: &serde_json::Map<String, Value>) -> bool {
    root.keys().any(|k| {
        k.starts_with("category:") || k == "modules" || k == "external_sources" || k == "extensions"
    })
}

struct Reader {
    entries: Vec<(String, CatalogEntry)>,
    visited: HashSet<String>,
    documents: usize,
    origin: String,
    name: String,
    deadline: Instant,
}

/// Reads the catalog at `address` (or `initial_text` already fetched from it).
pub(crate) fn read(address: &str, initial_text: Option<&str>) -> ModuleResult<CatalogDocument> {
    let mut reader = Reader {
        entries: Vec::new(),
        visited: HashSet::new(),
        documents: 0,
        origin: address.trim().to_owned(),
        name: String::new(),
        deadline: Instant::now() + Duration::from_secs(30),
    };
    reader.load(address.trim(), 0, None, initial_text)?;
    let name = if reader.name.trim().is_empty() {
        url::Url::parse(&reader.origin)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_else(|| "Catalog".into())
    } else {
        reader.name
    };
    Ok(CatalogDocument {
        origin: reader.origin,
        name,
        entries: reader.entries.into_iter().map(|(_, e)| e).collect(),
    })
}

impl Reader {
    fn load(
        &mut self,
        url: &str,
        depth: usize,
        base_override: Option<String>,
        supplied: Option<&str>,
    ) -> ModuleResult<()> {
        if depth > 4 || Instant::now() > self.deadline {
            return Err(ModuleError::invalid());
        }
        let normalized = if url.trim().is_empty() && supplied.is_some() {
            "file".to_owned()
        } else {
            web_url(url)?
        };
        if !self.visited.insert(normalized.clone()) {
            return Ok(());
        }
        self.documents += 1;
        if self.documents > 32 {
            return Err(ModuleError::invalid());
        }
        let (mut text, mut final_url) = match supplied {
            Some(text) => (text.to_owned(), url.to_owned()),
            None => {
                let (bytes, final_url) = source(&normalized)?;
                (String::from_utf8_lossy(&bytes).into_owned(), final_url)
            }
        };
        if !final_url.is_empty()
            && final_url != normalized
            && !self.visited.insert(final_url.clone())
        {
            return Ok(());
        }
        text = text.trim_start_matches('\u{feff}').trim().to_owned();
        if text.starts_with('<') {
            let index = discover_index(&text, &final_url)?;
            if !self.visited.insert(index.clone()) {
                return Err(ModuleError::invalid());
            }
            self.documents += 1;
            if self.documents > 32 {
                return Err(ModuleError::invalid());
            }
            let (bytes, url) = source(&index)?;
            text = String::from_utf8_lossy(&bytes)
                .trim_start_matches('\u{feff}')
                .trim()
                .to_owned();
            if url != index && !self.visited.insert(url.clone()) {
                return Err(ModuleError::invalid());
            }
            final_url = url;
        }
        let root: serde_json::Map<String, Value> =
            serde_json::from_str(&text).map_err(|_| ModuleError::invalid())?;
        if !is_catalog(&root) {
            return Err(ModuleError::invalid());
        }
        if depth == 0 {
            self.origin = final_url.clone();
            self.name = match opt_str(&Value::Object(root.clone()), "name") {
                "" => opt_str(&Value::Object(root.clone()), "title").to_owned(),
                name => name.to_owned(),
            };
        }
        let base = match base_override.filter(|b| !b.trim().is_empty()) {
            Some(b) => web_url(&resolve_url(
                &format!("{}/", b.trim_end_matches('/')),
                &final_url,
            )?)?,
            None => final_url.clone(),
        };
        for (key, value) in &root {
            if (key.starts_with("category:") || key == "modules" || key == "extensions")
                && let Some(items) = value.as_array()
            {
                let provider = key != "category:artworks" && key != "category:testing";
                self.append(items, provider, &base)?;
            }
        }
        let external = root
            .get("external_sources")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if external.len() > 32 {
            return Err(ModuleError::invalid());
        }
        for source in &external {
            let Some(source) = source.as_object() else {
                continue;
            };
            let source_base = match source.get("baseUrl").and_then(Value::as_str) {
                Some(b) if !b.trim().is_empty() => Some(web_url(&resolve_url(
                    &format!("{}/", b.trim_end_matches('/')),
                    &final_url,
                )?)?),
                _ => None,
            };
            if let Some(inline) = source.get("modules").and_then(Value::as_array) {
                let base = source_base.clone().unwrap_or_else(|| base.clone());
                self.append(inline, true, &base)?;
            } else if let Some(url) = source
                .get("url")
                .and_then(Value::as_str)
                .filter(|u| !u.trim().is_empty())
            {
                let next = resolve_url(url, &final_url)?;
                self.load(&next, depth + 1, source_base, None)?;
            }
        }
        Ok(())
    }

    fn append(&mut self, items: &[Value], provider: bool, base: &str) -> ModuleResult<()> {
        if items.len() > 1000 {
            return Err(ModuleError::invalid());
        }
        for item in items {
            if !item.is_object() {
                continue;
            }
            let text = |k: &str| opt_str(item, k).to_owned();
            let direct = [text("download"), text("download_url"), text("url")]
                .into_iter()
                .find(|s| !s.trim().is_empty())
                .unwrap_or_default();
            let reference = if !direct.is_empty() {
                direct
            } else {
                let file = text("file");
                let folder = text("folder").trim_matches('/').to_owned();
                if file.is_empty() || folder.is_empty() {
                    file
                } else {
                    format!("{folder}/{file}")
                }
            };
            let download = if reference.trim().is_empty() {
                None
            } else {
                Some(web_url(&resolve_url(&reference, base)?)?)
            };
            let id = match text("id") {
                id if !id.trim().is_empty() => id,
                _ => download.clone().unwrap_or_default(),
            };
            if id.trim().is_empty() {
                continue;
            }
            let checksum = text("sha256");
            if !checksum.is_empty() && !is_sha256_hex(&checksum) {
                return Err(ModuleError::invalid());
            }
            let name = [text("display_name"), text("name")]
                .into_iter()
                .find(|s| !s.trim().is_empty())
                .unwrap_or_else(|| id.clone());
            let kind = item.get("type").and_then(Value::as_str).unwrap_or("MODULE");
            let mut labels = strings(item.get("labels"));
            for tag in strings(item.get("tags")) {
                if !labels.contains(&tag) {
                    labels.push(tag);
                }
            }
            let entry = CatalogEntry {
                id: id.clone(),
                name,
                version: text("version"),
                author: text("author"),
                description: text("description"),
                supported: provider && kind.eq_ignore_ascii_case("MODULE") && download.is_some(),
                download: download.clone(),
                labels,
                capabilities: strings(item.get("capabilities")),
                sha256: checksum,
            };
            let key = download.unwrap_or_else(|| format!("{base}#{id}"));
            if !self.entries.iter().any(|(k, _)| *k == key) {
                self.entries.push((key, entry));
            }
            if self.entries.len() > 1000 {
                return Err(ModuleError::invalid());
            }
        }
        Ok(())
    }
}

static LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)<(?:link|a)\b([^>]*)>"#).unwrap());
static HREF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)\bhref\s*=\s*["']([^"']+)["']"#).unwrap());
static TYPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)\btype\s*=\s*["']([^"']+)["']"#).unwrap());

/// The JSON index an HTML page links to, else `index.json` next to a directory URL.
fn discover_index(html: &str, origin: &str) -> ModuleResult<String> {
    if origin.is_empty() || origin == "file" {
        return Err(ModuleError::invalid());
    }
    for tag in LINK.captures_iter(html) {
        let attrs = &tag[1];
        let Some(href) = HREF.captures(attrs).map(|c| c[1].to_owned()) else {
            continue;
        };
        let kind = TYPE
            .captures(attrs)
            .map(|c| {
                c[1].split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase()
            })
            .unwrap_or_default();
        let file = href
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if kind == "application/json"
            || ["index.json", "catalog.json", "source.json"].contains(&file.as_str())
        {
            return web_url(&resolve_url(&href, origin)?);
        }
    }
    let path = url::Url::parse(origin)
        .map(|u| u.path().to_owned())
        .unwrap_or_default();
    if !path.is_empty() && !path.ends_with('/') {
        return Err(ModuleError::new(Failure::InvalidDocument));
    }
    web_url(&resolve_url("index.json", origin)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_catalog_entries_resolve_against_the_base() {
        let text = r#"{"name": "Test", "modules": [
            {"id": "a", "name": "A", "version": "1", "download": "./a.arch"},
            {"id": "b", "type": "THEME", "download": "b.arch"},
            {"id": "c"}
        ], "external_sources": [{"baseUrl": "https://cdn.example.org/x", "modules": [{"id": "d", "file": "d.arch"}]}]}"#;
        let doc = read("https://example.org/m/catalog.json", Some(text)).unwrap();
        assert_eq!(doc.name, "Test");
        let ids: Vec<_> = doc.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c", "d"]);
        assert_eq!(
            doc.entries[0].download.as_deref(),
            Some("https://example.org/m/a.arch")
        );
        assert!(doc.entries[0].supported);
        assert!(!doc.entries[1].supported, "not a MODULE");
        assert!(!doc.entries[2].supported, "no download");
        assert_eq!(
            doc.entries[3].download.as_deref(),
            Some("https://cdn.example.org/x/d.arch")
        );
        assert!(read("https://example.org/c.json", Some(r#"{"x": 1}"#)).is_err());
    }

    #[test]
    fn html_pages_link_to_their_index() {
        let html = r#"<html><head><link rel="alternate" type="application/json" href="/feed/catalog.json"></head></html>"#;
        assert_eq!(
            discover_index(html, "https://example.org/page").unwrap(),
            "https://example.org/feed/catalog.json"
        );
        assert_eq!(
            discover_index("<html></html>", "https://example.org/mods/").unwrap(),
            "https://example.org/mods/index.json"
        );
    }
}
