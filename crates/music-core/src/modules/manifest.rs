// Manifest and setting contracts ported from ArchiveTune
// modules/src/main/kotlin/moe/rukamori/archivetune/modules/ModuleModels.kt (ModuleManifest.parse,
// ModuleSetting.acceptsValue, requireCompatible, moduleIcon) — © Rukamori and ArchiveTune
// contributors, GPL-3.0
//! The normalized module manifest and its typed settings (MODULES.md "Manifest",
//! "Settings").

use std::sync::LazyLock;

use base64::Engine as _;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::error::{Failure, ModuleError, ModuleResult, Stage};
use super::http::web_url;
use super::json::{equivalent, finite, opt_str, strings};

/// Capability names a provider may export; the first two are required.
pub const KNOWN_CAPABILITIES: [&str; 5] = [
    "searchTracks",
    "getTrackStreamUrl",
    "getAlbum",
    "getArtist",
    "getPlaylist",
];

/// Host features this build implements (`requiredRuntimeFeatures`).
pub const RUNTIME_FEATURES: [&str; 5] = [
    "fetch@1",
    "settings@1",
    "streamResolution@1",
    "packagedIcon@1",
    "qualityOptions@1",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingKind {
    Text,
    /// Masked input, always secret.
    Password,
    Number,
    Boolean,
    Select,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingOption {
    pub label: String,
    pub value: Value,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub kind: String,
}

/// One declared setting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingField {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    pub kind: SettingKind,
    /// Typed default; `null` = unconfigured.
    #[serde(default)]
    pub default: Value,
    #[serde(default)]
    pub required: bool,
    /// Stored through the secret hook, never in the registry file or backups.
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub options: Vec<SettingOption>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    /// `integer` fields.
    #[serde(default)]
    pub integer: bool,
}

static SENSITIVE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)token|password|secret|credential|api.?key|cookie|authorization").unwrap()
});

impl SettingField {
    /// Whether `value` is acceptable for this field (`acceptsValue`).
    pub fn accepts(&self, value: &Value) -> bool {
        if value.is_null() {
            return !self.required;
        }
        if self.required {
            let blank = match value {
                Value::String(s) => s.trim().is_empty(),
                _ => false,
            };
            if blank {
                return false;
            }
        }
        match self.kind {
            SettingKind::Text | SettingKind::Password => value.is_string(),
            SettingKind::Boolean => value.is_boolean(),
            SettingKind::Number => match value.as_f64() {
                Some(n) if value.is_number() && n.is_finite() => {
                    (!self.integer || n.fract() == 0.0)
                        && self.min.is_none_or(|m| n >= m)
                        && self.max.is_none_or(|m| n <= m)
                }
                _ => false,
            },
            SettingKind::Select => self.options.iter().any(|o| equivalent(&o.value, value)),
        }
    }

    fn parse(key: &str, field: &Value) -> ModuleResult<Option<Self>> {
        let raw_type = field
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("text")
            .to_ascii_lowercase();
        let secret = field.get("secret").and_then(Value::as_bool) == Some(true)
            || raw_type == "password"
            || SENSITIVE_KEY.is_match(key);
        let kind = match raw_type.as_str() {
            "text" | "string" | "url" if secret => SettingKind::Password,
            "text" | "string" | "url" => SettingKind::Text,
            "password" => SettingKind::Password,
            "number" | "integer" => SettingKind::Number,
            "boolean" | "switch" | "toggle" | "checkbox" => SettingKind::Boolean,
            "select" | "selector" | "dropdown" => SettingKind::Select,
            _ => return Ok(None),
        };
        let options = match field.get("options") {
            Some(Value::Array(items)) => items.as_slice(),
            _ => &[],
        };
        let label = opt_str(field, "label");
        if options.len() > 256 || key.chars().count() > 128 || label.chars().count() > 256 {
            return Err(ModuleError::invalid());
        }
        let default = field
            .get("defaultValue")
            .or_else(|| field.get("value"))
            .cloned()
            .unwrap_or(Value::Null);
        Ok(Some(Self {
            key: key.to_owned(),
            label: if field.get("label").is_some_and(Value::is_string) {
                label.to_owned()
            } else {
                key.to_owned()
            },
            description: opt_str(field, "description").to_owned(),
            kind,
            default,
            required: field.get("required").and_then(Value::as_bool) == Some(true),
            secret,
            options: options
                .iter()
                .map(|option| match option {
                    Value::Object(o) => {
                        let value = o.get("value").cloned().unwrap_or(Value::Null);
                        SettingOption {
                            label: o
                                .get("label")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                                .unwrap_or_else(|| display(&value)),
                            value,
                            description: opt_str(option, "description").to_owned(),
                            kind: opt_str(option, "kind").to_owned(),
                        }
                    }
                    other => SettingOption {
                        label: display(other),
                        value: other.clone(),
                        description: String::new(),
                        kind: String::new(),
                    },
                })
                .collect(),
            min: finite(field.get("min")),
            max: finite(field.get("max")),
            integer: raw_type == "integer",
        }))
    }

    /// The canonical declaration (used when re-validating a stored manifest).
    fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("label".into(), self.label.clone().into());
        o.insert("description".into(), self.description.clone().into());
        let kind = match (self.kind, self.integer) {
            (SettingKind::Number, true) => "integer",
            (SettingKind::Number, false) => "number",
            (SettingKind::Text, _) => "text",
            (SettingKind::Password, _) => "password",
            (SettingKind::Boolean, _) => "boolean",
            (SettingKind::Select, _) => "select",
        };
        o.insert("type".into(), kind.into());
        o.insert("defaultValue".into(), self.default.clone());
        o.insert("required".into(), self.required.into());
        o.insert("secret".into(), self.secret.into());
        if let Some(min) = self.min {
            o.insert("min".into(), min.into());
        }
        if let Some(max) = self.max {
            o.insert("max".into(), max.into());
        }
        o.insert(
            "options".into(),
            Value::Array(
                self.options
                    .iter()
                    .map(|opt| {
                        serde_json::json!({"label": opt.label, "value": opt.value,
                            "description": opt.description, "kind": opt.kind})
                    })
                    .collect(),
            ),
        );
        Value::Object(o)
    }
}

/// A JSON value as option label text (strings without quotes).
fn display(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A validated, normalized module manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModuleManifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub homepage: String,
    #[serde(default)]
    pub repository: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub types: Vec<String>,
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub min_app_version: String,
    #[serde(default)]
    pub required_runtime_features: Vec<String>,
    #[serde(default)]
    pub settings: Vec<SettingField>,
    /// The packaged PNG icon (base64 in the registry file).
    #[serde(default, with = "icon_base64")]
    pub icon: Option<Vec<u8>>,
}

mod icon_base64 {
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(icon: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match icon {
            Some(bytes) => {
                s.serialize_some(&base64::engine::general_purpose::STANDARD.encode(bytes))
            }
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        let text: Option<String> = Option::deserialize(d)?;
        Ok(text.and_then(|t| base64::engine::general_purpose::STANDARD.decode(t).ok()))
    }
}

impl ModuleManifest {
    /// Display name: `displayName`, else `name`.
    pub fn title(&self) -> &str {
        if self.display_name.trim().is_empty() {
            &self.name
        } else {
            &self.display_name
        }
    }

    pub fn setting(&self, key: &str) -> Option<&SettingField> {
        self.settings.iter().find(|f| f.key == key)
    }

    /// `ModuleManifest.parse` of a normalized `module` object (settings keyed).
    pub(crate) fn parse(value: &Value) -> ModuleResult<Self> {
        let empty = Map::new();
        let schema = value
            .get("settings")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        if schema.len() > 128 {
            return Err(ModuleError::invalid());
        }
        let mut settings = Vec::new();
        for (key, field) in schema {
            match SettingField::parse(key, field)? {
                Some(f) => settings.push(f),
                None => {
                    return Err(ModuleError::at(
                        Failure::Unsupported,
                        Stage::Manifest,
                        "ARCH_SETTING_TYPE",
                    ));
                }
            }
        }
        let mut labels = strings(value.get("labels"));
        for tag in strings(value.get("tags")) {
            if !labels.contains(&tag) {
                labels.push(tag);
            }
        }
        let icon = match value.get("iconBase64").and_then(Value::as_str) {
            Some(text) if !text.trim().is_empty() => Some(decode_icon(text)?),
            _ => None,
        };
        let manifest = Self {
            id: opt_str(value, "id").to_owned(),
            name: opt_str(value, "name").to_owned(),
            display_name: opt_str(value, "displayName").to_owned(),
            version: opt_str(value, "version").to_owned(),
            author: opt_str(value, "author").to_owned(),
            description: opt_str(value, "description").to_owned(),
            homepage: project_url(value, "homepage")?,
            repository: project_url(value, "repository")?,
            license: opt_str(value, "license").to_owned(),
            labels,
            types: string_list(value, "type")?,
            capabilities: strings(value.get("capabilities")),
            min_app_version: opt_str(value, "minAppVersion").to_owned(),
            required_runtime_features: string_list(value, "requiredRuntimeFeatures")?,
            settings,
            icon,
        };
        if manifest.id.trim().is_empty()
            || manifest.name.trim().is_empty()
            || !["searchTracks", "getTrackStreamUrl"]
                .iter()
                .all(|c| manifest.capabilities.iter().any(|m| m == c))
        {
            return Err(ModuleError::new(Failure::Unsupported));
        }
        let len = |s: &str| s.chars().count();
        if len(&manifest.id) > 256
            || len(&manifest.name) > 256
            || len(&manifest.version) > 128
            || len(&manifest.description) > 8192
            || len(&manifest.display_name) > 256
            || len(&manifest.license) > 256
            || (!manifest.min_app_version.is_empty() && !is_version(&manifest.min_app_version))
        {
            return Err(ModuleError::invalid());
        }
        Ok(manifest)
    }

    /// Rejects unknown required features and a too-new `minAppVersion`.
    pub fn require_compatible(&self, app_version: &str) -> ModuleResult<()> {
        if self
            .required_runtime_features
            .iter()
            .any(|f| !RUNTIME_FEATURES.contains(&f.as_str()))
        {
            return Err(ModuleError::at(
                Failure::Unsupported,
                Stage::Manifest,
                "ARCH_RUNTIME_FEATURE",
            ));
        }
        if self.min_app_version.is_empty() {
            return Ok(());
        }
        let current: Option<Vec<u32>> = {
            let prefix: String = app_version
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            let parts: Vec<Option<u32>> =
                prefix.split('.').take(3).map(|p| p.parse().ok()).collect();
            (parts.len() == 3)
                .then(|| parts.into_iter().collect())
                .flatten()
        };
        let required: Vec<u32> = self
            .min_app_version
            .split('.')
            .filter_map(|p| p.parse().ok())
            .collect();
        let too_old = match current {
            None => true,
            Some(current) => current < required,
        };
        if too_old {
            return Err(ModuleError::at(
                Failure::Unsupported,
                Stage::Manifest,
                "ARCH_APP_VERSION",
            ));
        }
        Ok(())
    }

    /// Defaults for every declared setting.
    pub fn defaults(&self) -> std::collections::BTreeMap<String, Value> {
        self.settings
            .iter()
            .map(|f| (f.key.clone(), f.default.clone()))
            .collect()
    }

    /// The `module` object handed to the provider wrapper (identity + settings).
    pub(crate) fn metadata_json(&self) -> Value {
        let mut settings = Map::new();
        for field in &self.settings {
            settings.insert(field.key.clone(), field.to_json());
        }
        serde_json::json!({
            "id": self.id, "name": self.name, "version": self.version,
            "author": self.author, "description": self.description,
            "labels": self.labels, "settings": settings,
            "displayName": self.display_name, "homepage": self.homepage,
            "repository": self.repository, "license": self.license, "type": self.types,
            "minAppVersion": self.min_app_version,
            "requiredRuntimeFeatures": self.required_runtime_features,
            "capabilities": self.capabilities,
            "iconBase64": self.icon.as_ref()
                .map(|b| base64::engine::general_purpose::STANDARD.encode(b))
                .unwrap_or_default(),
        })
    }
}

/// `[0-9]{1,6}\.[0-9]{1,6}\.[0-9]{1,6}`.
fn is_version(text: &str) -> bool {
    let parts: Vec<&str> = text.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| (1..=6).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()))
}

fn project_url(value: &Value, key: &str) -> ModuleResult<String> {
    let text = opt_str(value, key);
    if text.trim().is_empty() {
        return Ok(String::new());
    }
    if text.chars().count() > 2048 {
        return Err(ModuleError::invalid());
    }
    web_url(text)
}

/// A string or an array of strings (each nonblank, at most 128 chars, at most 32).
fn string_list(value: &Value, key: &str) -> ModuleResult<Vec<String>> {
    let values: Vec<String> = match value.get(key) {
        None => return Ok(Vec::new()),
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(ModuleError::invalid)
            })
            .collect::<ModuleResult<_>>()?,
        Some(_) => return Err(ModuleError::invalid()),
    };
    if values.len() > 32
        || values
            .iter()
            .any(|v| v.trim().is_empty() || v.chars().count() > 128)
    {
        return Err(ModuleError::invalid());
    }
    let mut out: Vec<String> = Vec::new();
    for v in values {
        if !out.contains(&v) {
            out.push(v);
        }
    }
    Ok(out)
}

fn decode_icon(encoded: &str) -> ModuleResult<Vec<u8>> {
    if encoded.len() > 349_528 {
        return Err(ModuleError::invalid());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|_| ModuleError::invalid())?;
    check_png(&bytes).map_err(|_| ModuleError::invalid())?;
    Ok(bytes)
}

/// PNG signature, size and IHDR dimensions (1–1024 px per axis, at most 256 KiB).
pub(crate) fn check_png(bytes: &[u8]) -> Result<(u32, u32), ()> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes.len() > 256 * 1024
        || bytes.len() < 24
        || bytes[..8] != SIGNATURE
        || &bytes[12..16] != b"IHDR"
    {
        return Err(());
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().map_err(|_| ())?);
    let h = u32::from_be_bytes(bytes[20..24].try_into().map_err(|_| ())?);
    if !(1..=1024).contains(&w) || !(1..=1024).contains(&h) {
        return Err(());
    }
    Ok((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base() -> Value {
        json!({"id": "org.example.x", "name": "X", "version": "1",
               "capabilities": ["searchTracks", "getTrackStreamUrl"]})
    }

    #[test]
    fn settings_are_typed_and_secret_keys_detected() {
        let mut m = base();
        m["settings"] = json!({
            "apiToken": {"type": "text", "label": "Token"},
            "quality": {"type": "select", "defaultValue": "lossless",
                        "options": [{"label": "Lossless", "value": "lossless"}, "hires"]},
            "limit": {"type": "integer", "min": 1, "max": 50, "defaultValue": 10},
            "fast": {"type": "toggle", "value": true},
        });
        let parsed = ModuleManifest::parse(&m).unwrap();
        let token = parsed.setting("apiToken").unwrap();
        assert_eq!(token.kind, SettingKind::Password);
        assert!(token.secret);
        let quality = parsed.setting("quality").unwrap();
        assert!(quality.accepts(&json!("hires")));
        assert!(!quality.accepts(&json!("nope")));
        let limit = parsed.setting("limit").unwrap();
        assert!(limit.accepts(&json!(5)));
        assert!(!limit.accepts(&json!(5.5)));
        assert!(!limit.accepts(&json!(51)));
        assert_eq!(parsed.setting("fast").unwrap().default, json!(true));
        // Round trip through the canonical declaration.
        let again = ModuleManifest::parse(&parsed.metadata_json()).unwrap();
        assert_eq!(again.settings.len(), 4);
    }

    #[test]
    fn identity_and_compatibility() {
        let mut m = base();
        m["capabilities"] = json!(["searchTracks"]);
        assert_eq!(
            ModuleManifest::parse(&m).unwrap_err().failure,
            Failure::Unsupported
        );
        let mut m = base();
        m["minAppVersion"] = json!("2.0.0");
        m["requiredRuntimeFeatures"] = json!("fetch@1");
        let parsed = ModuleManifest::parse(&m).unwrap();
        assert!(parsed.require_compatible("1.9.9").is_err());
        assert!(parsed.require_compatible("2.0.0-beta").is_ok());
        let mut m = base();
        m["requiredRuntimeFeatures"] = json!(["teleport@1"]);
        let err = ModuleManifest::parse(&m)
            .unwrap()
            .require_compatible("1.0.0")
            .unwrap_err();
        assert_eq!(err.code.as_deref(), Some("ARCH_RUNTIME_FEATURE"));
        let mut m = base();
        m["homepage"] = json!("ftp://example.org");
        assert!(ModuleManifest::parse(&m).is_err());
    }
}
