//! Where credentials live: the OS keychain when it works, else the settings file.
//!
//! Secrets are the Last.fm API secret and session key, the ListenBrainz user
//! token, the proxy password and the YouTube Music sign-in cookie. They are stored with the `keyring` crate (macOS Keychain, Windows
//! Credential Manager, Secret Service on Linux) under service [`SERVICE`], one
//! entry per [`Secret`].
//!
//! If the keychain can't be used (no Secret Service daemon, a sandbox, or
//! `MUSIC_SECRET_STORE=file`), the value goes into the `secrets` map of
//! `settings.json` instead, which is written with mode 0600. Reads check the
//! keychain first and then the file; a successful keychain write removes the
//! file copy, so secrets migrate into the keychain once it becomes available.
//!
//! Keychain calls can block (macOS may show an "allow access" prompt), so the
//! UI runs them on a worker thread; the functions here are synchronous. Values
//! are never logged.

use std::collections::BTreeMap;

pub const SERVICE: &str = crate::settings::APP_ID;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Secret {
    LastFmShared,
    LastFmSession,
    ListenBrainzToken,
    /// Network section: the proxy password.
    ProxyPassword,
    /// Lyrics providers / AI translation (Lyrics settings).
    PaxsenixKey,
    BetterLyricsKey,
    AiTranslateKey,
    /// `Cookie` header of a signed-in music.youtube.com session.
    YouTubeCookie,
}

impl Secret {
    #[cfg(test)]
    pub const ALL: [Secret; 8] = [
        Secret::LastFmShared,
        Secret::LastFmSession,
        Secret::ListenBrainzToken,
        Secret::PaxsenixKey,
        Secret::BetterLyricsKey,
        Secret::AiTranslateKey,
        Secret::YouTubeCookie,
        Secret::ProxyPassword,
    ];

    /// Keychain account name and settings-file key.
    pub fn key(self) -> &'static str {
        match self {
            Secret::LastFmShared => "lastfm-api-secret",
            Secret::LastFmSession => "lastfm-session-key",
            Secret::ListenBrainzToken => "listenbrainz-token",
            Secret::ProxyPassword => "proxy-password",
            Secret::PaxsenixKey => "paxsenix-api-key",
            Secret::BetterLyricsKey => "betterlyrics-api-key",
            Secret::AiTranslateKey => "ai-translate-api-key",
            Secret::YouTubeCookie => "youtube-cookie",
        }
    }
}

/// A credential store that may be unavailable at runtime.
pub trait Backend: Send + Sync {
    fn name(&self) -> &'static str;
    /// `Ok(None)` = no such entry. `Err` = the store itself failed.
    fn get(&self, key: &str) -> Result<Option<String>, String>;
    fn set(&self, key: &str, value: &str) -> Result<(), String>;
    /// Deleting a missing entry is not an error.
    fn delete(&self, key: &str) -> Result<(), String>;
}

/// The OS keychain via `keyring`.
pub struct Keychain;

impl Keychain {
    fn entry(key: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(SERVICE, key).map_err(|e| e.to_string())
    }
}

impl Backend for Keychain {
    fn name(&self) -> &'static str {
        if cfg!(target_os = "macos") {
            "macOS Keychain"
        } else if cfg!(windows) {
            "Windows Credential Manager"
        } else {
            "Secret Service"
        }
    }

    fn get(&self, key: &str) -> Result<Option<String>, String> {
        match Self::entry(key)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), String> {
        Self::entry(key)?
            .set_password(value)
            .map_err(|e| e.to_string())
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        match Self::entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// The keychain unless `MUSIC_SECRET_STORE=file` (or the platform has none).
pub fn default_backend() -> Option<Box<dyn Backend>> {
    if std::env::var("MUSIC_SECRET_STORE").is_ok_and(|v| v == "file") {
        return None;
    }
    match keyring::Entry::store_status() {
        Ok(()) => Some(Box::new(Keychain)),
        Err(e) => {
            eprintln!("music-gpui: keychain unavailable ({e}); secrets go to the settings file");
            None
        }
    }
}

/// Where a secret ended up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stored {
    Keychain,
    /// In the settings file; the reason the keychain wasn't used.
    File(String),
}

/// What [`load_all`] found. `Debug` is redacted.
#[derive(Clone, Default, PartialEq)]
pub struct Loaded {
    pub values: BTreeMap<Secret, String>,
    /// Secrets found in the keychain.
    pub in_keychain: usize,
    /// Keychain read errors (the file copy, if any, is used instead).
    pub errors: Vec<String>,
}

impl std::fmt::Debug for Loaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loaded")
            .field("present", &self.values.keys().collect::<Vec<_>>())
            .field("in_keychain", &self.in_keychain)
            .field("errors", &self.errors)
            .finish()
    }
}

/// Reads `wanted`: keychain first, then the file copy. Blocking.
pub fn load_all(
    backend: Option<&dyn Backend>,
    file: &BTreeMap<String, String>,
    wanted: &[Secret],
) -> Loaded {
    let mut loaded = Loaded::default();
    for &secret in wanted {
        let from_keychain = match backend.map(|b| b.get(secret.key())) {
            Some(Ok(value)) => value,
            Some(Err(e)) => {
                loaded.errors.push(format!("{}: {e}", secret.key()));
                None
            }
            None => None,
        };
        if from_keychain.is_some() {
            loaded.in_keychain += 1;
        }
        if let Some(value) = from_keychain.or_else(|| file.get(secret.key()).cloned()) {
            loaded.values.insert(secret, value);
        }
    }
    loaded
}

/// Stores `value` in the keychain if possible. Blocking. The caller applies the
/// result to the settings file with [`apply`].
pub fn store(backend: Option<&dyn Backend>, secret: Secret, value: &str) -> Stored {
    match backend {
        Some(b) => match b.set(secret.key(), value) {
            Ok(()) => Stored::Keychain,
            Err(e) => Stored::File(format!("{}: {e}", b.name())),
        },
        None => Stored::File("keychain disabled".into()),
    }
}

/// Removes `secret` from the keychain. Blocking. Also call [`apply`] with `None`.
pub fn delete(backend: Option<&dyn Backend>, secret: Secret) -> Result<(), String> {
    backend.map_or(Ok(()), |b| b.delete(secret.key()))
}

/// Updates the settings-file copy after [`store`] / [`delete`]: the file only
/// keeps a secret when the keychain refused it.
pub fn apply(
    file: &mut BTreeMap<String, String>,
    secret: Secret,
    value: Option<&str>,
    stored: &Stored,
) {
    match (value, stored) {
        (Some(value), Stored::File(_)) => {
            file.insert(secret.key().to_owned(), value.to_owned());
        }
        _ => {
            file.remove(secret.key());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// In-memory keychain that can be switched off.
    #[derive(Default)]
    struct Fake {
        entries: Mutex<BTreeMap<String, String>>,
        broken: bool,
    }

    impl Backend for Fake {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn get(&self, key: &str) -> Result<Option<String>, String> {
            if self.broken {
                return Err("locked".into());
            }
            Ok(self.entries.lock().unwrap().get(key).cloned())
        }
        fn set(&self, key: &str, value: &str) -> Result<(), String> {
            if self.broken {
                return Err("locked".into());
            }
            self.entries
                .lock()
                .unwrap()
                .insert(key.into(), value.into());
            Ok(())
        }
        fn delete(&self, key: &str) -> Result<(), String> {
            if self.broken {
                return Err("locked".into());
            }
            self.entries.lock().unwrap().remove(key);
            Ok(())
        }
    }

    fn put(backend: Option<&dyn Backend>, file: &mut BTreeMap<String, String>, s: Secret, v: &str) {
        let stored = store(backend, s, v);
        apply(file, s, Some(v), &stored);
    }

    #[test]
    fn keychain_is_preferred_and_the_file_stays_clean() {
        let keychain = Fake::default();
        let mut file = BTreeMap::new();
        assert_eq!(
            store(Some(&keychain), Secret::LastFmSession, "sk"),
            Stored::Keychain
        );
        put(Some(&keychain), &mut file, Secret::ListenBrainzToken, "tok");
        assert!(file.is_empty());
        let loaded = load_all(Some(&keychain), &file, &Secret::ALL);
        assert_eq!(loaded.in_keychain, 2);
        assert_eq!(loaded.values[&Secret::ListenBrainzToken], "tok");
        assert!(!format!("{loaded:?}").contains("tok\""));
    }

    #[test]
    fn falls_back_to_the_file_when_the_keychain_fails() {
        let keychain = Fake {
            broken: true,
            ..Fake::default()
        };
        let mut file = BTreeMap::new();
        put(Some(&keychain), &mut file, Secret::LastFmShared, "s3cret");
        assert_eq!(file["lastfm-api-secret"], "s3cret");
        let loaded = load_all(Some(&keychain), &file, &Secret::ALL);
        assert_eq!(loaded.values[&Secret::LastFmShared], "s3cret");
        assert_eq!(loaded.in_keychain, 0);
        assert_eq!(loaded.errors.len(), Secret::ALL.len());

        // No keychain at all (MUSIC_SECRET_STORE=file).
        let mut file = BTreeMap::new();
        put(None, &mut file, Secret::LastFmSession, "sk");
        assert_eq!(
            load_all(None, &file, &Secret::ALL).values[&Secret::LastFmSession],
            "sk"
        );
    }

    #[test]
    fn file_secrets_migrate_into_a_working_keychain() {
        let mut file = BTreeMap::from([("listenbrainz-token".to_owned(), "old".to_owned())]);
        let keychain = Fake::default();
        // Still readable from the file before anything is re-saved...
        let loaded = load_all(Some(&keychain), &file, &Secret::ALL);
        assert_eq!(loaded.values[&Secret::ListenBrainzToken], "old");
        // ...and a successful keychain write drops the file copy.
        put(Some(&keychain), &mut file, Secret::ListenBrainzToken, "old");
        assert!(file.is_empty());
        assert_eq!(
            load_all(Some(&keychain), &file, &Secret::ALL).values[&Secret::ListenBrainzToken],
            "old"
        );
    }

    #[test]
    fn delete_clears_both_places() {
        let keychain = Fake::default();
        let mut file = BTreeMap::from([("lastfm-session-key".to_owned(), "a".to_owned())]);
        put(
            Some(&keychain),
            &mut BTreeMap::new(),
            Secret::LastFmSession,
            "b",
        );
        delete(Some(&keychain), Secret::LastFmSession).unwrap();
        apply(&mut file, Secret::LastFmSession, None, &Stored::Keychain);
        assert!(
            load_all(Some(&keychain), &file, &Secret::ALL)
                .values
                .is_empty()
        );
    }

    /// Talks to the real OS keychain (may prompt on macOS); run with `--ignored`.
    #[test]
    #[ignore]
    fn real_keychain_round_trip() {
        let key = "music-gpui-selftest";
        let keychain = Keychain;
        keychain.set(key, "value").unwrap();
        assert_eq!(keychain.get(key).unwrap().as_deref(), Some("value"));
        keychain.delete(key).unwrap();
        assert_eq!(keychain.get(key).unwrap(), None);
    }
}
