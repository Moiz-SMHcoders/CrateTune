// Error model ported from ArchiveTune
// modules/src/main/kotlin/moe/rukamori/archivetune/modules/ModuleModels.kt (ModuleFailure,
// ModuleLoadStage, ModuleException) — © Rukamori and ArchiveTune contributors, GPL-3.0
//! Failures of module installation and execution.
//!
//! Like the reference implementation, errors carry a failure class, the stage
//! that failed and a short redacted diagnostic code (`ARCH_CHECKSUM`,
//! `HTTP_404`, `TypeError/module-provider.js:12`), never a provider's own
//! exception text or a URL, so they can be shown and copied safely.

use std::fmt;

/// What went wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Failure {
    /// A malformed package, manifest, catalog or result.
    InvalidDocument,
    /// Valid, but asks for something this host does not implement.
    Unsupported,
    /// Required settings missing or invalid.
    InvalidConfiguration,
    Network,
    Timeout,
    /// The provider script threw, or the engine failed.
    Execution,
    /// A checksum mismatch.
    Integrity,
}

/// Where it went wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Stage {
    Download,
    Decode,
    Engine,
    Host,
    Provider,
    Manifest,
    Storage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleError {
    pub failure: Failure,
    pub stage: Option<Stage>,
    /// Redacted diagnostic code, e.g. `ARCH_CHECKSUM`.
    pub code: Option<String>,
}

pub type ModuleResult<T> = std::result::Result<T, ModuleError>;

impl ModuleError {
    pub fn new(failure: Failure) -> Self {
        Self {
            failure,
            stage: None,
            code: None,
        }
    }

    pub fn at(failure: Failure, stage: Stage, code: impl Into<String>) -> Self {
        Self {
            failure,
            stage: Some(stage),
            code: Some(code.into()),
        }
    }

    pub(crate) fn with_stage(mut self, stage: Stage) -> Self {
        self.stage.get_or_insert(stage);
        self
    }

    /// `ARCH_<code>` package errors (invalid document, decode stage).
    pub(crate) fn arch(code: &str) -> Self {
        Self::at(
            Failure::InvalidDocument,
            Stage::Decode,
            format!("ARCH_{code}"),
        )
    }

    pub(crate) fn invalid() -> Self {
        Self::new(Failure::InvalidDocument)
    }

    /// "STAGE/CODE" for diagnostics.
    pub fn diagnostic(&self) -> String {
        let mut parts = Vec::new();
        if let Some(stage) = self.stage {
            parts.push(format!("{stage:?}").to_ascii_uppercase());
        }
        if let Some(code) = &self.code {
            parts.push(code.clone());
        }
        parts.join("/")
    }

    /// A sentence for the UI.
    pub fn describe(&self) -> String {
        let what = match self.failure {
            Failure::InvalidDocument => "Invalid module document",
            Failure::Unsupported => "Not supported by this app",
            Failure::InvalidConfiguration => "Module settings are incomplete or invalid",
            Failure::Network => "Network error",
            Failure::Timeout => "Timed out",
            Failure::Execution => "The module failed",
            Failure::Integrity => "Checksum mismatch",
        };
        let detail = self.diagnostic();
        if detail.is_empty() {
            what.to_owned()
        } else {
            format!("{what} ({detail})")
        }
    }
}

impl fmt::Display for ModuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.describe())
    }
}

impl std::error::Error for ModuleError {}

impl From<ModuleError> for crate::Error {
    fn from(e: ModuleError) -> Self {
        crate::Error::Unplayable(e.describe())
    }
}
