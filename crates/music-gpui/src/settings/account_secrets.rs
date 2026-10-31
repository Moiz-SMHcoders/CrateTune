//! `SettingsPanel` methods for the cookies of several YouTube Music accounts
//! ("Several accounts", `account_list.rs`).
//!
//! The account in use keeps its cookie in `Secret::YouTubeCookie`, as before; each
//! other signed-in account keeps its own under `secrets::account_cookie_key(id)`.
//! A switch moves them in one background job, in an order that never loses one: the
//! outgoing cookie is saved under its key first, then the incoming one becomes the
//! active cookie, then its own key is deleted. Values are never logged.

use gpui::Context;

use super::panel::{SettingsEvent, SettingsPanel};
use crate::secrets::{self, Secret, Stored};
use crate::util::off_thread;

impl SettingsPanel {
    /// A job that reads saved account `id`'s cookie (keychain, else the settings
    /// file). Blocking: run it off the UI thread.
    pub fn account_cookie_reader(
        &self,
        id: &str,
    ) -> impl FnOnce() -> Option<String> + Send + use<> {
        let backend = self.backend.clone();
        let file = self.settings.secrets.0.clone();
        let key = secrets::account_cookie_key(id);
        move || secrets::load_key(backend.as_deref(), &file, &key).filter(|c| !c.is_empty())
    }

    /// Deletes saved account `id`'s cookie in the background.
    pub fn forget_account_cookie(&mut self, id: &str, cx: &mut Context<Self>) {
        self.move_account_cookies(None, None, Some(id.to_owned()), cx);
    }

    /// A switch: `outgoing` (its id and latest cookie) goes under its own key,
    /// `incoming` becomes the active cookie, and `incoming_id`'s key is deleted.
    /// The active cookie is updated in memory at once (`youtube_cookie`).
    pub fn swap_account_cookies(
        &mut self,
        outgoing: Option<(String, String)>,
        incoming: String,
        incoming_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.secrets.insert(Secret::YouTubeCookie, incoming.clone());
        self.move_account_cookies(outgoing, Some(incoming), incoming_id, cx);
        cx.emit(SettingsEvent::Library);
    }

    /// The background job behind the methods above: (1) save `saved` under its key,
    /// (2) make `active` the active cookie, (3) delete `drop_id`'s key; then record in
    /// the settings file whatever the keychain refused, and save.
    fn move_account_cookies(
        &mut self,
        saved: Option<(String, String)>,
        active: Option<String>,
        drop_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let results = off_thread(move || {
                let b = backend.as_deref();
                let mut out: Vec<(String, Option<String>, Stored)> = Vec::new();
                if let Some((id, cookie)) = saved {
                    let key = secrets::account_cookie_key(&id);
                    let stored = secrets::store_key(b, &key, &cookie);
                    out.push((key, Some(cookie), stored));
                }
                if let Some(cookie) = active {
                    let key = Secret::YouTubeCookie.key().to_owned();
                    let stored = secrets::store_key(b, &key, &cookie);
                    out.push((key, Some(cookie), stored));
                }
                if let Some(id) = drop_id {
                    let key = secrets::account_cookie_key(&id);
                    let stored = match secrets::delete_key(b, &key) {
                        Ok(()) => Stored::Keychain,
                        Err(e) => Stored::File(e),
                    };
                    out.push((key, None, stored));
                }
                out
            })
            .await;
            this.update(cx, |this, cx| {
                for (key, value, stored) in &results {
                    if let Stored::File(reason) = stored {
                        eprintln!("music-gpui: {key}: keychain not used ({reason})");
                    }
                    secrets::apply_key(&mut this.settings.secrets.0, key, value.as_deref(), stored);
                }
                this.changed(false, cx);
                // The account list must match the keychain on disk soon.
                this.flush();
            })
            .ok();
        })
        .detach();
    }
}
