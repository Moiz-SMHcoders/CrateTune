//! Provider modules in the app: the shared [`Modules`] store (in
//! `<config dir>/modules`), the resolver the player and the download manager use
//! (enabled modules first, then YouTube), and the secret hook that keeps secret
//! module settings in the OS keychain.
//!
//! Module state lives in music-core's own `registry.json`, not in
//! `settings.json`; the Settings → Modules section talks to [`Modules`] directly
//! (always off the UI thread) and repaints when [`Modules::revision`] moves.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui::{App, Global};
use music_core::modules::{FileSecrets, ModuleResolver, Modules, SecretHook};
use music_core::youtube::{FixesStore, ResolverStatus, WebResolver};
use music_core::{IosClientResolver, StreamResolver};

use crate::secrets::{self, Backend};
use crate::settings::prefs::StreamSourceMode;

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
    /// Why `modules` is `None`: the (English, technical) error opening the store;
    /// `None` with no modules = there is no data directory.
    pub error: Option<String>,
    pub resolver: Arc<dyn StreamResolver>,
    /// Full-length YouTube streams (visionOS / TV / web + PoToken clients,
    /// preview fallback); the last resolver in the chain. Its clients follow
    /// Settings → Playback.
    pub youtube: Arc<WebResolver>,
    /// Bumped whenever [`WebResolver::status`] changes (Settings → Playback polls it).
    pub stream_revision: Arc<AtomicU64>,
}

impl Global for ModuleService {}

impl ModuleService {
    /// Opens the module store (fast: one small JSON file). Without a config
    /// directory the app plays from YouTube only.
    pub fn open(stream_source: StreamSourceMode, player_fixes: bool) -> Self {
        let config = crate::settings::file::config_dir();
        let cache = config.as_ref().map(|d| d.join("cache").join("youtube"));
        let web = Arc::new(WebResolver::new(
            vec![],
            music_core::youtube::platform_potoken_provider(),
            Some(Arc::new(IosClientResolver)),
            cache.clone(),
        ));
        web.set_clients(stream_source.clients(web.has_potoken()));
        let stream_revision = Arc::new(AtomicU64::new(0));
        let revision = stream_revision.clone();
        web.set_status_hook(move |status: &ResolverStatus| {
            if std::env::var_os("MUSIC_TRACE").is_some() {
                eprintln!("music-gpui: stream status {status:?}");
            }
            revision.fetch_add(1, Ordering::Relaxed);
        });
        // Reading the cached manifest is a small file; a fetch runs in the background.
        web.use_player_fixes(fixes_store(cache, player_fixes));
        let youtube: Arc<dyn StreamResolver> = web.clone();
        let Some(dir) = config.map(|d| d.join("modules")) else {
            return Self {
                modules: None,
                error: None,
                resolver: youtube,
                youtube: web,
                stream_revision,
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
                youtube: web,
                stream_revision,
            },
            Err(e) => {
                eprintln!("music-gpui: modules unavailable: {e}");
                Self {
                    modules: None,
                    error: Some(e.to_string()),
                    resolver: youtube,
                    youtube: web,
                    stream_revision,
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

    /// Applies Settings → Playback → YouTube streams.
    pub fn set_stream_source(cx: &App, mode: StreamSourceMode) {
        if let Some(s) = Self::global(cx) {
            s.youtube.set_clients(mode.clients(s.youtube.has_potoken()));
        }
    }

    /// Applies Settings → Playback → Player fixes (off: only the built-in ones).
    pub fn set_player_fixes(cx: &App, enabled: bool) {
        if let Some(s) = Self::global(cx)
            && PLAYER_FIXES_ON.swap(enabled, Ordering::Relaxed) != enabled
        {
            let cache =
                crate::settings::file::config_dir().map(|d| d.join("cache").join("youtube"));
            let web = s.youtube.clone();
            // A cached manifest is read (small); a check runs in the background.
            let _ = std::thread::Builder::new()
                .name("player-fixes".into())
                .spawn(move || web.use_player_fixes(fixes_store(cache, enabled)));
        }
    }

    /// The stream resolver's status (which client played last, which ones are
    /// skipped, which player fixes apply).
    pub fn stream_status(cx: &App) -> Option<(ResolverStatus, u64)> {
        Self::global(cx).map(|s| {
            (
                s.youtube.status(),
                s.stream_revision.load(Ordering::Relaxed),
            )
        })
    }

    /// Signed-in playback uses the account cookies (or none after sign-out).
    pub fn set_youtube_auth(cx: &App, auth: Option<music_core::innertube::Auth>) {
        if let Some(s) = Self::global(cx) {
            s.youtube.set_auth(auth);
        }
    }

    /// One-time slow work (player script, BotGuard) on a background thread, so the
    /// first song starts quickly. WebKit work is dispatched to the main thread, which
    /// must keep running its loop — never call the resolver from the main thread.
    pub fn prewarm(cx: &App) {
        if let Some(s) = Self::global(cx) {
            let web = s.youtube.clone();
            let _ = std::thread::Builder::new()
                .name("youtube-prewarm".into())
                .spawn(move || web.prewarm());
        }
    }
}

/// Whether fetched player fixes are on (mirrors the setting the resolver was given).
static PLAYER_FIXES_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Where the player-fixes manifest comes from: `packaging/player-fixes.json` on
/// the main branch of the app's repository, then the relay's mirror
/// (`https://<relay host>/player-fixes.json`). `MUSIC_PLAYER_FIXES_URL` replaces
/// both (tests). `None` when off, or without a cache directory.
fn fixes_store(cache: Option<std::path::PathBuf>, enabled: bool) -> Option<FixesStore> {
    PLAYER_FIXES_ON.store(enabled, Ordering::Relaxed);
    if !enabled {
        return None;
    }
    let urls = fixes_urls(
        std::env::var("MUSIC_PLAYER_FIXES_URL").ok().as_deref(),
        crate::identity::UPDATE_REPO,
        crate::identity::RELAY_URL,
    );
    let cache = cache?;
    (!urls.is_empty()).then(|| FixesStore::new(cache, urls))
}

fn fixes_urls(env: Option<&str>, repo: &str, relay: &str) -> Vec<String> {
    if let Some(url) = env.map(str::trim).filter(|u| !u.is_empty()) {
        return vec![url.to_owned()];
    }
    let mut urls = Vec::new();
    let repo = repo.trim();
    if !repo.is_empty() {
        urls.push(format!(
            "https://raw.githubusercontent.com/{repo}/main/packaging/player-fixes.json"
        ));
    }
    // wss://relay.example.com/… → https://relay.example.com/player-fixes.json
    if let Some(host) = relay
        .trim()
        .strip_prefix("wss://")
        .and_then(|rest| rest.split('/').next())
        .filter(|h| !h.is_empty())
    {
        urls.push(format!("https://{host}/player-fixes.json"));
    }
    urls
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

    #[test]
    fn player_fixes_sources() {
        assert_eq!(
            fixes_urls(None, "Owner/App", "wss://relay.example.com/x"),
            [
                "https://raw.githubusercontent.com/Owner/App/main/packaging/player-fixes.json",
                "https://relay.example.com/player-fixes.json"
            ]
        );
        assert_eq!(
            fixes_urls(None, "", "ws://lan:1"),
            Vec::<String>::new(),
            "no repo, no TLS relay: nothing"
        );
        assert_eq!(
            fixes_urls(Some("http://127.0.0.1:9/f.json"), "Owner/App", ""),
            ["http://127.0.0.1:9/f.json"]
        );
    }
}
