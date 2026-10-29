//! Modules section: `.arch` provider modules (alternative audio sources).
//!
//! * Playback source: YouTube only / modules first, then YouTube / modules only.
//! * Add: install from a file, a direct package or descriptor URL, or a catalog
//!   URL. Every package is validated (and its script run once to check its
//!   exports) before a review card offers "Install".
//! * Installed modules in playback order: icon, name, version, author,
//!   capabilities, enable switch, move up / down, settings form generated from
//!   the manifest (text, password, number, toggle, select), check, update,
//!   diagnostics (last check, last playback outcome, the module's log, copy),
//!   uninstall.
//! * Catalogs: entries with Install / Update, refresh, remove.
//!
//! The section talks to `music_core::modules::Modules` (from
//! [`crate::module_service::ModuleService`]) only on worker threads; it repaints
//! when [`Modules::revision`] moves (checked twice a second while visible).
//! Module state is music-core's `modules/registry.json`, not `settings.json`;
//! secret settings go to the keychain.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, AppContext as _, ClipboardItem, Context, Entity, FontWeight, Hsla, Image,
    ImageFormat, IntoElement, ObjectFit, ParentElement as _, PathPromptOptions, SharedString,
    Styled as _, StyledImage as _, Task, Window, div, img, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    switch::Switch,
    tag::Tag,
    v_flex,
};
use music_core::modules::{
    CatalogEntry, Diagnostic, Failure, Import, InstalledModule, ModuleError, ModuleHealth,
    ModuleManifest, Modules, PendingModule, Registry, SettingField, SettingKind,
};
use serde_json::Value;

use super::SettingsPanel;
use super::widgets::{group, group_at, note, row, status_line};
use crate::icons::Lucide;
use crate::module_service::ModuleService;
use crate::util::off_thread;

/// How songs pick their audio source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourcePreference {
    YouTube,
    ModulesFirst,
    ModulesOnly,
}

impl SourcePreference {
    const ALL: [Self; 3] = [Self::YouTube, Self::ModulesFirst, Self::ModulesOnly];

    fn title(self) -> &'static str {
        match self {
            Self::YouTube => t!("modules.source.youtube"),
            Self::ModulesFirst => t!("modules.source.first"),
            Self::ModulesOnly => t!("modules.source.only"),
        }
    }

    fn of(registry: &Registry) -> Self {
        match (registry.enabled, registry.fallback_to_youtube) {
            (false, _) => Self::YouTube,
            (true, true) => Self::ModulesFirst,
            (true, false) => Self::ModulesOnly,
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "youtube" | "off" => Some(Self::YouTube),
            "first" | "on" | "modules" => Some(Self::ModulesFirst),
            "only" => Some(Self::ModulesOnly),
            _ => None,
        }
    }
}

enum FormValue {
    Input(Entity<InputState>),
    Toggle(bool),
    Select(Value),
}

struct FormField {
    field: SettingField,
    value: FormValue,
}

/// The open settings form of one installation.
struct Form {
    id: String,
    fields: Vec<FormField>,
    error: Option<SharedString>,
}

/// The section's state, owned by the panel.
pub struct ModulesState {
    url: Entity<InputState>,
    catalog_url: Entity<InputState>,
    busy: Option<SharedString>,
    /// `(is_error, text)` of the last action.
    status: Option<(bool, SharedString)>,
    pending: Option<Box<PendingModule>>,
    expanded: Option<String>,
    form: Option<Form>,
    confirm_remove: Option<String>,
    snapshot: Registry,
    health: HashMap<String, ModuleHealth>,
    seen: u64,
    icons: HashMap<String, Arc<Image>>,
    /// Per installation: the outcome of "Check for update" / "Check".
    notes: HashMap<String, (bool, SharedString)>,
    poll: Option<Task<()>>,
}

impl ModulesState {
    pub fn new(window: &mut Window, cx: &mut Context<SettingsPanel>) -> Self {
        let input = |placeholder: &'static str, window: &mut Window, cx: &mut Context<_>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        Self {
            url: input("https://example.org/provider.arch", window, cx),
            catalog_url: input("https://example.org/modules/catalog.json", window, cx),
            busy: None,
            status: None,
            pending: None,
            expanded: None,
            form: None,
            confirm_remove: None,
            snapshot: Registry::default(),
            health: HashMap::new(),
            seen: 0,
            icons: HashMap::new(),
            notes: HashMap::new(),
            poll: None,
        }
    }

    /// Re-reads the registry when it changed.
    fn refresh(&mut self, modules: &Modules) {
        let revision = modules.revision();
        if revision == self.seen {
            return;
        }
        self.seen = revision;
        self.snapshot = modules.registry();
        // Scripts aren't needed to draw the list.
        for m in &mut self.snapshot.modules {
            m.script = String::new();
        }
        self.health = self
            .snapshot
            .modules
            .iter()
            .map(|m| (m.id.clone(), modules.health(&m.id)))
            .collect();
    }

    fn icon(&mut self, key: &str, manifest: &ModuleManifest) -> Option<Arc<Image>> {
        let bytes = manifest.icon.as_ref()?;
        Some(
            self.icons
                .entry(key.to_owned())
                .or_insert_with(|| Arc::new(Image::from_bytes(ImageFormat::Png, bytes.clone())))
                .clone(),
        )
    }
}

/// What failed, in the UI language (music-core's `ModuleError::describe` is English).
fn failure_text(failure: Failure) -> &'static str {
    match failure {
        Failure::InvalidDocument => t!("modules.failure.invalid_document"),
        Failure::Unsupported => t!("modules.failure.unsupported"),
        Failure::InvalidConfiguration => t!("modules.failure.invalid_configuration"),
        Failure::Network => t!("modules.failure.network"),
        Failure::Timeout => t!("modules.failure.timeout"),
        Failure::Execution => t!("modules.failure.execution"),
        Failure::Integrity => t!("modules.failure.integrity"),
    }
}

/// The failure plus its diagnostic code (`DECODE/ARCH_CHECKSUM`, kept as is).
fn error_text(e: &ModuleError) -> SharedString {
    let what = failure_text(e.failure);
    let code = e.diagnostic();
    if code.is_empty() {
        what.into()
    } else {
        t!("modules.error_with_code", error = what, code = code).into()
    }
}

/// A one-line summary of a check / playback outcome, like music-core's
/// `Diagnostic::summary` (which stays English for copied reports).
fn summary(d: &Diagnostic) -> String {
    use music_core::modules::{CheckStatus, MatchRejection, ResolutionPhase};
    let status = match d.status {
        CheckStatus::Ready => t!("modules.status.ready").to_owned(),
        CheckStatus::Configuration => t!("modules.status.configuration").to_owned(),
        CheckStatus::Unavailable => t!("modules.status.unavailable").to_owned(),
        CheckStatus::Failed => t!("modules.status.failed").to_owned(),
        CheckStatus::NoMatch => match d.rejection {
            Some(MatchRejection::NoResults) | None => t!("modules.status.no_results").to_owned(),
            Some(r) => {
                let reason = match r {
                    MatchRejection::NoResults | MatchRejection::Metadata => {
                        t!("modules.rejection.metadata")
                    }
                    MatchRejection::Title => t!("modules.rejection.title"),
                    MatchRejection::Artist => t!("modules.rejection.artist"),
                    MatchRejection::Recording => t!("modules.rejection.recording"),
                    MatchRejection::Duration => t!("modules.rejection.duration"),
                    MatchRejection::Explicit => t!("modules.rejection.explicit"),
                };
                t!("modules.status.no_match", reason = reason)
            }
        },
        CheckStatus::Resolved => t!("modules.status.resolved").to_owned(),
        CheckStatus::Timeout => t!("modules.status.timeout").to_owned(),
    };
    let mut parts = vec![status];
    if let Some(phase) = d.phase {
        parts.push(
            match phase {
                ResolutionPhase::Search => t!("modules.phase.search"),
                ResolutionPhase::Match => t!("modules.phase.match"),
                ResolutionPhase::Stream => t!("modules.phase.stream"),
            }
            .to_owned(),
        );
    }
    if d.candidates > 0 {
        parts.push(tp!("modules.candidates", d.candidates));
    }
    if !d.variant.is_empty() {
        parts.push(d.variant.clone());
    }
    if !d.code.is_empty() {
        parts.push(d.code.clone());
    }
    parts.push(t!(
        "modules.elapsed_ms",
        ms = crate::i18n::format::number(d.elapsed_ms)
    ));
    parts.join(" · ")
}

/// "5 min ago" for a Unix-milliseconds time.
fn ago(ms: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    crate::i18n::format::ago(now.saturating_sub(ms) / 1000)
}

impl SettingsPanel {
    /// Runs blocking module work on a worker thread, then `done` on the UI thread.
    fn modules_run<T: Send + 'static>(
        &mut self,
        label: &'static str,
        work: impl FnOnce(Modules) -> T + Send + 'static,
        done: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(modules) = ModuleService::modules(cx) else {
            self.modules.status = Some((true, t!("modules.unavailable").into()));
            cx.notify();
            return;
        };
        self.modules.busy = Some(label.into());
        cx.spawn_in(window, async move |this, cx| {
            let result = off_thread(move || work(modules)).await;
            this.update_in(cx, |this, window, cx| {
                this.modules.busy = None;
                done(this, result, window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn modules_report(&mut self, error: bool, text: impl Into<SharedString>) {
        self.modules.status = Some((error, text.into()));
    }

    /// Shows a module for review, or saves a catalog right away.
    fn modules_imported(
        &mut self,
        result: Result<Import, ModuleError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(Import::Module(pending)) => {
                self.modules.status = None;
                self.modules.pending = Some(pending);
            }
            Ok(Import::Catalog(catalog)) => {
                let name = catalog.name.clone();
                let count = catalog.entries.len();
                self.modules_run(
                    t!("modules.busy.saving_catalog"),
                    move |m| m.commit(Import::Catalog(catalog)),
                    move |this, r, _, _| match r {
                        Ok(_) => this.modules_report(
                            false,
                            tp!("modules.catalog_added", count, name = name),
                        ),
                        Err(e) => this.modules_report(true, error_text(&e)),
                    },
                    window,
                    cx,
                );
            }
            Err(e) => self.modules_report(true, error_text(&e)),
        }
    }

    pub fn modules_set_source(
        &mut self,
        preference: SourcePreference,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modules_run(
            t!("modules.busy.saving"),
            move |m| {
                m.set_enabled(preference != SourcePreference::YouTube)?;
                m.set_fallback_to_youtube(preference != SourcePreference::ModulesOnly)
            },
            |this, r, _, _| {
                if let Err(e) = r {
                    this.modules_report(true, error_text(&e));
                }
            },
            window,
            cx,
        );
    }

    pub fn modules_install_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(t!("common.install").into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = picked.await else {
                return;
            };
            let Some(path) = paths.pop() else { return };
            this.update_in(cx, |this, window, cx| {
                this.modules_import_path(path, window, cx)
            })
            .ok();
        })
        .detach();
    }

    /// Validates a package / descriptor / catalog file for review.
    pub fn modules_import_path(
        &mut self,
        path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modules_run(
            t!("modules.busy.checking_package"),
            move |m| m.import_file(&path),
            |this, r, window, cx| this.modules_imported(r, window, cx),
            window,
            cx,
        );
    }

    pub fn modules_install_url(
        &mut self,
        url: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let url = url.unwrap_or_else(|| self.modules.url.read(cx).value().trim().to_owned());
        if url.is_empty() {
            self.modules_report(true, t!("modules.url_missing"));
            cx.notify();
            return;
        }
        self.modules_run(
            t!("common.downloading"),
            move |m| m.import_url(&url),
            |this, r, window, cx| this.modules_imported(r, window, cx),
            window,
            cx,
        );
    }

    pub fn modules_add_catalog(
        &mut self,
        url: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let url =
            url.unwrap_or_else(|| self.modules.catalog_url.read(cx).value().trim().to_owned());
        if url.is_empty() {
            self.modules_report(true, t!("modules.catalog_url_missing"));
            cx.notify();
            return;
        }
        self.modules_run(
            t!("modules.busy.reading_catalog"),
            move |m| m.fetch_catalog(&url),
            |this, r, window, cx| this.modules_imported(r.map(Import::Catalog), window, cx),
            window,
            cx,
        );
    }

    /// Installs the module under review.
    pub fn modules_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.modules.pending.take() else {
            return;
        };
        let name = pending.manifest.title().to_owned();
        let needs_setup = pending
            .manifest
            .settings
            .iter()
            .any(|f| f.required && f.default.is_null());
        self.modules_run(
            t!("modules.busy.installing"),
            move |m| m.commit(Import::Module(pending)),
            move |this, r, window, cx| match r {
                Ok(id) => {
                    this.modules_report(
                        false,
                        if needs_setup {
                            t!("modules.installed_needs_setup", name = name)
                        } else {
                            t!("modules.installed", name = name)
                        },
                    );
                    if needs_setup {
                        this.modules_expand(id, window, cx);
                    }
                }
                Err(e) => this.modules_report(true, error_text(&e)),
            },
            window,
            cx,
        );
    }

    pub fn modules_cancel(&mut self, cx: &mut Context<Self>) {
        self.modules.pending = None;
        cx.notify();
    }

    pub fn modules_set_enabled(
        &mut self,
        id: String,
        on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modules_run(
            t!("modules.busy.saving"),
            move |m| m.set_module_enabled(&id, on),
            |this, r, _, _| {
                if let Err(e) = r {
                    this.modules_report(true, error_text(&e));
                }
            },
            window,
            cx,
        );
    }

    fn modules_move(
        &mut self,
        id: String,
        delta: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modules_run(
            t!("modules.busy.saving"),
            move |m| m.move_module(&id, delta),
            |_, _, _, _| {},
            window,
            cx,
        );
    }

    pub fn modules_remove(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.modules.confirm_remove.as_deref() != Some(id.as_str()) {
            self.modules.confirm_remove = Some(id);
            cx.notify();
            return;
        }
        self.modules.confirm_remove = None;
        if self.modules.expanded.as_deref() == Some(id.as_str()) {
            self.modules.expanded = None;
            self.modules.form = None;
        }
        self.modules_run(
            t!("modules.busy.removing"),
            move |m| m.remove(&id),
            |this, r, _, _| match r {
                Ok(()) => this.modules_report(false, t!("modules.removed")),
                Err(e) => this.modules_report(true, error_text(&e)),
            },
            window,
            cx,
        );
    }

    pub fn modules_check(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let key = id.clone();
        self.modules_run(
            t!("modules.busy.checking"),
            move |m| m.check(&id),
            move |this, d: Diagnostic, _, _| {
                let ok = d.status == music_core::modules::CheckStatus::Ready;
                this.modules.notes.insert(key, (!ok, summary(&d).into()));
            },
            window,
            cx,
        );
    }

    pub fn modules_update(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let key = id.clone();
        self.modules_run(
            t!("modules.busy.checking_update"),
            move |m| m.check_update(&id),
            move |this, r, _, _| match r {
                Ok(Some(pending)) => {
                    this.modules.notes.insert(
                        key,
                        (
                            false,
                            t!(
                                "modules.update_available",
                                version = pending.manifest.version
                            )
                            .into(),
                        ),
                    );
                    this.modules.pending = Some(Box::new(pending));
                }
                Ok(None) => {
                    let has_url = this
                        .modules
                        .snapshot
                        .modules
                        .iter()
                        .any(|m| m.id == key && !m.update_url.is_empty());
                    let text = if has_url {
                        t!("modules.up_to_date")
                    } else {
                        t!("modules.update_from_file")
                    };
                    this.modules.notes.insert(key, (false, text.into()));
                }
                Err(e) => {
                    this.modules.notes.insert(key, (true, error_text(&e)));
                }
            },
            window,
            cx,
        );
    }

    /// Opens (or closes) a module's settings + diagnostics.
    pub fn modules_expand(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.modules.expanded.as_deref() == Some(id.as_str()) {
            self.modules.expanded = None;
            self.modules.form = None;
            cx.notify();
            return;
        }
        self.modules.expanded = Some(id.clone());
        self.modules.form = None;
        let key = id.clone();
        self.modules_run(
            t!("modules.busy.loading_settings"),
            move |m| {
                let module = m.module(&id);
                let values = m.settings(&id);
                (module, values)
            },
            move |this, (module, values), window, cx| {
                let (Some(module), Ok(values)) = (module, values) else {
                    this.modules_report(true, t!("modules.settings_read_failed"));
                    return;
                };
                if this.modules.expanded.as_deref() == Some(key.as_str()) {
                    this.modules.form = Some(build_form(&module, &values, window, cx));
                }
            },
            window,
            cx,
        );
    }

    /// Script hook: types into (or toggles / selects) a field of the open form.
    pub fn modules_set_field(
        &mut self,
        key: &str,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(form) = &mut self.modules.form else {
            eprintln!("music-gpui: no module settings open");
            return;
        };
        let Some(f) = form.fields.iter_mut().find(|f| f.field.key == key) else {
            eprintln!("music-gpui: no module setting {key}");
            return;
        };
        match &mut f.value {
            FormValue::Input(input) => {
                let value = value.to_owned();
                input.update(cx, |i, cx| i.set_value(value, window, cx));
            }
            FormValue::Toggle(on) => *on = matches!(value, "on" | "true" | "1"),
            FormValue::Select(selected) => {
                if let Some(option) = f.field.options.iter().find(|o| {
                    o.label == value
                        || o.value.as_str() == Some(value)
                        || o.value == Value::String(value.to_owned())
                        || o.value.to_string().as_str() == value
                }) {
                    *selected = option.value.clone();
                }
            }
        }
        cx.notify();
    }

    pub fn modules_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &mut self.modules.form else {
            return;
        };
        let mut values = BTreeMap::new();
        for f in &form.fields {
            let value = match &f.value {
                FormValue::Toggle(on) => Value::Bool(*on),
                FormValue::Select(v) => v.clone(),
                FormValue::Input(input) => {
                    let text = input.read(cx).value().to_string();
                    let trimmed = text.trim();
                    match f.field.kind {
                        _ if trimmed.is_empty() => Value::Null,
                        SettingKind::Number => match trimmed.parse::<f64>() {
                            Ok(n) if f.field.integer && n.fract() == 0.0 => Value::from(n as i64),
                            Ok(n) => {
                                serde_json::Number::from_f64(n).map_or(Value::Null, Value::Number)
                            }
                            Err(_) => {
                                form.error = Some(
                                    t!("modules.field.not_a_number", field = f.field.label).into(),
                                );
                                cx.notify();
                                return;
                            }
                        },
                        SettingKind::Password => Value::String(text),
                        _ => Value::String(trimmed.to_owned()),
                    }
                }
            };
            if !f.field.accepts(&value) {
                form.error = Some(
                    if value.is_null() {
                        t!("modules.field.required", field = f.field.label)
                    } else {
                        t!("modules.field.invalid", field = f.field.label)
                    }
                    .into(),
                );
                cx.notify();
                return;
            }
            values.insert(f.field.key.clone(), value);
        }
        form.error = None;
        let id = form.id.clone();
        self.modules_run(
            t!("modules.busy.saving_settings"),
            move |m| m.save_settings(&id, &values),
            |this, r, _, _| match r {
                Ok(()) => this.modules_report(false, t!("modules.settings_saved")),
                Err(e) => {
                    if let Some(form) = &mut this.modules.form {
                        form.error = Some(error_text(&e));
                    }
                }
            },
            window,
            cx,
        );
    }

    fn modules_install_entry(
        &mut self,
        entry: CatalogEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modules_run(
            t!("common.downloading"),
            move |m| m.import_catalog_entry(&entry),
            |this, r, window, cx| this.modules_imported(r, window, cx),
            window,
            cx,
        );
    }

    fn modules_refresh_catalog(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.modules_run(
            t!("modules.busy.refreshing"),
            move |m| m.refresh_catalog(&id),
            |this, r, _, _| match r {
                Ok(()) => this.modules_report(false, t!("modules.catalog_refreshed")),
                Err(e) => this.modules_report(true, error_text(&e)),
            },
            window,
            cx,
        );
    }

    fn modules_remove_catalog(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.modules_run(
            t!("modules.busy.removing"),
            move |m| m.remove_catalog(&id),
            |_, _, _, _| {},
            window,
            cx,
        );
    }

    /// Script hook: the installation id of the `n`th module (playback order).
    pub fn modules_nth(&self, n: usize) -> Option<String> {
        self.modules.snapshot.modules.get(n).map(|m| m.id.clone())
    }

    /// Script hook: an installation id by index or manifest id.
    pub fn modules_find(&self, which: &str, cx: &gpui::App) -> Option<String> {
        if let Ok(n) = which.parse::<usize>() {
            return self.modules_nth(n).or_else(|| {
                ModuleService::modules(cx)
                    .and_then(|m| m.registry().modules.get(n).map(|m| m.id.clone()))
            });
        }
        ModuleService::modules(cx).and_then(|m| {
            m.registry()
                .modules
                .iter()
                .find(|m| m.manifest.id == which || m.id == which)
                .map(|m| m.id.clone())
        })
    }

    /// Keeps the section current while it is visible.
    fn modules_poll(&mut self, cx: &mut Context<Self>) {
        if self.modules.poll.is_some() {
            return;
        }
        let executor = cx.background_executor().clone();
        self.modules.poll = Some(cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(500)).await;
                let keep = this
                    .update(cx, |this, cx| {
                        if this.section != super::SectionId::Modules {
                            this.modules.poll = None;
                            return false;
                        }
                        if ModuleService::modules(cx)
                            .is_some_and(|m| m.revision() != this.modules.seen)
                        {
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        }));
    }
}

fn build_form(
    module: &InstalledModule,
    values: &BTreeMap<String, Value>,
    window: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> Form {
    let fields = module
        .manifest
        .settings
        .iter()
        .map(|field| {
            let current = values.get(&field.key).cloned().unwrap_or(Value::Null);
            let value = match field.kind {
                SettingKind::Boolean => FormValue::Toggle(current.as_bool().unwrap_or(false)),
                SettingKind::Select => FormValue::Select(current),
                SettingKind::Text | SettingKind::Password | SettingKind::Number => {
                    let text = match &current {
                        Value::Null => String::new(),
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    let masked = field.kind == SettingKind::Password;
                    let placeholder = match (field.kind, field.min, field.max) {
                        (SettingKind::Number, Some(a), Some(b)) => format!("{a} – {b}"),
                        _ if field.required => t!("modules.field.placeholder_required").into(),
                        _ => t!("modules.field.placeholder_optional").into(),
                    };
                    FormValue::Input(cx.new(|cx| {
                        InputState::new(window, cx)
                            .placeholder(placeholder)
                            .masked(masked)
                            .default_value(text)
                    }))
                }
            };
            FormField {
                field: field.clone(),
                value,
            }
        })
        .collect();
    Form {
        id: module.id.clone(),
        fields,
        error: None,
    }
}

fn chips(items: impl IntoIterator<Item = String>) -> impl IntoElement {
    h_flex().gap_1().flex_wrap().children(
        items
            .into_iter()
            .map(|c| Tag::secondary().small().rounded_full().child(c)),
    )
}

fn icon_box(icon: Option<Arc<Image>>, size: f32, cx: &gpui::App) -> AnyElement {
    match icon {
        Some(image) => img(image)
            .size(px(size))
            .flex_none()
            .rounded(px(size * 0.2))
            .object_fit(ObjectFit::Contain)
            .into_any_element(),
        None => div()
            .size(px(size))
            .flex_none()
            .rounded(px(size * 0.2))
            .bg(cx.theme().muted)
            .flex()
            .items_center()
            .justify_center()
            .child(gpui_component::Icon::new(Lucide::Puzzle).small())
            .into_any_element(),
    }
}

fn identity_line(manifest: &ModuleManifest) -> String {
    let mut parts = vec![format!("v{}", manifest.version)];
    if !manifest.author.trim().is_empty() {
        parts.push(manifest.author.clone());
    }
    parts.push(manifest.id.clone());
    parts.join(" · ")
}

fn diagnostic_line(label: &str, d: &Diagnostic, color: Hsla) -> impl IntoElement {
    let mut text = t!(
        "modules.diagnostic_line",
        label = label,
        when = ago(d.at),
        summary = summary(d)
    );
    if !d.track.is_empty() {
        text = t!("modules.diagnostic_track", line = text, track = d.track);
    }
    status_line(text.into(), color)
}

pub(super) fn render(
    panel: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let service = ModuleService::global(cx).cloned();
    let Some(modules) = service.as_ref().and_then(|s| s.modules.clone()) else {
        let error: SharedString = match service.map(|s| s.error) {
            Some(Some(e)) => t!("modules.unavailable_error", error = e).into(),
            Some(None) => t!("modules.no_data_dir").into(),
            None => t!("modules.unavailable").into(),
        };
        return note(error, cx).into_any_element();
    };
    panel.modules_poll(cx);
    panel.modules.refresh(&modules);
    let theme = cx.theme();
    let (muted, danger, success, warning) = (
        theme.muted_foreground,
        theme.danger,
        theme.success,
        theme.warning,
    );
    let border = theme.border;
    let preference = SourcePreference::of(&panel.modules.snapshot);
    let busy = panel.modules.busy.is_some();

    // ---- playback source ----
    let source_buttons = h_flex()
        .gap_1()
        .flex_wrap()
        .children(SourcePreference::ALL.map(|p| {
            let selected = p == preference;
            Button::new(SharedString::from(format!("module-source-{p:?}")))
                .small()
                .map(|b| if selected { b.primary() } else { b.outline() })
                .selected(selected)
                .label(p.title())
                .on_click(
                    cx.listener(move |this, _, window, cx| this.modules_set_source(p, window, cx)),
                )
        }));
    let enabled_count = panel
        .modules
        .snapshot
        .modules
        .iter()
        .filter(|m| m.enabled)
        .count();
    let source_note = match preference {
        SourcePreference::YouTube => t!("modules.source_note.youtube").to_owned(),
        SourcePreference::ModulesFirst => tp!("modules.source_note.first", enabled_count),
        SourcePreference::ModulesOnly => tp!("modules.source_note.only", enabled_count),
    };

    // ---- add ----
    let status = panel
        .modules
        .busy
        .clone()
        .map(|b| (false, b))
        .or(panel.modules.status.clone());
    let add =
        group_at("modules.add", t!("modules.add.group"), cx)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("module-install-file")
                            .small()
                            .outline()
                            .icon(Lucide::FolderOpen)
                            .label(t!("modules.add.from_file"))
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.modules_install_file(window, cx)
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(note(t!("modules.add.from_file_note"), cx)),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&panel.modules.url).small()),
                    )
                    .child(
                        Button::new("module-install-url")
                            .small()
                            .outline()
                            .icon(Lucide::Link)
                            .label(t!("modules.add.from_url"))
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.modules_install_url(None, window, cx)
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&panel.modules.catalog_url).small()),
                    )
                    .child(
                        Button::new("module-add-catalog")
                            .small()
                            .outline()
                            .icon(Lucide::LibraryBig)
                            .label(t!("modules.add.catalog"))
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.modules_add_catalog(None, window, cx)
                            })),
                    ),
            )
            .when_some(status, |this, (error, text)| {
                this.child(status_line(
                    text,
                    if error {
                        danger
                    } else if busy {
                        muted
                    } else {
                        success
                    },
                ))
            })
            .child(note(t!("modules.add.note"), cx));

    // ---- review ----
    let review = panel.modules.pending.as_ref().map(|pending| {
        let manifest = pending.manifest.clone();
        let icon = manifest
            .icon
            .clone()
            .map(|b| Arc::new(Image::from_bytes(ImageFormat::Png, b)));
        let replaces = pending.replacement.as_ref().and_then(|id| {
            panel
                .modules
                .snapshot
                .modules
                .iter()
                .find(|m| &m.id == id)
                .map(|m| m.manifest.version.clone())
        });
        let required: Vec<String> = manifest
            .settings
            .iter()
            .filter(|f| f.required)
            .map(|f| f.label.clone())
            .collect();
        group(t!("modules.review.group"), cx)
            .child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .child(icon_box(icon, 48., cx))
                    .child(
                        v_flex()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(manifest.title().to_owned()),
                            )
                            .child(note(identity_line(&manifest), cx))
                            .when(!manifest.description.is_empty(), |this| {
                                this.child(div().text_sm().child(manifest.description.clone()))
                            })
                            .child(chips(manifest.capabilities.clone()))
                            .when(!manifest.labels.is_empty(), |this| {
                                this.child(chips(manifest.labels.clone()))
                            }),
                    ),
            )
            .child(note(
                match &replaces {
                    Some(v) => t!("modules.review.replaces", version = v),
                    None => t!("modules.review.new").into(),
                },
                cx,
            ))
            .when(!required.is_empty(), |this| {
                this.child(note(
                    t!("modules.review.needs", settings = required.join(", ")),
                    cx,
                ))
            })
            .when(!manifest.homepage.is_empty(), |this| {
                this.child(note(
                    t!("modules.review.homepage", url = manifest.homepage),
                    cx,
                ))
            })
            .when(!manifest.license.is_empty(), |this| {
                this.child(note(
                    t!("modules.review.license", license = manifest.license),
                    cx,
                ))
            })
            .child(note(t!("modules.review.sha256", hash = pending.sha256), cx))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("module-confirm")
                            .small()
                            .primary()
                            .label(if replaces.is_some() {
                                t!("common.update")
                            } else {
                                t!("common.install")
                            })
                            .disabled(busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.modules_confirm(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("module-cancel")
                            .small()
                            .outline()
                            .label(t!("common.cancel"))
                            .on_click(cx.listener(|this, _, _, cx| this.modules_cancel(cx))),
                    ),
            )
    });

    // ---- installed ----
    let installed: Vec<InstalledModule> = panel.modules.snapshot.modules.clone();
    let count = installed.len();
    let mut list = group_at(
        "modules.installed",
        t!("modules.installed_group", count = count),
        cx,
    );
    if installed.is_empty() {
        list = list.child(note(t!("modules.nothing_installed"), cx));
    }
    for (index, module) in installed.iter().enumerate() {
        let id = module.id.clone();
        let icon = panel.modules.icon(
            &format!("{}:{}", module.id, module.revision),
            &module.manifest,
        );
        let health = panel.modules.health.get(&id).cloned().unwrap_or_default();
        let expanded = panel.modules.expanded.as_deref() == Some(id.as_str());
        let confirming = panel.modules.confirm_remove.as_deref() == Some(id.as_str());
        let note_line = panel.modules.notes.get(&id).cloned();
        let missing: Vec<String> = module
            .manifest
            .settings
            .iter()
            .filter(|f| {
                f.required
                    && !f.secret
                    && !f.accepts(module.configuration.get(&f.key).unwrap_or(&f.default))
            })
            .map(|f| f.label.clone())
            .collect();
        let (toggle_id, up_id, down_id) = (id.clone(), id.clone(), id.clone());
        let (settings_id, check_id, update_id, remove_id) =
            (id.clone(), id.clone(), id.clone(), id.clone());
        let header = h_flex()
            .gap_3()
            .items_start()
            .child(icon_box(icon, 40., cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .truncate()
                                    .child(module.manifest.title().to_owned()),
                            )
                            .child(
                                Tag::secondary()
                                    .small()
                                    .rounded_full()
                                    .child(format!("#{}", index + 1)),
                            ),
                    )
                    .child(note(identity_line(&module.manifest), cx))
                    .child(chips(module.manifest.capabilities.clone())),
            )
            .child(
                Switch::new(SharedString::from(format!("module-enabled-{index}")))
                    .checked(module.enabled)
                    .on_click(cx.listener(move |this, checked: &bool, window, cx| {
                        this.modules_set_enabled(toggle_id.clone(), *checked, window, cx)
                    })),
            );
        let small = |name: String, icon: Lucide, label: &'static str| {
            Button::new(SharedString::from(name))
                .small()
                .ghost()
                .icon(icon)
                .label(label)
        };
        let actions = h_flex()
            .gap_1()
            .flex_wrap()
            .child(
                small(
                    format!("module-settings-{index}"),
                    Lucide::Settings2,
                    t!("modules.action.settings"),
                )
                .selected(expanded)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.modules_expand(settings_id.clone(), window, cx)
                })),
            )
            .child(
                small(
                    format!("module-check-{index}"),
                    Lucide::Stethoscope,
                    t!("modules.action.check"),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.modules_check(check_id.clone(), window, cx)
                })),
            )
            .child(
                small(
                    format!("module-update-{index}"),
                    Lucide::RefreshCw,
                    t!("modules.action.check_update"),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.modules_update(update_id.clone(), window, cx)
                })),
            )
            .child(
                Button::new(SharedString::from(format!("module-up-{index}")))
                    .small()
                    .ghost()
                    .icon(Lucide::ArrowUp)
                    .tooltip(t!("modules.action.higher"))
                    .disabled(index == 0)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.modules_move(up_id.clone(), -1, window, cx)
                    })),
            )
            .child(
                Button::new(SharedString::from(format!("module-down-{index}")))
                    .small()
                    .ghost()
                    .icon(Lucide::ArrowDown)
                    .tooltip(t!("modules.action.lower"))
                    .disabled(index + 1 == count)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.modules_move(down_id.clone(), 1, window, cx)
                    })),
            )
            .child(
                Button::new(SharedString::from(format!("module-remove-{index}")))
                    .small()
                    .map(|b| if confirming { b.danger() } else { b.ghost() })
                    .icon(Lucide::Trash)
                    .label(if confirming {
                        t!("modules.action.confirm_remove")
                    } else {
                        t!("modules.action.uninstall")
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.modules_remove(remove_id.clone(), window, cx)
                    })),
            );
        let mut card = v_flex()
            .gap_2()
            .p_3()
            .rounded(crate::theme::radius(8.))
            .border_1()
            .border_color(border)
            .child(header)
            .when(!missing.is_empty(), |this| {
                this.child(status_line(
                    t!("modules.needs_configuration", settings = missing.join(", ")).into(),
                    warning,
                ))
            })
            .when_some(health.playback.clone(), |this, d| {
                let color = match d.status {
                    music_core::modules::CheckStatus::Resolved => success,
                    music_core::modules::CheckStatus::NoMatch => muted,
                    _ => danger,
                };
                this.child(diagnostic_line(t!("modules.diag.last_song"), &d, color))
            })
            .when_some(note_line, |this, (error, text)| {
                this.child(status_line(text, if error { danger } else { muted }))
            })
            .child(actions);
        if expanded {
            card = card.child(render_details(panel, module, &health, cx));
        }
        list = list.child(card);
    }

    // ---- catalogs ----
    let catalogs = panel.modules.snapshot.catalogs.clone();
    let installed_ids: HashMap<String, String> = installed
        .iter()
        .map(|m| (m.manifest.id.clone(), m.manifest.version.clone()))
        .collect();
    let catalog_group = (!catalogs.is_empty()).then(|| {
        let mut g = group_at("modules.catalogs", t!("modules.catalogs"), cx);
        for (ci, catalog) in catalogs.iter().enumerate() {
            let (refresh_id, remove_id) = (catalog.id.clone(), catalog.id.clone());
            g = g.child(
                h_flex()
                    .gap_2()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(catalog.name.clone()),
                            )
                            .child(note(
                                tp!(
                                    "modules.catalog_line",
                                    catalog.entries.len(),
                                    url = catalog.url,
                                    when = ago(catalog.refreshed_at)
                                ),
                                cx,
                            )),
                    )
                    .child(
                        Button::new(SharedString::from(format!("catalog-refresh-{ci}")))
                            .small()
                            .ghost()
                            .icon(Lucide::RefreshCw)
                            .tooltip(t!("common.refresh"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.modules_refresh_catalog(refresh_id.clone(), window, cx)
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("catalog-remove-{ci}")))
                            .small()
                            .ghost()
                            .icon(Lucide::Trash)
                            .tooltip(t!("modules.remove_catalog"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.modules_remove_catalog(remove_id.clone(), window, cx)
                            })),
                    ),
            );
            for (ei, entry) in catalog.entries.iter().enumerate() {
                let installed_version = installed_ids.get(&entry.id);
                let (label, enabled) = match installed_version {
                    Some(v) if !entry.version.is_empty() && *v != entry.version => {
                        (t!("common.update"), true)
                    }
                    Some(_) => (t!("modules.entry.installed"), false),
                    None if entry.supported => (t!("common.install"), true),
                    None => (t!("modules.entry.not_a_module"), false),
                };
                let e = entry.clone();
                g = g.child(
                    h_flex()
                        .gap_3()
                        .pl_3()
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_sm().child(format!(
                                    "{}{}",
                                    entry.name,
                                    if entry.version.is_empty() {
                                        String::new()
                                    } else {
                                        format!(" {}", entry.version)
                                    }
                                )))
                                .child(note(
                                    [entry.author.clone(), entry.description.clone()]
                                        .into_iter()
                                        .filter(|s| !s.is_empty())
                                        .collect::<Vec<_>>()
                                        .join(" · "),
                                    cx,
                                )),
                        )
                        .child(
                            Button::new(SharedString::from(format!("catalog-{ci}-install-{ei}")))
                                .small()
                                .outline()
                                .label(label)
                                .disabled(!enabled || busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.modules_install_entry(e.clone(), window, cx)
                                })),
                        ),
                );
            }
        }
        g
    });

    let (installed_first, installed_last) = if count > 0 {
        (Some(list), None)
    } else {
        (None, Some(list))
    };
    v_flex()
        .gap_5()
        .child(
            group_at("modules.source", t!("modules.source.group"), cx)
                .child(source_buttons)
                .child(note(source_note, cx))
                .child(note(t!("modules.source.note"), cx)),
        )
        .children(review)
        // Installed modules first once there are any; the add form below them.
        .children(installed_first)
        .child(add)
        .children(installed_last)
        .children(catalog_group)
        .into_any_element()
}

fn render_details(
    panel: &mut SettingsPanel,
    module: &InstalledModule,
    health: &ModuleHealth,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let theme = cx.theme();
    let (muted, danger, success, border) = (
        theme.muted_foreground,
        theme.danger,
        theme.success,
        theme.border,
    );
    let mono = theme.mono_font_family.clone();
    let mut settings = v_flex().gap_2();
    match &panel.modules.form {
        Some(form) if form.id == module.id => {
            if form.fields.is_empty() {
                settings = settings.child(note(t!("modules.no_settings"), cx));
            }
            for (fi, f) in form.fields.iter().enumerate() {
                let key = f.field.key.clone();
                let label = format!(
                    "{}{}",
                    f.field.label,
                    if f.field.required { " *" } else { "" }
                );
                let control: AnyElement = match &f.value {
                    FormValue::Input(input) => {
                        let input = Input::new(input).small();
                        let input = if f.field.kind == SettingKind::Password {
                            input.mask_toggle()
                        } else {
                            input
                        };
                        div().w(px(300.)).child(input).into_any_element()
                    }
                    FormValue::Toggle(on) => {
                        Switch::new(SharedString::from(format!("module-field-{fi}")))
                            .checked(*on)
                            .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                if let Some(form) = &mut this.modules.form
                                    && let Some(f) =
                                        form.fields.iter_mut().find(|f| f.field.key == key)
                                {
                                    f.value = FormValue::Toggle(*checked);
                                }
                                cx.notify();
                            }))
                            .into_any_element()
                    }
                    FormValue::Select(selected) => h_flex()
                        .gap_1()
                        .flex_wrap()
                        .max_w(px(360.))
                        .children(f.field.options.iter().enumerate().map(|(oi, option)| {
                            let is = music_core_equivalent(&option.value, selected);
                            let (key, value) = (key.clone(), option.value.clone());
                            Button::new(SharedString::from(format!("module-field-{fi}-{oi}")))
                                .small()
                                .map(|b| if is { b.primary() } else { b.outline() })
                                .selected(is)
                                .label(option.label.clone())
                                .when(!option.description.is_empty(), |b| {
                                    b.tooltip(option.description.clone())
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(form) = &mut this.modules.form
                                        && let Some(f) =
                                            form.fields.iter_mut().find(|f| f.field.key == key)
                                    {
                                        f.value = FormValue::Select(value.clone());
                                    }
                                    cx.notify();
                                }))
                        }))
                        .into_any_element(),
                };
                let description: Option<&'static str> = None;
                settings = settings.child(row(label, description, control, cx));
                if !f.field.description.is_empty() {
                    settings = settings.child(note(f.field.description.clone(), cx));
                }
            }
            if !form.fields.is_empty() {
                settings =
                    settings.child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("module-save")
                                    .small()
                                    .primary()
                                    .label(t!("common.save"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.modules_save(window, cx)
                                    })),
                            )
                            .when_some(form.error.clone(), |this, e| {
                                this.child(status_line(e, danger))
                            }),
                    );
            }
        }
        _ => settings = settings.child(note(t!("modules.busy.loading_settings"), cx)),
    }

    let id = module.id.clone();
    let (copy_id, clear_id) = (id.clone(), id.clone());
    let log_lines = health.log.iter().rev().take(14).collect::<Vec<_>>();
    let diagnostics = v_flex()
        .gap_1()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(t!("modules.diagnostics")),
        )
        .child(match &health.check {
            Some(d) => diagnostic_line(
                t!("modules.diag.check"),
                d,
                if d.status == music_core::modules::CheckStatus::Ready {
                    success
                } else {
                    danger
                },
            )
            .into_any_element(),
            None => note(t!("modules.diag.not_checked"), cx).into_any_element(),
        })
        .child(match &health.playback {
            Some(d) => diagnostic_line(t!("modules.diag.playback"), d, muted).into_any_element(),
            None => note(t!("modules.diag.not_used"), cx).into_any_element(),
        })
        .child(note(
            if module.update_url.is_empty() {
                t!(
                    "modules.installed_line_file",
                    when = ago(module.installed_at),
                    hash = module.package_sha256.get(..12).unwrap_or("")
                )
            } else {
                t!(
                    "modules.installed_line_url",
                    when = ago(module.installed_at),
                    hash = module.package_sha256.get(..12).unwrap_or(""),
                    url = module.update_url
                )
            },
            cx,
        ))
        .child(
            v_flex()
                .p_2()
                .rounded(crate::theme::radius(6.))
                .bg(theme.muted)
                .text_xs()
                .font_family(mono)
                .when(log_lines.is_empty(), |this| {
                    this.child(div().text_color(muted).child(t!("modules.log_empty")))
                })
                .children(log_lines.into_iter().rev().map(|l| {
                    let color = match l.level.as_str() {
                        "error" => danger,
                        "warn" => theme.warning,
                        _ => theme.foreground,
                    };
                    div().text_color(color).line_clamp(2).child(format!(
                        "{} {:<5} {}",
                        ago(l.at),
                        l.level,
                        l.text
                    ))
                })),
        )
        .child(
            h_flex()
                .gap_1()
                .child(
                    Button::new("module-copy-diagnostics")
                        .small()
                        .ghost()
                        .icon(Lucide::ClipboardCopy)
                        .label(t!("modules.copy_diagnostics"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(m) = ModuleService::modules(cx) {
                                let mut text = m.diagnostics_report(&copy_id);
                                for line in m.health(&copy_id).log {
                                    text.push_str(&format!("{} {}\n", line.level, line.text));
                                }
                                cx.write_to_clipboard(ClipboardItem::new_string(text));
                                this.modules_report(false, t!("modules.diagnostics_copied"));
                                cx.notify();
                            }
                        })),
                )
                .child(
                    Button::new("module-clear-log")
                        .small()
                        .ghost()
                        .label(t!("modules.clear_log"))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            if let Some(m) = ModuleService::modules(cx) {
                                m.clear_log(&clear_id);
                            }
                            cx.notify();
                        })),
                ),
        );
    v_flex()
        .gap_3()
        .pt_2()
        .border_t_1()
        .border_color(border)
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(t!("modules.settings_heading")),
        )
        .child(settings)
        .child(diagnostics)
        .into_any_element()
}

/// JSON equality where `1` == `1.0` (select option values).
fn music_core_equivalent(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        _ => a == b,
    }
}
