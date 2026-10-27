// `.arch` package validation ported from ArchiveTune
// modules/src/main/kotlin/moe/rukamori/archivetune/modules/ModulePackageReader.kt
// — © Rukamori and ArchiveTune contributors, GPL-3.0
//! Reading (and, for authors and tests, writing) `.arch` packages.
//!
//! An `.arch` file is a ZIP with exactly `manifest.json`, `provider.js` and an
//! optional `icon.png` at its root (MODULES.md "Package format"). Entries are
//! read into bounded memory; nothing is extracted to disk. The manifest is
//! checked completely before any JavaScript runs.

use std::collections::HashMap;
use std::io::{Cursor, Read, Write};
use std::path::Path;

use serde_json::{Map, Value};

use super::error::{Failure, ModuleError, ModuleResult, Stage};
use super::http::MAX_SOURCE_BYTES;
use super::json::{depth, equivalent, is_sha256_hex, sha256_hex};
use super::manifest::{KNOWN_CAPABILITIES, ModuleManifest, SettingKind, check_png};

const MANIFEST_LIMIT: u64 = 128 * 1024;
const SCRIPT_LIMIT: u64 = 3 * 1024 * 1024;
const ICON_LIMIT: u64 = 256 * 1024;
/// The `format` value of a module manifest (MODULES.md).
pub const FORMAT: &str = "archivetune.module";

/// A validated package.
#[derive(Debug, Clone)]
pub struct Package {
    pub manifest: ModuleManifest,
    /// `provider.js` (UTF-8, BOM removed).
    pub script: String,
    /// SHA-256 of the package bytes.
    pub sha256: String,
}

/// ZIP local-file or empty-archive signature.
pub fn is_package(bytes: &[u8]) -> bool {
    bytes.len() >= 4
        && bytes[0] == b'P'
        && bytes[1] == b'K'
        && matches!((bytes[2], bytes[3]), (3, 4) | (5, 6))
}

fn invalid(code: &str) -> ModuleError {
    ModuleError::arch(code)
}

fn utf8(bytes: Vec<u8>) -> ModuleResult<String> {
    let text = String::from_utf8(bytes).map_err(|_| invalid("UTF8"))?;
    Ok(text
        .strip_prefix('\u{feff}')
        .map(str::to_owned)
        .unwrap_or(text))
}

/// Parses a manifest document: strict JSON object, nesting at most 32 levels.
pub(crate) fn manifest_document(bytes: Vec<u8>) -> ModuleResult<Map<String, Value>> {
    let text = utf8(bytes)?;
    let manifest_err =
        || ModuleError::at(Failure::InvalidDocument, Stage::Manifest, "ARCH_MANIFEST");
    let value: Value = serde_json::from_str(&text).map_err(|e| {
        if e.to_string().contains("recursion limit") {
            invalid("MANIFEST_DEPTH")
        } else {
            manifest_err()
        }
    })?;
    if depth(&value) > 32 {
        return Err(invalid("MANIFEST_DEPTH"));
    }
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(invalid("MANIFEST")),
    }
}

/// Reads and validates a package. `app_version` is checked against `minAppVersion`.
pub fn read(bytes: &[u8], app_version: &str) -> ModuleResult<Package> {
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(invalid("SIZE"));
    }
    let zip_err = || ModuleError::at(Failure::InvalidDocument, Stage::Decode, "ARCH_ZIP");
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| zip_err())?;
    let mut files: HashMap<String, Vec<u8>> = HashMap::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|e| match e {
            zip::result::ZipError::UnsupportedArchive(_) => invalid("ENTRY"),
            _ => zip_err(),
        })?;
        let name = entry.name().map_err(|_| invalid("ENTRY"))?.into_owned();
        let limit = match name.as_str() {
            "manifest.json" => MANIFEST_LIMIT,
            "provider.js" => SCRIPT_LIMIT,
            "icon.png" => ICON_LIMIT,
            // The extension-compatibility runtime is not implemented.
            "index.js" => {
                return Err(ModuleError::at(
                    Failure::Unsupported,
                    Stage::Decode,
                    "ARCH_UNSUPPORTED_runtime",
                ));
            }
            _ => return Err(invalid("ENTRY")),
        };
        if entry.is_dir()
            || entry.encrypted()
            || files.contains_key(&name)
            || entry.size() > limit
            || !matches!(
                entry.compression(),
                zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated
            )
        {
            return Err(invalid("ENTRY"));
        }
        let mut data = Vec::new();
        (&mut entry)
            .take(limit + 1)
            .read_to_end(&mut data)
            .map_err(|_| zip_err())?;
        if data.len() as u64 > limit {
            return Err(invalid("ENTRY"));
        }
        files.insert(name, data);
    }
    // The central directory listed the same name twice (the reader keeps one).
    if central_directory_entries(bytes).is_some_and(|n| n != files.len())
        || archive.len() != files.len()
    {
        return Err(invalid("ENTRY"));
    }
    let manifest_bytes = files
        .remove("manifest.json")
        .ok_or_else(|| invalid("MANIFEST_MISSING"))?;
    let script_bytes = files
        .remove("provider.js")
        .ok_or_else(|| invalid("SCRIPT_MISSING"))?;
    let icon = files.remove("icon.png");

    let root = manifest_document(manifest_bytes)?;
    if root.get("format").and_then(Value::as_str) != Some(FORMAT)
        || !super::json::is_int(root.get("formatVersion"), 1)
        || !super::json::is_int(root.get("apiVersion"), 1)
    {
        return Err(ModuleError::at(
            Failure::Unsupported,
            Stage::Manifest,
            "ARCH_VERSION",
        ));
    }
    if root.get("entry").and_then(Value::as_str) != Some("provider.js") {
        return Err(invalid("ENTRY"));
    }
    checksum(&script_bytes, root.get("sha256"), "CHECKSUM")?;
    let mut metadata = match root.get("module") {
        Some(Value::Object(m)) => m.clone(),
        _ => {
            return Err(ModuleError::at(
                Failure::InvalidDocument,
                Stage::Manifest,
                "ARCH_MANIFEST",
            ));
        }
    };
    match root.get("runtime").map(|r| r.as_str()) {
        None | Some(Some("archivetune")) => {}
        Some(Some("extension")) => {
            return Err(ModuleError::at(
                Failure::Unsupported,
                Stage::Manifest,
                "ARCH_UNSUPPORTED_runtime",
            ));
        }
        Some(_) => return Err(invalid("RUNTIME")),
    }
    for key in [
        "signedSession",
        "permissions",
        "serviceHealth",
        "searchBehavior",
        "urlHandler",
    ] {
        if metadata.contains_key(key) {
            return Err(ModuleError::at(
                Failure::Unsupported,
                Stage::Manifest,
                format!("ARCH_UNSUPPORTED_{key}"),
            ));
        }
    }
    for key in ["id", "name", "version"] {
        if !metadata.get(key).is_some_and(Value::is_string) {
            return Err(invalid("IDENTITY"));
        }
    }
    for key in [
        "author",
        "description",
        "displayName",
        "homepage",
        "repository",
        "license",
        "minAppVersion",
        "icon",
        "qualityLabel",
    ] {
        if metadata.get(key).is_some_and(|v| !v.is_string()) {
            return Err(invalid("IDENTITY"));
        }
    }
    if metadata.contains_key("iconBase64") {
        return Err(invalid("ICON"));
    }
    if let Some(declared) = metadata.get("icon") {
        let icon = match (declared.as_str(), &icon) {
            (Some("icon.png"), Some(icon)) => icon,
            _ => return Err(invalid("ICON")),
        };
        checksum(icon, root.get("iconSha256"), "ICON_CHECKSUM")?;
        check_png(icon).map_err(|_| invalid("ICON"))?;
        use base64::Engine as _;
        metadata.insert(
            "iconBase64".into(),
            base64::engine::general_purpose::STANDARD
                .encode(icon)
                .into(),
        );
    } else if icon.is_some() || root.contains_key("iconSha256") {
        return Err(invalid("ICON"));
    }
    normalize_settings(&mut metadata)?;
    for key in ["labels", "tags"] {
        match metadata.get(key) {
            None => {}
            Some(Value::Array(items)) if items.iter().all(Value::is_string) => {}
            Some(_) => return Err(invalid("LABELS")),
        }
    }
    let metadata = Value::Object(metadata);
    let manifest = ModuleManifest::parse(&metadata).map_err(|e| {
        if e.code.is_some() {
            e
        } else if e.failure == Failure::Unsupported {
            invalid("CAPABILITIES")
        } else {
            invalid("IDENTITY")
        }
    })?;
    manifest.require_compatible(app_version)?;
    if !valid_id(&manifest.id) || manifest.version.trim().is_empty() {
        return Err(invalid("IDENTITY"));
    }
    let declared = metadata
        .get("capabilities")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("CAPABILITIES"))?;
    let mut unique = manifest.capabilities.clone();
    unique.sort();
    unique.dedup();
    if declared.len() != manifest.capabilities.len()
        || unique.len() != declared.len()
        || manifest
            .capabilities
            .iter()
            .any(|c| !KNOWN_CAPABILITIES.contains(&c.as_str()))
    {
        return Err(invalid("CAPABILITIES"));
    }
    let schema = metadata.get("settings").and_then(Value::as_object);
    for field in &manifest.settings {
        let declaration = schema
            .and_then(|s| s.get(&field.key))
            .cloned()
            .unwrap_or_default();
        let mut optional = field.clone();
        optional.required = false;
        if field.key.trim().is_empty()
            || matches!((field.min, field.max), (Some(min), Some(max)) if min > max)
            || !optional.accepts(&field.default)
            || (field.kind == SettingKind::Select && field.options.is_empty())
            || field.options.iter().any(|o| {
                o.label.chars().count() > 256
                    || o.description.chars().count() > 2048
                    || o.kind.chars().count() > 64
            })
        {
            return Err(invalid("SETTINGS"));
        }
        for key in ["required", "secret"] {
            if declaration.get(key).is_some_and(|v| !v.is_boolean()) {
                return Err(invalid("SETTINGS"));
            }
        }
        for key in ["min", "max"] {
            if declaration
                .get(key)
                .is_some_and(|v| v.as_f64().is_none_or(|f| !f.is_finite()))
            {
                return Err(invalid("SETTINGS"));
            }
        }
    }
    let script = utf8(script_bytes)?;
    if script.trim().is_empty() {
        return Err(invalid("SCRIPT_EMPTY"));
    }
    if script.len() + metadata.to_string().len() + 1024 > MAX_SOURCE_BYTES {
        return Err(invalid("SIZE"));
    }
    Ok(Package {
        manifest,
        script,
        sha256: sha256_hex(bytes),
    })
}

/// The entry count of the end-of-central-directory record (`None` for ZIP64 /
/// not found). The ZIP reader indexes entries by name, so this is how
/// duplicate names are noticed.
fn central_directory_entries(bytes: &[u8]) -> Option<usize> {
    const EOCD: [u8; 4] = [b'P', b'K', 5, 6];
    let earliest = bytes.len().saturating_sub(22 + 65_535);
    let at = (earliest..=bytes.len().checked_sub(22)?)
        .rev()
        .find(|&i| bytes[i..i + 4] == EOCD)?;
    let total = u16::from_le_bytes([bytes[at + 10], bytes[at + 11]]);
    (total != 0xffff).then_some(total as usize)
}

/// `[a-z0-9][a-z0-9._-]{0,127}`.
fn valid_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && matches!(bytes[0], b'a'..=b'z' | b'0'..=b'9')
        && bytes
            .iter()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
}

fn checksum(bytes: &[u8], expected: Option<&Value>, code: &str) -> ModuleResult<()> {
    let expected = match expected.and_then(Value::as_str) {
        Some(e) if is_sha256_hex(e) => e,
        _ => return Err(invalid(code)),
    };
    if sha256_hex(bytes) != expected {
        return Err(ModuleError::at(
            Failure::Integrity,
            Stage::Decode,
            format!("ARCH_{code}"),
        ));
    }
    Ok(())
}

/// Settings as a keyed object (arrays with `key`s accepted), `default` as a
/// typed default, and `qualityOptions` turned into a `quality` select.
pub(crate) fn normalize_settings(metadata: &mut Map<String, Value>) -> ModuleResult<()> {
    let bad = || invalid("SETTINGS");
    let mut fields: Map<String, Value> = match metadata.get("settings") {
        None => Map::new(),
        Some(Value::Object(o)) => o.clone(),
        Some(Value::Array(items)) => {
            let mut keyed = Map::new();
            for item in items {
                let key = item.get("key").and_then(Value::as_str).ok_or_else(bad)?;
                if key.trim().is_empty() || keyed.contains_key(key) || !item.is_object() {
                    return Err(bad());
                }
                keyed.insert(key.to_owned(), item.clone());
            }
            keyed
        }
        Some(_) => return Err(bad()),
    };
    for field in fields.values_mut() {
        let Value::Object(f) = field else {
            return Err(bad());
        };
        if !f.contains_key("defaultValue")
            && !f.contains_key("value")
            && let Some(default) = f.get("default").cloned()
        {
            f.insert("defaultValue".into(), default);
        }
    }
    let quality_err = || invalid("QUALITY_OPTIONS");
    if let Some(declared) = metadata.get("qualityOptions") {
        let declared = declared.as_array().ok_or_else(quality_err)?;
        if !(1..=256).contains(&declared.len()) || fields.contains_key("quality") {
            return Err(quality_err());
        }
        let mut options = Vec::new();
        let mut ids: Vec<Value> = Vec::new();
        for option in declared {
            let option = option.as_object().ok_or_else(quality_err)?;
            let id = option.get("id").cloned().unwrap_or(Value::Null);
            if id.is_null()
                || id.is_object()
                || id.is_array()
                || ids.iter().any(|i| equivalent(i, &id))
            {
                return Err(quality_err());
            }
            for key in ["label", "description", "kind"] {
                if option.get(key).is_some_and(|v| !v.is_string()) {
                    return Err(quality_err());
                }
            }
            let label = option
                .get("label")
                .and_then(Value::as_str)
                .ok_or_else(quality_err)?;
            let text = |k: &str| {
                option
                    .get(k)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned()
            };
            if label.trim().is_empty()
                || label.chars().count() > 256
                || text("description").chars().count() > 2048
                || text("kind").chars().count() > 64
            {
                return Err(quality_err());
            }
            ids.push(id.clone());
            options.push(serde_json::json!({"label": label, "value": id,
                "description": text("description"), "kind": text("kind")}));
        }
        let default = metadata
            .get("defaultQuality")
            .cloned()
            .unwrap_or_else(|| ids[0].clone());
        if !ids.iter().any(|i| equivalent(i, &default)) {
            return Err(quality_err());
        }
        let label = match metadata.get("qualityLabel") {
            Some(Value::String(s)) => s.clone(),
            _ => "quality".into(),
        };
        if label.trim().is_empty() || label.chars().count() > 256 {
            return Err(quality_err());
        }
        fields.insert(
            "quality".into(),
            serde_json::json!({"type": "select", "label": label, "defaultValue": default, "options": options}),
        );
    } else if metadata.contains_key("defaultQuality") {
        return Err(quality_err());
    }
    metadata.insert("settings".into(), Value::Object(fields));
    Ok(())
}

/// Packs an authoring directory (`manifest.json`, `provider.js`, optional
/// `icon.png` declared as `module.icon`) into `.arch` bytes, adding `sha256`
/// (and `iconSha256`) to the packaged manifest like the reference packager.
/// The output is deterministic (fixed timestamps, sorted entries).
pub fn pack_dir(dir: &Path) -> std::io::Result<Vec<u8>> {
    let manifest_text = std::fs::read_to_string(dir.join("manifest.json"))?;
    let script = std::fs::read(dir.join("provider.js"))?;
    let mut manifest: Map<String, Value> = serde_json::from_str(
        manifest_text
            .strip_prefix('\u{feff}')
            .unwrap_or(&manifest_text),
    )
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    manifest.insert("sha256".into(), sha256_hex(&script).into());
    let declares_icon = manifest.get("module").and_then(|m| m.get("icon")).is_some();
    let icon = if declares_icon {
        let icon = std::fs::read(dir.join("icon.png"))?;
        manifest.insert("iconSha256".into(), sha256_hex(&icon).into());
        Some(icon)
    } else {
        None
    };
    let mut out = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut out);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .last_modified_time(zip::DateTime::default())
            .unix_permissions(0o644);
        zip.start_file("manifest.json", options)?;
        zip.write_all(
            serde_json::to_string_pretty(&manifest)
                .unwrap_or_default()
                .as_bytes(),
        )?;
        zip.write_all(b"\n")?;
        zip.start_file("provider.js", options)?;
        zip.write_all(&script)?;
        if let Some(icon) = icon {
            zip.start_file("icon.png", options)?;
            zip.write_all(&icon)?;
        }
        zip.finish()?;
    }
    Ok(out.into_inner())
}
