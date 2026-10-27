// Installed-module registry ported from ArchiveTune
// app/src/main/kotlin/moe/rukamori/archivetune/modules/ModuleRepository.kt and
// modules/src/main/kotlin/moe/rukamori/archivetune/modules/ModuleRuntimeStorage.kt
// — © Rukamori and ArchiveTune contributors, GPL-3.0
//! What is installed, persisted as `registry.json` in the modules directory
//! (written atomically, mode 0600), plus per-installation script storage and
//! diagnostic logs.
//!
//! Secret settings (passwords, tokens, …) never go into the registry file:
//! they are handed to a [`SecretHook`] supplied by the app (e.g. the OS
//! keychain). [`FileSecrets`] is a plain 0600 JSON file for tests and
//! headless use.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::catalog::SavedCatalog;
use super::error::{Failure, ModuleError, ModuleResult, Stage};
use super::manifest::ModuleManifest;

/// Where secret module settings are kept. Keys look like
/// `module.<installation id>.<setting key>`; values are JSON text.
pub trait SecretHook: Send + Sync {
    fn get(&self, key: &str) -> Option<String>;
    /// `None` deletes.
    fn set(&self, key: &str, value: Option<&str>) -> Result<(), String>;
}

/// Secrets in a JSON file (mode 0600). For tests and setups without a keychain.
pub struct FileSecrets {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileSecrets {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            lock: Mutex::new(()),
        }
    }

    fn read(&self) -> BTreeMap<String, String> {
        std::fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
}

impl SecretHook for FileSecrets {
    fn get(&self, key: &str) -> Option<String> {
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        self.read().remove(key)
    }

    fn set(&self, key: &str, value: Option<&str>) -> Result<(), String> {
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let mut all = self.read();
        match value {
            Some(v) => all.insert(key.to_owned(), v.to_owned()),
            None => all.remove(key),
        };
        let bytes = serde_json::to_vec_pretty(&all).map_err(|e| e.to_string())?;
        write_private(&self.path, &bytes).map_err(|e| e.to_string())
    }
}

/// Writes via a temp file + rename, readable only by the user.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// One installed module.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstalledModule {
    /// Installation id (stable across updates).
    pub id: String,
    pub manifest: ModuleManifest,
    /// `provider.js`.
    pub script: String,
    /// Where it was imported from (base for relative URLs; empty for files).
    #[serde(default)]
    pub origin: String,
    /// Revisited by "Check for update" (empty: file import).
    #[serde(default)]
    pub update_url: String,
    pub enabled: bool,
    /// Non-secret setting values (secret ones live behind the [`SecretHook`]).
    #[serde(default)]
    pub configuration: BTreeMap<String, Value>,
    /// Changes with every update or settings change (engines and cached
    /// resolutions of an older revision are dropped).
    pub revision: String,
    /// SHA-256 of the installed package.
    #[serde(default)]
    pub package_sha256: String,
    /// Unix milliseconds.
    #[serde(default)]
    pub installed_at: u64,
}

/// Everything persisted in `registry.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Registry {
    pub version: u32,
    /// "Use modules": resolve songs through enabled modules first.
    pub enabled: bool,
    /// Play from YouTube when no module resolved a song (otherwise it fails).
    #[serde(default = "yes")]
    pub fallback_to_youtube: bool,
    /// Playback priority order.
    pub modules: Vec<InstalledModule>,
    #[serde(default)]
    pub catalogs: Vec<SavedCatalog>,
}

fn yes() -> bool {
    true
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            version: 1,
            enabled: false,
            fallback_to_youtube: true,
            modules: Vec::new(),
            catalogs: Vec::new(),
        }
    }
}

const MAX_REGISTRY_BYTES: u64 = 32 * 1024 * 1024;

impl Registry {
    pub(crate) fn load(path: &Path) -> ModuleResult<Self> {
        let storage = |_| ModuleError::at(Failure::InvalidDocument, Stage::Storage, "REGISTRY");
        match std::fs::metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(_) => {
                return Err(ModuleError::at(
                    Failure::InvalidDocument,
                    Stage::Storage,
                    "REGISTRY",
                ));
            }
            Ok(meta) if meta.len() > MAX_REGISTRY_BYTES => {
                return Err(ModuleError::at(
                    Failure::InvalidDocument,
                    Stage::Storage,
                    "REGISTRY",
                ));
            }
            Ok(_) => {}
        }
        let bytes = std::fs::read(path).map_err(|_| storage(()))?;
        let registry: Self = serde_json::from_slice(&bytes).map_err(|_| storage(()))?;
        if registry.version != 1 {
            return Err(storage(()));
        }
        Ok(registry)
    }

    pub(crate) fn save(&self, path: &Path) -> ModuleResult<()> {
        if self.modules.len() > 256 || self.catalogs.len() > 128 {
            return Err(ModuleError::invalid());
        }
        let bytes = serde_json::to_vec(self).map_err(|_| ModuleError::invalid())?;
        if bytes.len() as u64 > MAX_REGISTRY_BYTES {
            return Err(ModuleError::invalid());
        }
        write_private(path, &bytes)
            .map_err(|_| ModuleError::at(Failure::Execution, Stage::Storage, "REGISTRY_WRITE"))
    }
}

/// A line of a module's diagnostic log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// Unix milliseconds.
    pub at: u64,
    pub level: String,
    pub text: String,
}

/// The last 200 log lines of a module (its `console` output and host notes).
#[derive(Default)]
pub struct LogBuffer {
    lines: Mutex<VecDeque<LogLine>>,
}

impl LogBuffer {
    pub fn push(&self, level: &str, text: &str) {
        let truncated = text.chars().count() > 1000;
        let mut text: String = text.chars().take(1000).collect();
        if truncated {
            text.push('…');
        }
        let mut lines = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
        lines.push_back(LogLine {
            at: now_ms(),
            level: level.chars().take(8).collect(),
            text,
        });
        while lines.len() > 200 {
            lines.pop_front();
        }
    }

    pub fn lines(&self) -> Vec<LogLine> {
        self.lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }

    pub fn clear(&self) {
        self.lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

/// The script-visible `storage` of one installation: a JSON object of at most
/// 1 MiB in `storage/<sha256(installation id)>.json` (mode 0600).
pub struct ModuleStorage {
    path: PathBuf,
    lock: Mutex<()>,
}

const STORAGE_LIMIT: usize = 1024 * 1024;

impl ModuleStorage {
    pub(crate) fn new(dir: &Path, installation: &str) -> Self {
        Self {
            path: dir.join("storage").join(format!(
                "{}.json",
                super::json::sha256_hex(installation.as_bytes())
            )),
            lock: Mutex::new(()),
        }
    }

    fn guard(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn read(&self) -> serde_json::Map<String, Value> {
        std::fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// `{action: get|set|remove|clear|keys, key, value}`.
    pub(crate) fn call(&self, input: &Value) -> Result<Value, String> {
        let _guard = self.guard();
        let mut values = self.read();
        let key = input.get("key").and_then(Value::as_str).unwrap_or_default();
        let action = input
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let write = |values: &serde_json::Map<String, Value>| -> Result<(), String> {
            let bytes = serde_json::to_vec(values).map_err(|e| e.to_string())?;
            if bytes.len() > STORAGE_LIMIT {
                return Err("storage limit (1 MiB) exceeded".into());
            }
            write_private(&self.path, &bytes).map_err(|e| e.to_string())
        };
        Ok(match action {
            "get" => values.get(key).cloned().unwrap_or(Value::Null),
            "keys" => Value::Array(values.keys().cloned().map(Value::from).collect()),
            "set" => {
                values.insert(
                    key.to_owned(),
                    input.get("value").cloned().unwrap_or(Value::Null),
                );
                write(&values)?;
                Value::Bool(true)
            }
            "remove" => {
                values.remove(key);
                write(&values)?;
                Value::Bool(true)
            }
            "clear" => {
                write(&serde_json::Map::new())?;
                Value::Bool(true)
            }
            _ => return Err("unknown storage action".into()),
        })
    }

    pub(crate) fn remove(&self) {
        let _guard = self.guard();
        let _ = std::fs::remove_file(&self.path);
    }
}
