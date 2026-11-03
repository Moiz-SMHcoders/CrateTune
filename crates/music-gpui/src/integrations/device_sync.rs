//! Sync with the paired devices in the app (BACKLOG M8): settings, likes, local
//! playlists, speed-dial pins, queue tabs and, if wanted, the listen history. The
//! document and its rules are `music_core::sync`; the rounds run on the device
//! hand-off node (`integrations/devices.rs`), over the same encrypted links.
//!
//! Off until switched on in Settings → Devices → Sync (on both devices). Then:
//! the engine opens `sync.sqlite` next to the library, the library tells the node
//! about every write (`Library::set_change_listener`; the node waits a few seconds,
//! records what changed and only talks to devices heard from lately if anything
//! did), the settings are passed in each time they are saved, and the node syncs
//! with every paired device when it starts and on "Sync now". Nothing runs on a
//! timer; a round's result arrives as a hand-off event on the app's poll.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{Context, Window};
use music_core::handoff::now_unix_ms;
use music_core::sync::{Category, SyncApplied, SyncEngine};

use crate::app::MusicApp;
use crate::util::off_thread;

/// The document's file, next to the library.
pub const FILE_NAME: &str = "sync.sqlite";

#[derive(Default)]
pub struct SyncState {
    engine: Option<Arc<SyncEngine>>,
    /// The device id the engine was opened for.
    engine_for: Option<String>,
    opening: bool,
    /// The categories last given to the engine.
    cats: Vec<Category>,
    /// Device id → when this device last synced with it (Unix ms).
    pub last: HashMap<String, u64>,
    /// Rounds asked for and not answered yet.
    pub busy: HashSet<String>,
    /// Device id → why the last round failed.
    pub errors: HashMap<String, String>,
    /// The last round that brought something: (when, entries).
    pub received: Option<(u64, usize)>,
}

impl SyncState {
    pub fn running(&self) -> bool {
        self.engine.is_some() && !self.cats.is_empty()
    }
}

impl MusicApp {
    /// Matches sync to the settings and the node: opens the document, switches the
    /// node's sync on or off, and syncs with everyone when something was switched on.
    /// Called when the Devices settings change and after the node started.
    pub(crate) fn sync_setup(&mut self, cx: &mut Context<Self>) {
        let prefs = self.devices_prefs(cx);
        let cats = prefs.sync.categories();
        let id = self.integrations.devices.id.clone();
        let library = self.store.library.clone();
        let Some(node) = self.integrations.devices.node() else {
            // Started again with the node (`devices_start` calls back).
            return;
        };
        let state = &mut self.integrations.sync;
        if cats.is_empty() {
            node.set_sync(None);
            if let Some(lib) = &library {
                lib.set_change_listener(None);
            }
            state.cats.clear();
            state.busy.clear();
            return;
        }
        let Some(id) = id else { return };
        if state.engine_for.as_deref() != Some(id.as_str()) {
            state.engine = None;
        }
        let Some(engine) = state.engine.clone() else {
            if state.opening {
                return;
            }
            let Some(dir) = self.store.dir.clone() else {
                return;
            };
            state.opening = true;
            cx.spawn(async move |this, cx| {
                let me = id.clone();
                let opened = off_thread(move || {
                    SyncEngine::open(&dir.join(FILE_NAME), &me, library).map(Arc::new)
                })
                .await;
                this.update(cx, |this, cx| {
                    this.integrations.sync.opening = false;
                    match opened {
                        Ok(engine) => {
                            let last = this
                                .devices_prefs(cx)
                                .paired
                                .iter()
                                .filter_map(|d| Some((d.id.clone(), engine.last_synced(&d.id)?)))
                                .collect();
                            let state = &mut this.integrations.sync;
                            state.last = last;
                            state.engine = Some(engine);
                            state.engine_for = Some(id);
                            state.cats.clear();
                            this.sync_setup(cx);
                        }
                        Err(e) => {
                            log::warn!("sync: the document can't be opened ({e})"); // i18n-ignore
                            this.integrations.devices.error = Some(e.to_string());
                        }
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
            return;
        };
        let fresh = state.cats != cats;
        let more = cats.iter().any(|c| !state.cats.contains(c));
        state.cats = cats.clone();
        engine.set_categories(&cats);
        let flat = crate::settings::sync_settings::flatten(self.settings.read(cx).settings());
        engine.set_defaults(crate::settings::sync_settings::flatten(
            &crate::settings::file::Settings::default(),
        ));
        engine.set_settings(flat);
        node.set_sync(Some(engine));
        if let Some(lib) = &library {
            lib.set_change_listener(Some(node.change_notifier()));
        }
        if fresh && more {
            self.sync_now(cx);
        }
    }

    /// "Sync now" (and after switching a category on): a round with every paired device.
    pub fn sync_now(&mut self, cx: &mut Context<Self>) {
        let paired: Vec<String> = self
            .devices_prefs(cx)
            .paired
            .into_iter()
            .map(|d| d.id)
            .collect();
        if !self.integrations.sync.running() {
            return;
        }
        let Some(node) = self.integrations.devices.node() else {
            return;
        };
        node.sync_all();
        let state = &mut self.integrations.sync;
        state.busy.extend(paired);
        state.errors.clear();
        cx.notify();
    }

    /// The settings were saved: the engine gets them, the node hears of the change.
    pub(crate) fn sync_settings_saved(&mut self, cx: &mut Context<Self>) {
        let state = &self.integrations.sync;
        let Some(engine) = state.engine.clone().filter(|_| state.running()) else {
            return;
        };
        if !state
            .cats
            .iter()
            .any(|c| matches!(c, Category::Settings | Category::Sound))
        {
            return;
        }
        let flat = crate::settings::sync_settings::flatten(self.settings.read(cx).settings());
        engine.set_settings(flat);
        if let Some(node) = self.integrations.devices.node() {
            node.local_changed();
        }
    }

    /// A round went through (from the node's events).
    pub(crate) fn sync_on_synced(
        &mut self,
        id: String,
        applied: SyncApplied,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let now = now_unix_ms();
        let state = &mut self.integrations.sync;
        state.busy.remove(&id);
        state.errors.remove(&id);
        state.last.insert(id.clone(), now);
        if applied.received > 0 {
            state.received = Some((now, applied.received));
            log::info!(
                "sync: {} change(s) from {}", // i18n-ignore
                applied.received,
                music_core::handoff::short_id(&id)
            );
        }
        if !applied.settings.is_empty() {
            let changes = applied.settings.clone();
            self.settings
                .update(cx, |panel, cx| panel.apply_synced(&changes, cx));
        }
        self.sync_reload(&applied.library, cx);
    }

    pub(crate) fn sync_on_failed(&mut self, id: String, error: String) {
        log::info!(
            "sync: {} failed ({error})", // i18n-ignore
            music_core::handoff::short_id(&id)
        );
        let state = &mut self.integrations.sync;
        state.busy.remove(&id);
        state.errors.insert(id, error);
    }

    /// Reloads what the app keeps of library data that sync changed.
    fn sync_reload(&mut self, cats: &[Category], cx: &mut Context<Self>) {
        if cats.is_empty() {
            return;
        }
        let Some(lib) = self.store.library.clone() else {
            return;
        };
        if cats.contains(&Category::QueueTabs) {
            let viewed = self.queue_tabs.viewed;
            self.load_queue_tabs();
            self.queue_tabs.viewed = viewed.min(self.queue_tabs.tabs.len().saturating_sub(1));
        }
        let likes = cats.contains(&Category::Likes);
        let pins = cats.contains(&Category::Pins);
        cx.spawn(async move |this, cx| {
            let loaded = off_thread(move || {
                let flags = likes.then(|| {
                    let liked: HashSet<String> = lib
                        .songs(&music_core::library::SongQuery {
                            scope: music_core::library::SongScope::Liked,
                            ..Default::default()
                        })
                        .map(|s| s.into_iter().map(|s| s.track.video_id).collect())
                        .unwrap_or_default();
                    (liked, lib.disliked_ids().unwrap_or_default())
                });
                let pins = pins.then(|| crate::pages::speed_dial_load(&lib));
                (flags, pins)
            })
            .await;
            this.update(cx, |this, cx| {
                let (flags, pins) = loaded;
                if let Some((liked, disliked)) = flags {
                    crate::store::set_rating_flags(liked, disliked);
                }
                if let Some(pins) = pins {
                    crate::pages::speed_dial_set_loaded(pins);
                }
                this.library_changed(cx);
            })
            .ok();
        })
        .detach();
    }

    /// `@device-sync-status`.
    pub(crate) fn print_sync_status(&self, cx: &gpui::App) {
        let s = &self.integrations.sync;
        let peers: Vec<String> = self
            .devices_prefs(cx)
            .paired
            .iter()
            .map(|d| d.id.clone())
            .collect();
        let entries = s.engine.as_ref().map(|e| e.status(&peers).entries);
        eprintln!(
            "music-gpui: sync running={} cats={:?} entries={entries:?} received={:?} busy={}",
            s.running(),
            s.cats.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
            s.received,
            s.busy.len()
        );
        for device in self.devices_prefs(cx).paired {
            eprintln!(
                "music-gpui: sync   {} ({}): last={:?} error={:?}",
                device.name,
                music_core::handoff::short_id(&device.id),
                s.last.get(&device.id),
                s.errors.get(&device.id)
            );
        }
    }
}
