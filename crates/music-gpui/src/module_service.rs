//! Provider modules in the app: the shared [`Modules`] store (in
//! `<config dir>/modules`), the resolver the player and the download manager use
//! (enabled modules first, then YouTube), and the secret hook that keeps secret
//! module settings in the OS keychain.
//!
//! Module state lives in music-core's own `registry.json`, not in
//! `settings.json`; the Settings → Modules section talks to [`Modules`] directly
//! (always off the UI thread) and repaints when [`Modules::revision`] moves.

use std::sync::Arc;

use gpui::{App, Global};
use music_core::modules::{FileSecrets, ModuleResolver, Modules, SecretHook};
use music_core::{IosClientResolver, StreamResolver};

use crate::secrets::{self, Backend};

/// Secret module settings: the keychain (service `music-macos`, account
/// `module.<installation>.<key>`), else a 0600 file next to the registry.
struct KeychainSecrets {
    backend: Option<Box<dyn Backend>>,
    file: FileSecrets,
}

impl SecretHook for KeychainSecrets {
    fn get(&self, key: &str) -> Option<String> {
        if let Some(b) = &self.backend {
            match b.get(key) {
                Ok(Some(v)) => return Some(v),
                Ok(None) => {}
                Err(e) => eprintln!("music-gpui: keychain read failed ({e}); trying the file"),
            }
        }
        self.file.get(key)
    }

    fn set(&self, key: &str, value: Option<&str>) -> Result<(), String> {
        if let Some(b) = &self.backend {
            let result = match value {
                Some(v) => b.set(key, v),
                None => b.delete(key),
            };
            match result {
                Ok(()) => {
                    // Never leave a stale copy behind in the file.
                    let _ = self.file.set(key, None);
                    return Ok(());
                }
                Err(e) => eprintln!("music-gpui: keychain write failed ({e}); using the file"),
            }
        }
        self.file.set(key, value)
    }
}

/// The app's modules (a GPUI global).
#[derive(Clone)]
pub struct ModuleService {
    pub modules: Option<Modules>,
    pub error: Option<String>,
    pub resolver: Arc<dyn StreamResolver>,
}

impl Global for ModuleService {}

impl ModuleService {
    /// Opens the module store (fast: one small JSON file). Without a config
    /// directory the app plays from YouTube only.
    pub fn open() -> Self {
        let youtube: Arc<dyn StreamResolver> = Arc::new(IosClientResolver);
        let Some(dir) = crate::settings::file::config_dir().map(|d| d.join("modules")) else {
            return Self {
                modules: None,
                error: Some("No data directory; modules are unavailable.".into()),
                resolver: youtube,
            };
        };
        let hook = KeychainSecrets {
            backend: secrets::default_backend(),
            file: FileSecrets::new(dir.join("secrets.json")),
        };
        match Modules::open(&dir, Arc::new(hook), env!("CARGO_PKG_VERSION")) {
            Ok(modules) => Self {
                resolver: Arc::new(ModuleResolver::new(modules.clone(), youtube)),
                modules: Some(modules),
                error: None,
            },
            Err(e) => {
                eprintln!("music-gpui: modules unavailable: {e}");
                Self {
                    modules: None,
                    error: Some(format!("Modules unavailable: {e}")),
                    resolver: youtube,
                }
            }
        }
    }

    pub fn global(cx: &App) -> Option<&Self> {
        cx.try_global::<Self>()
    }

    pub fn modules(cx: &App) -> Option<Modules> {
        Self::global(cx).and_then(|s| s.modules.clone())
    }
}

/// "YouTube · AAC 128 kbps", "Echo · FLAC 16/44.1", "File · MP3 320 kbps".
pub fn source_label(source: Option<&music_core::StreamSource>) -> Option<String> {
    let source = source?;
    let provider = if source.provider.is_empty() {
        "YouTube"
    } else {
        source.provider.as_str()
    };
    Some(match source.quality_label() {
        Some(quality) => format!("{provider} · {quality}"),
        None => provider.to_owned(),
    })
}

impl crate::app::MusicApp {
    /// Script actions for the Modules section (see `script.rs`); `false` if unknown.
    pub fn module_script_action(
        &mut self,
        name: &str,
        arg: Option<&str>,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        use crate::settings::modules::SourcePreference;
        let panel = self.settings.clone();
        let arg_owned = arg.unwrap_or("").to_owned();
        let find =
            |which: &str, cx: &mut gpui::Context<Self>| panel.read(cx).modules_find(which, cx);
        match name {
            "modules" => match SourcePreference::parse(&arg_owned) {
                Some(p) => panel.update(cx, |p_, cx| p_.modules_set_source(p, window, cx)),
                None => eprintln!("music-gpui: @modules wants youtube|first|only"),
            },
            "module-install" => {
                if arg_owned.starts_with("http://") || arg_owned.starts_with("https://") {
                    panel.update(cx, |p, cx| {
                        p.modules_install_url(Some(arg_owned), window, cx)
                    });
                } else {
                    panel.update(cx, |p, cx| {
                        p.modules_import_path(arg_owned.into(), window, cx)
                    });
                }
            }
            "module-catalog" => panel.update(cx, |p, cx| {
                p.modules_add_catalog(Some(arg_owned), window, cx)
            }),
            "module-confirm" => panel.update(cx, |p, cx| p.modules_confirm(window, cx)),
            "module-cancel" => panel.update(cx, |p, cx| p.modules_cancel(cx)),
            "module-save" => panel.update(cx, |p, cx| p.modules_save(window, cx)),
            "module-field" => match arg_owned.split_once(':') {
                Some((key, value)) => {
                    let (key, value) = (key.to_owned(), value.to_owned());
                    panel.update(cx, |p, cx| p.modules_set_field(&key, &value, window, cx));
                }
                None => eprintln!("music-gpui: @module-field wants KEY:VALUE"),
            },
            "module-open" | "module-check" | "module-update" | "module-remove"
            | "module-enable" => {
                let (which, rest) = arg_owned
                    .split_once(':')
                    .unwrap_or((arg_owned.as_str(), ""));
                let Some(id) = find(if which.is_empty() { "0" } else { which }, cx) else {
                    eprintln!("music-gpui: no module {which:?}");
                    return true;
                };
                let on = rest != "off";
                panel.update(cx, |p, cx| match name {
                    "module-open" => p.modules_expand(id, window, cx),
                    "module-check" => p.modules_check(id, window, cx),
                    "module-update" => p.modules_update(id, window, cx),
                    "module-remove" => p.modules_remove(id, window, cx),
                    _ => p.modules_set_enabled(id, on, window, cx),
                });
            }
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use music_core::{SourceKind, StreamSource};

    #[test]
    fn labels() {
        assert_eq!(source_label(None), None);
        let yt = StreamSource::youtube(140);
        assert_eq!(source_label(Some(&yt)).unwrap(), "YouTube · AAC 128 kbps");
        let module = StreamSource {
            kind: SourceKind::Module,
            provider: "Echo".into(),
            codec: Some("FLAC".into()),
            sample_rate: Some(96_000),
            bits_per_sample: Some(24),
            ..Default::default()
        };
        assert_eq!(source_label(Some(&module)).unwrap(), "Echo · FLAC 24/96");
        let odd = StreamSource {
            provider: "X".into(),
            codec: Some("ALAC".into()),
            sample_rate: Some(44_100),
            ..Default::default()
        };
        assert_eq!(source_label(Some(&odd)).unwrap(), "X · ALAC 44.1 kHz");
    }
}
