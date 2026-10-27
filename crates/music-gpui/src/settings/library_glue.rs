//! `SettingsPanel` methods for the library preferences (privacy, storage, local
//! folders, account) and the YouTube cookie secret, plus the settings JSON that
//! goes into / comes out of a backup.

use gpui::Context;

use super::file::Settings;
use super::library_config::LibraryConfig;
use super::panel::{SettingsEvent, SettingsPanel};
use crate::secrets::Secret;

impl SettingsPanel {
    /// Changes `settings.library`, saves (debounced) and tells the app.
    pub fn update_library(&mut self, f: impl FnOnce(&mut LibraryConfig), cx: &mut Context<Self>) {
        let before = self.settings.library.clone();
        f(&mut self.settings.library);
        self.settings.library.normalize();
        if self.settings.library != before {
            cx.emit(SettingsEvent::Library);
            self.changed(false, cx);
        }
    }

    /// Whether the keychain has been read (the cookie is known to be there or not).
    pub fn secrets_loaded(&self) -> bool {
        self.secrets_loaded
    }

    pub fn youtube_cookie(&self) -> Option<&str> {
        self.secrets
            .get(&Secret::YouTubeCookie)
            .map(String::as_str)
            .filter(|c| !c.is_empty())
    }

    /// Stores (keychain, else the settings file) or forgets the sign-in cookie.
    pub fn set_youtube_cookie(&mut self, cookie: Option<String>, cx: &mut Context<Self>) {
        self.put_secret(Secret::YouTubeCookie, cookie, cx);
        cx.emit(SettingsEvent::Library);
    }

    /// Settings for a backup: no secrets, no window frames (they belong to this machine).
    pub fn backup_json(&self) -> String {
        backup_settings(&self.settings).to_json()
    }

    /// Applies settings from a backup. Secrets, the signed-in account and the window
    /// frames of this machine are kept. Saved right away; most of it takes effect after
    /// a restart.
    pub fn restore_from_backup(
        &mut self,
        json: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let restored = Settings::from_json(json).map_err(|e| e.to_string())?;
        self.settings = merge_restored(&self.settings, restored);
        cx.emit(SettingsEvent::Appearance);
        cx.emit(SettingsEvent::PlaybackModes);
        cx.emit(SettingsEvent::Library);
        self.changed(true, cx);
        self.flush();
        Ok(())
    }
}

fn backup_settings(settings: &Settings) -> Settings {
    let mut s = settings.clone();
    s.secrets = Default::default();
    s.window = None;
    s.mini_player = None;
    s.library.account = None;
    s
}

fn merge_restored(current: &Settings, mut restored: Settings) -> Settings {
    restored.secrets = current.secrets.clone();
    restored.window = current.window;
    restored.mini_player = current.mini_player;
    restored.library.account = current.library.account.clone();
    restored
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::library_config::AccountConfig;

    #[test]
    fn backups_carry_no_secrets_and_keep_local_state_on_restore() {
        let mut s = Settings::default();
        s.secrets
            .0
            .insert("youtube-cookie".into(), "SAPISID=x".into());
        s.library.account = Some(AccountConfig {
            name: "Me".into(),
            ..Default::default()
        });
        s.volume = 0.3;
        let json = backup_settings(&s).to_json();
        assert!(!json.contains("SAPISID"));
        assert!(!json.contains("\"Me\""));

        let mut from_backup = Settings::from_json(&json).unwrap();
        from_backup.volume = 0.8;
        let merged = merge_restored(&s, from_backup);
        assert_eq!(merged.volume, 0.8);
        assert_eq!(merged.secrets, s.secrets);
        assert_eq!(merged.library.account, s.library.account);
    }
}
