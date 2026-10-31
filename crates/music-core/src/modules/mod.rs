// Module management and resolution ported from ArchiveTune
// app/src/main/kotlin/moe/rukamori/archivetune/modules/ModuleRepository.kt,
// ModulePlaybackUseCases.kt (ResolveModuleAudioUseCase, diagnostics) and
// ModuleSettingsUseCases.kt — © Rukamori and ArchiveTune contributors, GPL-3.0
//! Provider modules: `.arch` packages that supply alternative audio for songs.
//!
//! A module is a JavaScript provider (QuickJS via `rquickjs`) with two
//! capabilities, `searchTracks(query, limit, context)` and
//! `getTrackStreamUrl(trackId, quality, context)`, packaged with a manifest
//! (MODULES.md describes the format and API v1). The song keeps its identity:
//! a module only supplies the audio.
//!
//! * [`Modules`] — install (file, direct URL, descriptor, catalog entry),
//!   update checks, enable / order / remove, typed settings (secret ones via a
//!   [`SecretHook`]), health checks, diagnostics and logs. Blocking; call it
//!   off the UI thread.
//! * [`ModuleResolver`] — a [`StreamResolver`] that resolves a YouTube
//!   [`Track`] through the enabled modules in priority order (search →
//!   [`matcher`] → stream URL) and falls back to another resolver (YouTube).
//!
//! Limits follow the reference: 5 s to initialize a package, 15 s per module
//! (search + match + stream), 30 s for the whole ordered sequence. HLS / DASH
//! results are rejected (this player streams progressive files only).
//!
//! No module, catalog or provider is bundled or preconfigured.

mod catalog;
mod error;
mod http;
mod json;
mod manifest;
pub mod matcher;
pub mod package;
mod registry;
mod runtime;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use catalog::{CatalogEntry, SavedCatalog};
pub use error::{Failure, ModuleError, ModuleResult, Stage};
pub use http::{resolve_url, web_url};
pub use manifest::{
    KNOWN_CAPABILITIES, ModuleManifest, RUNTIME_FEATURES, SettingField, SettingKind, SettingOption,
};
pub use matcher::{MatchRejection, MatchTarget};
pub use registry::{FileSecrets, InstalledModule, LogLine, Registry, SecretHook};
pub use runtime::{ModuleStream, ModuleTrack};

use registry::{LogBuffer, ModuleStorage, now_ms};
use runtime::{EngineSpec, Engines, call_context, parse_search, parse_stream};

use crate::{Quality, ResolvedStream, SourceKind, StreamResolver, StreamSource, Track};

/// Time to load a package and check its exports.
pub const INIT_TIMEOUT: Duration = Duration::from_secs(5);
/// Search + match + stream for one module.
pub const MODULE_TIMEOUT: Duration = Duration::from_secs(15);
/// The whole ordered module sequence for one song.
pub const SEQUENCE_TIMEOUT: Duration = Duration::from_secs(30);
/// Results requested from `searchTracks`.
pub const SEARCH_LIMIT: u32 = 10;

/// Outcome of a health check or a playback resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckStatus {
    /// Initialization / configuration succeeded (not proof of audio access).
    Ready,
    Configuration,
    Unavailable,
    Failed,
    NoMatch,
    Resolved,
    Timeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolutionPhase {
    Search,
    Match,
    Stream,
}

/// One recorded check or playback attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub status: CheckStatus,
    /// Unix milliseconds.
    pub at: u64,
    pub elapsed_ms: u64,
    pub failure: Option<Failure>,
    pub revision: String,
    pub phase: Option<ResolutionPhase>,
    pub candidates: usize,
    pub rejection: Option<MatchRejection>,
    pub upstream: String,
    pub variant: String,
    /// Redacted diagnostic code.
    pub code: String,
    /// The song (title — artist) for playback outcomes.
    pub track: String,
}

impl Diagnostic {
    fn new(status: CheckStatus, started: Instant, revision: &str) -> Self {
        Self {
            status,
            at: now_ms(),
            elapsed_ms: started.elapsed().as_millis() as u64,
            failure: None,
            revision: revision.to_owned(),
            phase: None,
            candidates: 0,
            rejection: None,
            upstream: String::new(),
            variant: String::new(),
            code: String::new(),
            track: String::new(),
        }
    }

    fn failed(error: &ModuleError, started: Instant, revision: &str) -> Self {
        let status = match error.failure {
            Failure::InvalidConfiguration => CheckStatus::Configuration,
            Failure::Network => CheckStatus::Unavailable,
            Failure::Timeout => CheckStatus::Timeout,
            _ => CheckStatus::Failed,
        };
        Self {
            failure: Some(error.failure),
            code: error.diagnostic(),
            ..Self::new(status, started, revision)
        }
    }

    /// A one-line summary for the UI.
    pub fn summary(&self) -> String {
        let status = match self.status {
            CheckStatus::Ready => "Ready".to_owned(),
            CheckStatus::Configuration => "Needs configuration".into(),
            CheckStatus::Unavailable => "Service unavailable".into(),
            CheckStatus::Failed => "Failed".into(),
            CheckStatus::NoMatch => match self.rejection {
                Some(MatchRejection::NoResults) | None => "No results".into(),
                Some(r) => format!("No acceptable match ({r:?})")
                    .to_lowercase()
                    .replacen('n', "N", 1),
            },
            CheckStatus::Resolved => "Resolved".into(),
            CheckStatus::Timeout => "Timed out".into(),
        };
        let mut parts = vec![status];
        if let Some(phase) = self.phase {
            parts.push(format!("in {phase:?}").to_lowercase());
        }
        if self.candidates > 0 {
            parts.push(format!("{} candidates", self.candidates));
        }
        if !self.variant.is_empty() {
            parts.push(self.variant.clone());
        }
        if !self.code.is_empty() {
            parts.push(self.code.clone());
        }
        parts.push(format!("{} ms", self.elapsed_ms));
        parts.join(" · ")
    }
}

/// The latest check, the latest playback outcome and the log of a module.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModuleHealth {
    pub check: Option<Diagnostic>,
    pub playback: Option<Diagnostic>,
    pub log: Vec<LogLine>,
}

/// A validated package waiting for the user's confirmation.
#[derive(Debug, Clone)]
pub struct PendingModule {
    pub manifest: ModuleManifest,
    pub script: String,
    pub origin: String,
    /// Empty for file imports.
    pub update_url: String,
    pub sha256: String,
    /// Installation this replaces (an update), if known.
    pub replacement: Option<String>,
}

/// What an import produced.
#[derive(Debug, Clone)]
pub enum Import {
    Module(Box<PendingModule>),
    Catalog(SavedCatalog),
}

/// A module stream chosen for a song.
#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    /// The song's video id.
    pub original_id: String,
    pub installation: String,
    pub module_name: String,
    pub candidate: ModuleTrack,
    pub stream: ModuleStream,
    /// Identity of this exact audio (module, revision, track, variant, …).
    pub cache_key: String,
}

impl Selection {
    /// The stream as the player / downloader consume it.
    pub fn to_resolved(&self) -> ResolvedStream {
        let codec = match self.stream.codecs.to_ascii_lowercase().as_str() {
            "" => None,
            "flac" => Some("FLAC".to_owned()),
            "alac" => Some("ALAC".into()),
            "opus" => Some("Opus".into()),
            "vorbis" => Some("Vorbis".into()),
            "mp3" | "mpeg" => Some("MP3".into()),
            c if c.starts_with("mp4a") || c == "aac" => Some("AAC".into()),
            _ => Some(self.stream.codecs.clone()),
        };
        ResolvedStream {
            url: self.stream.url.clone(),
            itag: 0,
            mime_type: self.stream.mime_type.clone(),
            content_length: (self.stream.content_length > 0).then_some(self.stream.content_length),
            user_agent: "",
            loudness_db: None,
            headers: self.stream.headers.clone(),
            progressive: true,
            source: Some(StreamSource {
                kind: SourceKind::Module,
                provider: self.module_name.clone(),
                module_id: Some(self.installation.clone()),
                codec,
                sample_rate: self.stream.sample_rate,
                bits_per_sample: None,
                bitrate: (self.stream.bitrate > 0).then_some(self.stream.bitrate),
                channels: None,
                variant: (!self.stream.variant.is_empty()).then(|| self.stream.variant.clone()),
            }),
            not_before: None,
            chunk_size: None,
            full_access: true,
            prefix: None,
            valid_until: (self.stream.expires_at_ms > 0)
                .then(|| UNIX_EPOCH + Duration::from_millis(self.stream.expires_at_ms)),
            video: None,
        }
    }
}

struct CacheEntry {
    revision: String,
    target: MatchTarget,
    selection: Option<Selection>,
    /// Unix milliseconds.
    expires_at: u64,
}

struct Inner {
    dir: PathBuf,
    app_version: String,
    secrets: Arc<dyn SecretHook>,
    registry: Mutex<Registry>,
    /// Secret setting values read through the hook, per installation.
    secret_values: Mutex<HashMap<String, BTreeMap<String, Value>>>,
    engines: Engines,
    logs: Mutex<HashMap<String, Arc<LogBuffer>>>,
    storages: Mutex<HashMap<String, Arc<ModuleStorage>>>,
    checks: Mutex<HashMap<String, Diagnostic>>,
    playback: Mutex<HashMap<String, Diagnostic>>,
    cache: Mutex<HashMap<(String, String), CacheEntry>>,
    resolving: Mutex<()>,
    revision: AtomicU64,
    /// Per-module and whole-sequence budgets in milliseconds.
    module_timeout_ms: AtomicU64,
    sequence_timeout_ms: AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Installed provider modules. Cloning shares the same state.
#[derive(Clone)]
pub struct Modules {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Modules {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Modules")
            .field("dir", &self.inner.dir)
            .finish()
    }
}

fn secret_key(installation: &str, key: &str) -> String {
    format!("module.{installation}.{key}")
}

impl Modules {
    /// Opens (or creates) the module store in `dir`. `app_version` is compared
    /// with a package's `minAppVersion`. A corrupt registry is moved aside to
    /// `registry.json.bad` and an empty one used.
    pub fn open(
        dir: impl Into<PathBuf>,
        secrets: Arc<dyn SecretHook>,
        app_version: &str,
    ) -> ModuleResult<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)
            .map_err(|_| ModuleError::at(Failure::Execution, Stage::Storage, "DIR"))?;
        let path = dir.join("registry.json");
        let registry = match Registry::load(&path) {
            Ok(r) => r,
            Err(e) => {
                log::warn!("modules: {e}; starting with an empty registry");
                let _ = std::fs::rename(&path, dir.join("registry.json.bad"));
                Registry::default()
            }
        };
        Ok(Self {
            inner: Arc::new(Inner {
                dir,
                app_version: app_version.to_owned(),
                secrets,
                registry: Mutex::new(registry),
                secret_values: Mutex::default(),
                engines: Engines::default(),
                logs: Mutex::default(),
                storages: Mutex::default(),
                checks: Mutex::default(),
                playback: Mutex::default(),
                cache: Mutex::default(),
                resolving: Mutex::new(()),
                revision: AtomicU64::new(1),
                module_timeout_ms: AtomicU64::new(MODULE_TIMEOUT.as_millis() as u64),
                sequence_timeout_ms: AtomicU64::new(SEQUENCE_TIMEOUT.as_millis() as u64),
            }),
        })
    }

    /// Overrides the per-module (default 15 s) and whole-sequence (30 s)
    /// budgets, e.g. for tests.
    pub fn set_timeouts(&self, module: Duration, sequence: Duration) {
        self.inner
            .module_timeout_ms
            .store(module.as_millis() as u64, Ordering::Relaxed);
        self.inner
            .sequence_timeout_ms
            .store(sequence.as_millis() as u64, Ordering::Relaxed);
    }

    fn module_timeout(&self) -> Duration {
        Duration::from_millis(self.inner.module_timeout_ms.load(Ordering::Relaxed))
    }

    pub fn dir(&self) -> &Path {
        &self.inner.dir
    }

    /// Bumped by every change (installs, settings, diagnostics): poll it to refresh a UI.
    pub fn revision(&self) -> u64 {
        self.inner.revision.load(Ordering::Relaxed)
    }

    fn bump(&self) {
        self.inner.revision.fetch_add(1, Ordering::Relaxed);
    }

    /// A snapshot of what is installed (without secret values).
    pub fn registry(&self) -> Registry {
        lock(&self.inner.registry).clone()
    }

    pub fn module(&self, id: &str) -> Option<InstalledModule> {
        lock(&self.inner.registry)
            .modules
            .iter()
            .find(|m| m.id == id)
            .cloned()
    }

    fn change(&self, f: impl FnOnce(&mut Registry) -> ModuleResult<()>) -> ModuleResult<()> {
        let mut registry = lock(&self.inner.registry);
        let mut next = registry.clone();
        f(&mut next)?;
        next.save(&self.inner.dir.join("registry.json"))?;
        *registry = next;
        drop(registry);
        self.bump();
        Ok(())
    }

    /// "Use modules".
    pub fn set_enabled(&self, enabled: bool) -> ModuleResult<()> {
        self.change(|r| {
            r.enabled = enabled;
            Ok(())
        })
    }

    /// Play from YouTube when no module resolved a song.
    pub fn set_fallback_to_youtube(&self, fallback: bool) -> ModuleResult<()> {
        self.change(|r| {
            r.fallback_to_youtube = fallback;
            Ok(())
        })
    }

    pub fn set_module_enabled(&self, id: &str, enabled: bool) -> ModuleResult<()> {
        self.change(|r| {
            let m = r
                .modules
                .iter_mut()
                .find(|m| m.id == id)
                .ok_or_else(ModuleError::invalid)?;
            m.enabled = enabled;
            Ok(())
        })?;
        self.invalidate_failures(id);
        Ok(())
    }

    /// Moves a module up (`-1`) or down (`+1`) in the playback priority.
    pub fn move_module(&self, id: &str, delta: isize) -> ModuleResult<()> {
        self.change(|r| {
            let from = r
                .modules
                .iter()
                .position(|m| m.id == id)
                .ok_or_else(ModuleError::invalid)?;
            let to = (from as isize + delta).clamp(0, r.modules.len() as isize - 1) as usize;
            let m = r.modules.remove(from);
            r.modules.insert(to, m);
            Ok(())
        })
    }

    /// Uninstalls a module: its settings (secrets too), storage and engines.
    pub fn remove(&self, id: &str) -> ModuleResult<()> {
        let module = self.module(id).ok_or_else(ModuleError::invalid)?;
        self.change(|r| {
            r.modules.retain(|m| m.id != id);
            Ok(())
        })?;
        self.inner.engines.unload(id);
        for field in module.manifest.settings.iter().filter(|f| f.secret) {
            let _ = self.inner.secrets.set(&secret_key(id, &field.key), None);
        }
        lock(&self.inner.secret_values).remove(id);
        self.storage(id).remove();
        lock(&self.inner.storages).remove(id);
        lock(&self.inner.checks).remove(id);
        lock(&self.inner.playback).remove(id);
        lock(&self.inner.cache).retain(|(_, m), _| m != id);
        Ok(())
    }

    fn log(&self, id: &str) -> Arc<LogBuffer> {
        lock(&self.inner.logs)
            .entry(id.to_owned())
            .or_default()
            .clone()
    }

    fn storage(&self, id: &str) -> Arc<ModuleStorage> {
        lock(&self.inner.storages)
            .entry(id.to_owned())
            .or_insert_with(|| Arc::new(ModuleStorage::new(&self.inner.dir, id)))
            .clone()
    }

    fn spec(&self, module: &InstalledModule) -> EngineSpec {
        EngineSpec {
            key: format!("{}:{}", module.id, module.revision),
            installation: module.id.clone(),
            script: Arc::from(module.script.as_str()),
            metadata: module.manifest.metadata_json(),
            origin: module.origin.clone(),
            storage: self.storage(&module.id),
            log: self.log(&module.id),
        }
    }

    // ---- installing ----

    /// Reads a package (or descriptor / catalog JSON) from a file.
    pub fn import_file(&self, path: &Path) -> ModuleResult<Import> {
        let meta = std::fs::metadata(path)
            .map_err(|_| ModuleError::at(Failure::InvalidDocument, Stage::Download, "FILE"))?;
        if meta.len() > http::MAX_SOURCE_BYTES as u64 {
            return Err(ModuleError::arch("SIZE"));
        }
        let bytes = std::fs::read(path)
            .map_err(|_| ModuleError::at(Failure::InvalidDocument, Stage::Download, "FILE"))?;
        self.import_bytes(&bytes, "")
    }

    /// Validates a package, descriptor or catalog given as bytes. `origin` is
    /// the URL they came from (empty for files).
    pub fn import_bytes(&self, bytes: &[u8], origin: &str) -> ModuleResult<Import> {
        self.prepare(bytes, origin, None, &mut Vec::new(), 0)
    }

    /// Installs from a direct package URL, a descriptor or a catalog URL.
    pub fn import_url(&self, url: &str) -> ModuleResult<Import> {
        self.import_url_for(url, None, "")
    }

    fn import_url_for(
        &self,
        url: &str,
        replacement: Option<&str>,
        expected_sha256: &str,
    ) -> ModuleResult<Import> {
        if !expected_sha256.is_empty() && !json::is_sha256_hex(expected_sha256) {
            return Err(ModuleError::invalid());
        }
        let url = web_url(url)?;
        let (bytes, origin) = http::source(&url)?;
        if !expected_sha256.is_empty() && json::sha256_hex(&bytes) != expected_sha256 {
            return Err(ModuleError::at(
                Failure::Integrity,
                Stage::Download,
                "CATALOG_CHECKSUM",
            ));
        }
        let mut visited = Vec::new();
        let prepared = self.prepare(&bytes, &origin, replacement, &mut visited, 0)?;
        Ok(match prepared {
            Import::Module(mut m) => {
                if m.update_url.is_empty() || m.update_url == origin {
                    m.update_url = url;
                }
                Import::Module(m)
            }
            other => other,
        })
    }

    fn prepare(
        &self,
        bytes: &[u8],
        origin: &str,
        replacement: Option<&str>,
        visited: &mut Vec<String>,
        depth: usize,
    ) -> ModuleResult<Import> {
        if depth > 4 || (!origin.is_empty() && visited.iter().any(|v| v == origin)) {
            return Err(ModuleError::at(
                Failure::InvalidDocument,
                Stage::Decode,
                "DESCRIPTOR_LOOP",
            ));
        }
        if package::is_package(bytes) {
            let pkg = package::read(bytes, &self.inner.app_version)?;
            let spec = EngineSpec {
                key: format!("inspect:{}", pkg.sha256),
                installation: String::new(),
                script: Arc::from(pkg.script.as_str()),
                metadata: pkg.manifest.metadata_json(),
                origin: origin.to_owned(),
                storage: Arc::new(ModuleStorage::new(
                    &self.inner.dir.join("inspect"),
                    &pkg.sha256,
                )),
                log: Arc::new(LogBuffer::default()),
            };
            Engines::inspect(&spec, Instant::now() + INIT_TIMEOUT)?;
            let replacement = replacement.map(str::to_owned).or_else(|| {
                lock(&self.inner.registry)
                    .modules
                    .iter()
                    .find(|m| m.manifest.id == pkg.manifest.id && m.origin == origin)
                    .map(|m| m.id.clone())
            });
            return Ok(Import::Module(Box::new(PendingModule {
                manifest: pkg.manifest,
                script: pkg.script,
                origin: origin.to_owned(),
                update_url: origin.to_owned(),
                sha256: pkg.sha256,
                replacement,
            })));
        }
        let text = String::from_utf8_lossy(bytes);
        let trimmed = text.trim_start_matches('\u{feff}').trim();
        let json: Option<serde_json::Map<String, Value>> = if trimmed.starts_with('{') {
            serde_json::from_str(trimmed).ok()
        } else {
            None
        };
        let Some(json) = json else {
            // A web page that links to a catalog index.
            if trimmed.starts_with('<') && !origin.is_empty() {
                let doc = catalog::read(origin, None)?;
                return Ok(Import::Catalog(SavedCatalog {
                    id: json::uuid_v4(),
                    name: doc.name,
                    url: doc.origin,
                    refreshed_at: now_ms(),
                    entries: doc.entries,
                }));
            }
            return Err(ModuleError::at(
                Failure::Unsupported,
                Stage::Decode,
                "UNSUPPORTED_DOCUMENT",
            ));
        };
        if catalog::is_catalog(&json) {
            let doc = catalog::read(origin, Some(trimmed))?;
            return Ok(Import::Catalog(SavedCatalog {
                id: json::uuid_v4(),
                name: doc.name,
                url: if origin.is_empty() {
                    String::new()
                } else {
                    doc.origin
                },
                refreshed_at: now_ms(),
                entries: doc.entries,
            }));
        }
        let value = Value::Object(json);
        let target = ["download", "download_url", "url"]
            .iter()
            .map(|k| json::opt_str(&value, k))
            .find(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                ModuleError::at(Failure::Unsupported, Stage::Decode, "UNSUPPORTED_DOCUMENT")
            })?;
        let next = if origin.is_empty() {
            web_url(target)?
        } else {
            resolve_url(target, origin)?
        };
        let (bytes, address) = http::source(&next)?;
        if !origin.is_empty() {
            visited.push(origin.to_owned());
        }
        let prepared = self.prepare(&bytes, &address, replacement, visited, depth + 1)?;
        Ok(match prepared {
            Import::Module(mut m) => {
                m.update_url = if origin.is_empty() {
                    address
                } else {
                    origin.to_owned()
                };
                Import::Module(m)
            }
            other => other,
        })
    }

    /// Installs (or replaces) a reviewed import. Returns the installation id
    /// of a module, or the catalog id.
    pub fn commit(&self, import: Import) -> ModuleResult<String> {
        match import {
            Import::Catalog(catalog) => {
                let mut id = catalog.id.clone();
                self.change(|r| {
                    match r.catalogs.iter_mut().find(|c| {
                        c.id == catalog.id || (!catalog.url.is_empty() && c.url == catalog.url)
                    }) {
                        Some(existing) => {
                            id = existing.id.clone();
                            *existing = SavedCatalog {
                                id: existing.id.clone(),
                                ..catalog
                            };
                        }
                        None => r.catalogs.push(catalog),
                    }
                    Ok(())
                })?;
                Ok(id)
            }
            Import::Module(pending) => {
                let existing = {
                    let registry = lock(&self.inner.registry);
                    pending
                        .replacement
                        .as_ref()
                        .and_then(|id| registry.modules.iter().find(|m| &m.id == id))
                        .or_else(|| {
                            registry.modules.iter().find(|m| {
                                m.manifest.id == pending.manifest.id
                                    && (m.origin == pending.origin
                                        || (!m.update_url.is_empty()
                                            && m.update_url == pending.update_url))
                            })
                        })
                        .cloned()
                };
                if let Some(existing) = &existing
                    && existing.manifest.id != pending.manifest.id
                {
                    return Err(ModuleError::at(
                        Failure::InvalidDocument,
                        Stage::Manifest,
                        "ARCH_IDENTITY",
                    ));
                }
                let id = existing
                    .as_ref()
                    .map_or_else(json::uuid_v4, |m| m.id.clone());
                let previous = existing
                    .as_ref()
                    .map(|m| self.values(m))
                    .unwrap_or_default();
                let mut configuration = BTreeMap::new();
                let mut secrets = Vec::new();
                for field in &pending.manifest.settings {
                    let value = previous
                        .get(&field.key)
                        .filter(|v| field.accepts(v))
                        .cloned()
                        .unwrap_or_else(|| field.default.clone());
                    if field.secret {
                        secrets.push((field.key.clone(), value));
                    } else {
                        configuration.insert(field.key.clone(), value);
                    }
                }
                let module = InstalledModule {
                    id: id.clone(),
                    manifest: pending.manifest,
                    script: pending.script,
                    origin: pending.origin,
                    update_url: pending.update_url,
                    enabled: existing.as_ref().is_none_or(|m| m.enabled),
                    configuration,
                    revision: json::uuid_v4(),
                    package_sha256: pending.sha256,
                    installed_at: now_ms(),
                };
                self.change(|r| {
                    match r.modules.iter_mut().find(|m| m.id == id) {
                        Some(slot) => *slot = module,
                        None => r.modules.push(module),
                    }
                    Ok(())
                })?;
                let mut cached = BTreeMap::new();
                for (key, value) in secrets {
                    let text = (!value.is_null()).then(|| value.to_string());
                    if let Err(e) = self
                        .inner
                        .secrets
                        .set(&secret_key(&id, &key), text.as_deref())
                    {
                        log::warn!("modules: storing a secret setting failed: {e}");
                    }
                    cached.insert(key, value);
                }
                lock(&self.inner.secret_values).insert(id.clone(), cached);
                self.inner.engines.unload(&id);
                lock(&self.inner.cache).retain(|(_, m), _| *m != id);
                Ok(id)
            }
        }
    }

    /// Fetches a catalog (does not save it; [`Self::commit`] does).
    pub fn fetch_catalog(&self, url: &str) -> ModuleResult<SavedCatalog> {
        let doc = catalog::read(&web_url(url)?, None)?;
        Ok(SavedCatalog {
            id: json::uuid_v4(),
            name: doc.name,
            url: doc.origin,
            refreshed_at: now_ms(),
            entries: doc.entries,
        })
    }

    /// Re-reads a saved catalog's entries.
    pub fn refresh_catalog(&self, id: &str) -> ModuleResult<()> {
        let saved = self
            .registry()
            .catalogs
            .into_iter()
            .find(|c| c.id == id)
            .ok_or_else(ModuleError::invalid)?;
        if saved.url.is_empty() {
            return Ok(());
        }
        let mut fresh = self.fetch_catalog(&saved.url)?;
        fresh.id = saved.id;
        fresh.name = saved.name;
        self.commit(Import::Catalog(fresh)).map(drop)
    }

    pub fn remove_catalog(&self, id: &str) -> ModuleResult<()> {
        self.change(|r| {
            r.catalogs.retain(|c| c.id != id);
            Ok(())
        })
    }

    /// Downloads a catalog entry's package for review (checked against its `sha256`).
    pub fn import_catalog_entry(&self, entry: &CatalogEntry) -> ModuleResult<Import> {
        let url = entry
            .download
            .as_deref()
            .ok_or_else(|| ModuleError::new(Failure::Unsupported))?;
        let replacement = lock(&self.inner.registry)
            .modules
            .iter()
            .find(|m| m.manifest.id == entry.id)
            .map(|m| m.id.clone());
        self.import_url_for(url, replacement.as_deref(), &entry.sha256)
    }

    /// Revisits a URL installation's import URL. `Ok(None)`: up to date, or a
    /// file installation without an update URL.
    pub fn check_update(&self, id: &str) -> ModuleResult<Option<PendingModule>> {
        let module = self.module(id).ok_or_else(ModuleError::invalid)?;
        if module.update_url.is_empty() {
            return Ok(None);
        }
        match self.import_url_for(&module.update_url, Some(id), "")? {
            Import::Module(pending) => Ok((pending.script != module.script
                || pending.manifest != module.manifest)
                .then_some(*pending)),
            Import::Catalog(_) => Err(ModuleError::invalid()),
        }
    }

    // ---- settings ----

    /// Setting values of an installation, secret ones included.
    pub fn settings(&self, id: &str) -> ModuleResult<BTreeMap<String, Value>> {
        let module = self.module(id).ok_or_else(ModuleError::invalid)?;
        Ok(self.values(&module))
    }

    fn values(&self, module: &InstalledModule) -> BTreeMap<String, Value> {
        let secret_fields: Vec<&SettingField> = module
            .manifest
            .settings
            .iter()
            .filter(|f| f.secret)
            .collect();
        let secrets = if secret_fields.is_empty() {
            BTreeMap::new()
        } else {
            let cached = lock(&self.inner.secret_values).get(&module.id).cloned();
            match cached {
                Some(values) => values,
                None => {
                    let values: BTreeMap<String, Value> = secret_fields
                        .iter()
                        .map(|f| {
                            let value = self
                                .inner
                                .secrets
                                .get(&secret_key(&module.id, &f.key))
                                .and_then(|text| serde_json::from_str(&text).ok())
                                .unwrap_or_else(|| f.default.clone());
                            (f.key.clone(), value)
                        })
                        .collect();
                    lock(&self.inner.secret_values).insert(module.id.clone(), values.clone());
                    values
                }
            }
        };
        module
            .manifest
            .settings
            .iter()
            .map(|f| {
                let value = if f.secret {
                    secrets.get(&f.key).cloned()
                } else {
                    module.configuration.get(&f.key).cloned()
                }
                .unwrap_or_else(|| f.default.clone());
                (f.key.clone(), value)
            })
            .collect()
    }

    /// Validates and saves setting values (missing keys get their defaults).
    /// A new revision: engines restart and cached resolutions are dropped.
    pub fn save_settings(&self, id: &str, values: &BTreeMap<String, Value>) -> ModuleResult<()> {
        let module = self.module(id).ok_or_else(ModuleError::invalid)?;
        let mut all = BTreeMap::new();
        for field in &module.manifest.settings {
            let value = values
                .get(&field.key)
                .cloned()
                .unwrap_or_else(|| field.default.clone());
            if !field.accepts(&value) {
                return Err(ModuleError::at(
                    Failure::InvalidConfiguration,
                    Stage::Manifest,
                    format!("SETTING_{}", field.key),
                ));
            }
            all.insert(field.key.clone(), value);
        }
        let previous = self.values(&module);
        let mut secrets = BTreeMap::new();
        for field in module.manifest.settings.iter().filter(|f| f.secret) {
            let value = all[&field.key].clone();
            if previous.get(&field.key) != Some(&value) {
                let text = (!value.is_null()).then(|| value.to_string());
                self.inner
                    .secrets
                    .set(&secret_key(id, &field.key), text.as_deref())
                    .map_err(|_| {
                        ModuleError::at(Failure::Execution, Stage::Storage, "SECRET_WRITE")
                    })?;
            }
            secrets.insert(field.key.clone(), value);
        }
        self.change(|r| {
            let m = r
                .modules
                .iter_mut()
                .find(|m| m.id == id)
                .ok_or_else(ModuleError::invalid)?;
            m.configuration = all
                .iter()
                .filter(|(k, _)| !module.manifest.setting(k).is_some_and(|f| f.secret))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            m.revision = json::uuid_v4();
            Ok(())
        })?;
        lock(&self.inner.secret_values).insert(id.to_owned(), secrets);
        self.inner.engines.unload(id);
        lock(&self.inner.cache).retain(|(_, m), _| m != id);
        Ok(())
    }

    fn validate(
        &self,
        module: &InstalledModule,
        values: &BTreeMap<String, Value>,
    ) -> ModuleResult<()> {
        module
            .manifest
            .require_compatible(&self.inner.app_version)?;
        for field in &module.manifest.settings {
            let value = values.get(&field.key).unwrap_or(&field.default);
            if !field.accepts(value) {
                return Err(ModuleError::at(
                    Failure::InvalidConfiguration,
                    Stage::Manifest,
                    format!("SETTING_{}", field.key),
                ));
            }
        }
        Ok(())
    }

    // ---- diagnostics ----

    /// "Check module": configuration + initialization (not proof of audio access).
    pub fn check(&self, id: &str) -> Diagnostic {
        let started = Instant::now();
        let Some(module) = self.module(id) else {
            return Diagnostic::failed(&ModuleError::invalid(), started, "");
        };
        let values = self.values(&module);
        let result = self
            .validate(&module, &values)
            .and_then(|()| Engines::inspect(&self.spec(&module), Instant::now() + INIT_TIMEOUT));
        let diagnostic = match result {
            Ok(()) => {
                self.invalidate_failures(id);
                Diagnostic::new(CheckStatus::Ready, started, &module.revision)
            }
            Err(e) => Diagnostic::failed(&e, started, &module.revision),
        };
        self.log(id)
            .push("info", &format!("check: {}", diagnostic.summary()));
        lock(&self.inner.checks).insert(id.to_owned(), diagnostic.clone());
        self.bump();
        diagnostic
    }

    pub fn health(&self, id: &str) -> ModuleHealth {
        ModuleHealth {
            check: lock(&self.inner.checks).get(id).cloned(),
            playback: lock(&self.inner.playback).get(id).cloned(),
            log: lock(&self.inner.logs)
                .get(id)
                .map(|l| l.lines())
                .unwrap_or_default(),
        }
    }

    pub fn clear_log(&self, id: &str) {
        if let Some(log) = lock(&self.inner.logs).get(id) {
            log.clear();
        }
        self.bump();
    }

    /// A plain-text report for "Copy diagnostics" (no settings, URLs or secrets).
    pub fn diagnostics_report(&self, id: &str) -> String {
        let Some(module) = self.module(id) else {
            return String::new();
        };
        let health = self.health(id);
        let mut out = format!(
            "{} ({}) {}\npackage sha256 {}\ncapabilities {}\n",
            module.manifest.title(),
            module.manifest.id,
            module.manifest.version,
            module.package_sha256,
            module.manifest.capabilities.join(", ")
        );
        if let Some(c) = &health.check {
            out.push_str(&format!("check: {}\n", c.summary()));
        }
        if let Some(p) = &health.playback {
            out.push_str(&format!("playback: {}\n", p.summary()));
        }
        out
    }

    fn record_playback(&self, id: &str, diagnostic: Diagnostic) {
        self.log(id).push(
            "info",
            &format!("playback: {} — {}", diagnostic.track, diagnostic.summary()),
        );
        lock(&self.inner.playback).insert(id.to_owned(), diagnostic);
        self.bump();
    }

    /// Forgets cached failures (after a check or configuration change).
    pub fn invalidate_failures(&self, id: &str) {
        lock(&self.inner.cache).retain(|(_, m), e| m != id || e.selection.is_some());
    }

    // ---- calling providers ----

    /// `searchTracks(query, 10, context)` on one module.
    pub fn search(&self, id: &str, query: &str) -> ModuleResult<Vec<ModuleTrack>> {
        let module = self.module(id).ok_or_else(ModuleError::invalid)?;
        self.search_module(&module, query, Instant::now() + self.module_timeout())
    }

    fn search_module(
        &self,
        module: &InstalledModule,
        query: &str,
        deadline: Instant,
    ) -> ModuleResult<Vec<ModuleTrack>> {
        let values = self.values(module);
        self.validate(module, &values)?;
        let args = serde_json::json!([query, SEARCH_LIMIT, call_context(&values, None)]);
        let value = self
            .inner
            .engines
            .call(&self.spec(module), "searchTracks", args, deadline)?;
        parse_search(&value)
    }

    /// `getTrackStreamUrl(trackId, quality, context)` on one module.
    pub fn stream(&self, id: &str, track: &ModuleTrack) -> ModuleResult<ModuleStream> {
        let module = self.module(id).ok_or_else(ModuleError::invalid)?;
        self.stream_module(&module, track, Instant::now() + self.module_timeout())
    }

    fn stream_module(
        &self,
        module: &InstalledModule,
        track: &ModuleTrack,
        deadline: Instant,
    ) -> ModuleResult<ModuleStream> {
        let values = self.values(module);
        self.validate(module, &values)?;
        let quality = values.get("quality").cloned().unwrap_or(Value::Null);
        let args = serde_json::json!([track.id, quality, call_context(&values, Some(track))]);
        let value =
            self.inner
                .engines
                .call(&self.spec(module), "getTrackStreamUrl", args, deadline)?;
        let stream = parse_stream(&value, &track.id)?;
        if stream.is_manifest() {
            return Err(ModuleError::at(
                Failure::Unsupported,
                Stage::Provider,
                "SEGMENTED_STREAM",
            ));
        }
        Ok(stream)
    }

    fn selection(
        &self,
        original_id: &str,
        module: &InstalledModule,
        candidate: &ModuleTrack,
        deadline: Instant,
    ) -> ModuleResult<Selection> {
        let stream = self.stream_module(module, candidate, deadline)?;
        let identity = [
            original_id,
            &module.id,
            &module.revision,
            &candidate.id,
            &stream.upstream_provider,
            &stream.upstream_id,
            &stream.variant,
            &stream.mime_type,
            &stream.codecs,
            &stream.bitrate.to_string(),
            &stream
                .sample_rate
                .map(|r| r.to_string())
                .unwrap_or_default(),
        ]
        .iter()
        .map(|part| format!("{}:{part}", part.len()))
        .collect::<String>();
        Ok(Selection {
            original_id: original_id.to_owned(),
            installation: module.id.clone(),
            module_name: module.manifest.title().to_owned(),
            candidate: candidate.clone(),
            stream,
            cache_key: format!("module:{}", json::sha256_hex(identity.as_bytes())),
        })
    }

    /// Drops the cached resolution of `video_id` by `installation` (its URL
    /// stopped working), so the next [`Self::resolve`] asks the module again.
    pub fn forget_resolution(&self, video_id: &str, installation: &str) {
        lock(&self.inner.cache).remove(&(video_id.to_owned(), installation.to_owned()));
    }

    /// Resolves a song through the enabled modules in priority order: search
    /// with its title and primary artist, pick the best acceptable match,
    /// resolve its stream. `None` when modules are off, the song isn't
    /// eligible (local files) or no module produced a stream. Outcomes are
    /// recorded as playback diagnostics; results are cached until shortly
    /// before the URL expires (misses for 30 s, failures for 10 s).
    pub fn resolve(&self, track: &Track) -> Option<Selection> {
        let registry = self.registry();
        if !registry.enabled || !registry.modules.iter().any(|m| m.enabled) {
            return None;
        }
        let target = MatchTarget::from_track(track);
        if !target.eligible
            || target.title.trim().is_empty()
            || target.artists.iter().all(|a| a.trim().is_empty())
        {
            return None;
        }
        let overall = Instant::now()
            + Duration::from_millis(self.inner.sequence_timeout_ms.load(Ordering::Relaxed));
        let _guard = lock(&self.inner.resolving);
        let label = format!("{} — {}", track.title, track.artists);
        for module in registry.modules.iter().filter(|m| m.enabled) {
            if Instant::now() >= overall {
                return None;
            }
            let key = (track.video_id.clone(), module.id.clone());
            {
                let cache = lock(&self.inner.cache);
                if let Some(entry) = cache.get(&key)
                    && entry.revision == module.revision
                    && entry.target == target
                    && entry.expires_at > now_ms()
                {
                    match &entry.selection {
                        Some(selection) => return Some(selection.clone()),
                        None => continue,
                    }
                }
            }
            let started = Instant::now();
            let deadline = (started + self.module_timeout()).min(overall);
            let mut phase = ResolutionPhase::Search;
            let mut candidates = 0usize;
            let outcome = (|| -> ModuleResult<Result<Selection, MatchRejection>> {
                let tracks = self.search_module(module, &matcher::query(&target), deadline)?;
                candidates = tracks.len();
                phase = ResolutionPhase::Match;
                match matcher::best_match(&target, &tracks) {
                    Ok(candidate) => {
                        phase = ResolutionPhase::Stream;
                        self.selection(&track.video_id, module, candidate, deadline)
                            .map(Ok)
                    }
                    Err(rejection) => Ok(Err(rejection)),
                }
            })();
            let (selection, diagnostic, keep_for) = match outcome {
                Ok(Ok(selection)) => {
                    let mut d = Diagnostic::new(CheckStatus::Resolved, started, &module.revision);
                    d.upstream = selection.stream.upstream_provider.clone();
                    d.variant = selection.stream.variant.clone();
                    let expires = selection.stream.expires_at_ms.saturating_sub(60_000);
                    (Some(selection), d, expires.saturating_sub(now_ms()))
                }
                Ok(Err(rejection)) => {
                    let mut d = Diagnostic::new(CheckStatus::NoMatch, started, &module.revision);
                    d.rejection = Some(rejection);
                    (None, d, 30_000)
                }
                Err(e) => (
                    None,
                    Diagnostic::failed(&e, started, &module.revision),
                    10_000,
                ),
            };
            let mut diagnostic = diagnostic;
            diagnostic.phase = Some(phase);
            diagnostic.candidates = candidates;
            diagnostic.track = label.clone();
            self.record_playback(&module.id, diagnostic);
            {
                let mut cache = lock(&self.inner.cache);
                cache.insert(
                    key,
                    CacheEntry {
                        revision: module.revision.clone(),
                        target: target.clone(),
                        selection: selection.clone(),
                        expires_at: now_ms() + keep_for,
                    },
                );
                while cache.len() > 256 {
                    let oldest = cache
                        .iter()
                        .min_by_key(|(_, e)| e.expires_at)
                        .map(|(k, _)| k.clone());
                    match oldest {
                        Some(k) => cache.remove(&k),
                        None => break,
                    };
                }
            }
            if selection.is_some() {
                return selection;
            }
        }
        None
    }
}

/// A [`StreamResolver`] that tries the enabled modules first (in priority
/// order) and falls back to `fallback` (YouTube) unless the user turned the
/// fallback off. Local files and modules switched off go straight to `fallback`.
pub struct ModuleResolver {
    modules: Modules,
    fallback: Arc<dyn StreamResolver>,
}

impl ModuleResolver {
    pub fn new(modules: Modules, fallback: Arc<dyn StreamResolver>) -> Self {
        Self { modules, fallback }
    }

    pub fn modules(&self) -> &Modules {
        &self.modules
    }
}

impl StreamResolver for ModuleResolver {
    fn resolve(&self, video_id: &str) -> crate::Result<ResolvedStream> {
        self.fallback.resolve(video_id)
    }

    fn resolve_with_quality(
        &self,
        video_id: &str,
        quality: Quality,
    ) -> crate::Result<ResolvedStream> {
        self.fallback.resolve_with_quality(video_id, quality)
    }

    /// A module stream is resolved again through the same module, past the
    /// cache (its URL stopped working); anything else goes to the fallback.
    fn refresh_track(
        &self,
        track: &Track,
        quality: Quality,
        stale: &ResolvedStream,
    ) -> crate::Result<ResolvedStream> {
        let module = stale
            .source
            .as_ref()
            .filter(|s| s.kind == SourceKind::Module)
            .and_then(|s| s.module_id.clone());
        let Some(module) = module else {
            return self.fallback.refresh_track(track, quality, stale);
        };
        self.modules.forget_resolution(&track.video_id, &module);
        match self.modules.resolve(track) {
            Some(selection) => Ok(selection.to_resolved()),
            None => Err(crate::Error::Unplayable(
                "the module no longer resolves this song".into(),
            )),
        }
    }

    /// YouTube resolves ahead for songs no module takes (modules keep their own cache).
    fn prefetch_track(&self, track: &Track, quality: Quality) {
        let enabled = {
            let r = lock(&self.modules.inner.registry);
            r.enabled && r.modules.iter().any(|m| m.enabled)
        };
        if !enabled || !MatchTarget::from_track(track).eligible {
            self.fallback.prefetch_track(track, quality);
        }
    }

    fn resolve_track(&self, track: &Track, quality: Quality) -> crate::Result<ResolvedStream> {
        let (enabled, fallback) = {
            let r = lock(&self.modules.inner.registry);
            (
                r.enabled && r.modules.iter().any(|m| m.enabled),
                r.fallback_to_youtube,
            )
        };
        if !enabled || !MatchTarget::from_track(track).eligible {
            return self.fallback.resolve_track(track, quality);
        }
        match self.modules.resolve(track) {
            Some(selection) => Ok(selection.to_resolved()),
            None if fallback => self.fallback.resolve_track(track, quality),
            None => Err(crate::Error::Unplayable(
                "no enabled module could play this song (YouTube fallback is off)".into(),
            )),
        }
    }
}
