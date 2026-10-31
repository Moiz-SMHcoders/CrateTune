//! Chromium's cookie store: SQLite (`Cookies`, table `cookies`) whose values are
//! encrypted per OS (see the module docs of [`super`]).
//!
//! An `encrypted_value` starts with a version tag:
//!
//! * `v10` / `v11` on macOS and Linux: AES-128-CBC, IV of 16 spaces, PKCS#7, key =
//!   PBKDF2-HMAC-SHA1(password, `saltysalt`, 1003 rounds on macOS / 1 on Linux).
//!   Linux `v10` uses the password `peanuts`; `v11` the one from libsecret / KWallet
//!   (or an empty one when the browser had no keyring).
//! * `v10` on Windows: AES-256-GCM, 12-byte nonce after the tag, 16-byte tag at the
//!   end; the key is `os_crypt.encrypted_key` of `Local State` opened with DPAPI.
//! * `v20` on Windows: app-bound encryption; not decryptable outside the browser
//!   ([`ImportError::AppBound`]).
//!
//! Since cookie database version 24 the plaintext starts with SHA-256 of the host
//! (32 bytes); it is checked and dropped.

use std::path::Path;

use aes::cipher::{BlockDecryptMut as _, KeyIvInit as _, block_padding::Pkcs7};
use base64::Engine as _;
use sha2::{Digest as _, Sha256};

use super::{Cookie, ImportError, TempCopy};

/// PBKDF2 salt Chromium uses for the cookie key.
const SALT: &[u8] = b"saltysalt";
/// Rounds on macOS / on Linux.
pub const MAC_ROUNDS: u32 = 1003;
pub const LINUX_ROUNDS: u32 = 1;

/// A cookie key.
#[derive(Clone, PartialEq, Eq)]
pub enum Key {
    /// AES-128-CBC (macOS, Linux).
    Cbc([u8; 16]),
    /// AES-256-GCM (Windows).
    Gcm([u8; 32]),
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Key::Cbc(_) => "Key::Cbc(<redacted>)",
            Key::Gcm(_) => "Key::Gcm(<redacted>)",
        })
    }
}

impl Key {
    /// The CBC key for `password` (`rounds`: [`MAC_ROUNDS`] or [`LINUX_ROUNDS`]).
    pub fn derive(password: &[u8], rounds: u32) -> Key {
        let mut key = [0u8; 16];
        pbkdf2::pbkdf2_hmac::<sha1::Sha1>(password, SALT, rounds, &mut key);
        Key::Cbc(key)
    }

    /// Linux `v10`: the built-in password.
    pub fn peanuts() -> Key {
        Key::derive(b"peanuts", LINUX_ROUNDS)
    }
}

/// The keys to try per version tag.
#[derive(Debug, Clone, Default)]
pub struct Keys {
    pub v10: Vec<Key>,
    pub v11: Vec<Key>,
}

impl Keys {
    /// macOS: `v10` with the Safe Storage password.
    pub fn mac(password: &[u8]) -> Keys {
        Keys {
            v10: vec![Key::derive(password, MAC_ROUNDS)],
            v11: Vec::new(),
        }
    }

    /// Linux: `v10` with "peanuts", `v11` with the keyring password if known, then
    /// the empty one (a browser that ran without a keyring).
    pub fn linux(keyring_password: Option<&[u8]>) -> Keys {
        let mut v11 = Vec::new();
        if let Some(p) = keyring_password {
            v11.push(Key::derive(p, LINUX_ROUNDS));
        }
        v11.push(Key::derive(b"", LINUX_ROUNDS));
        Keys {
            v10: vec![Key::peanuts()],
            v11,
        }
    }

    /// Windows: `v10` with the AES-256 key from `Local State`.
    pub fn windows(key: [u8; 32]) -> Keys {
        Keys {
            v10: vec![Key::Gcm(key)],
            v11: Vec::new(),
        }
    }
}

/// Windows: the DPAPI-sealed key in `Local State` (`os_crypt.encrypted_key`, base64,
/// `DPAPI` prefix removed), for the app to open with `CryptUnprotectData`.
pub fn windows_sealed_key(local_state: &str) -> Result<Vec<u8>, ImportError> {
    let json: serde_json::Value = serde_json::from_str(local_state)
        .map_err(|e| ImportError::Format(format!("Local State: {e}")))?;
    let encoded = json
        .pointer("/os_crypt/encrypted_key")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| ImportError::NoKey("no encrypted_key in Local State".into()))?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|e| ImportError::Format(format!("Local State key: {e}")))?;
    raw.strip_prefix(b"DPAPI")
        .map(<[u8]>::to_vec)
        .ok_or_else(|| ImportError::NoKey("Local State key isn't DPAPI-sealed".into()))
}

/// The plaintext of one value with `key` (`None`: wrong key / damaged).
fn decrypt_with(key: &Key, data: &[u8]) -> Option<Vec<u8>> {
    match key {
        Key::Cbc(k) => {
            let mut buf = data.to_vec();
            let plain = cbc::Decryptor::<aes::Aes128>::new(k.into(), &[b' '; 16].into())
                .decrypt_padded_mut::<Pkcs7>(&mut buf)
                .ok()?;
            Some(plain.to_vec())
        }
        Key::Gcm(k) => {
            use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
            if data.len() < 12 + 16 {
                return None;
            }
            let (nonce, sealed) = data.split_at(12);
            let key = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, k).ok()?);
            let nonce = Nonce::try_assume_unique_for_key(nonce).ok()?;
            let mut buf = sealed.to_vec();
            let plain = key.open_in_place(nonce, Aad::empty(), &mut buf).ok()?;
            Some(plain.to_vec())
        }
    }
}

/// Decrypts one `encrypted_value` of `host`.
pub fn decrypt_value(host: &str, encrypted: &[u8], keys: &Keys) -> Result<String, ImportError> {
    let (tag, data) = encrypted.split_at(encrypted.len().min(3));
    let candidates = match tag {
        b"v10" => &keys.v10,
        b"v11" => &keys.v11,
        b"v20" => return Err(ImportError::AppBound),
        _ => return Err(ImportError::Decrypt),
    };
    if candidates.is_empty() {
        return Err(ImportError::NoKey(format!(
            "no key for {} values",
            String::from_utf8_lossy(tag)
        )));
    }
    for key in candidates {
        let Some(mut plain) = decrypt_with(key, data) else {
            continue;
        };
        // Database version 24+: SHA-256 of the host first.
        if plain.len() >= 32 && plain[..32] == Sha256::digest(host.as_bytes())[..] {
            plain.drain(..32);
        }
        if let Ok(text) = String::from_utf8(plain) {
            return Ok(text);
        }
    }
    Err(ImportError::Decrypt)
}

/// Chromium's `expires_utc` (µs since 1601) → Unix seconds; 0 = session cookie.
fn expires(utc: i64) -> Option<i64> {
    const EPOCH_DELTA_SECS: i64 = 11_644_473_600;
    (utc > 0).then(|| utc / 1_000_000 - EPOCH_DELTA_SECS)
}

/// The youtube.com cookies of the Chromium database at `db` (copied first),
/// decrypted with `keys`. Values that don't decrypt fail the import (a wrong key
/// would otherwise yield a half session).
pub fn read(db: &Path, keys: &Keys) -> Result<Vec<Cookie>, ImportError> {
    let copy = TempCopy::of(db)?;
    let conn = rusqlite::Connection::open(&copy.path)?;
    let mut stmt = conn.prepare(
        "SELECT host_key, name, value, encrypted_value, expires_utc FROM cookies \
         WHERE host_key = 'youtube.com' OR host_key LIKE '%.youtube.com'",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Vec<u8>>(3)?,
            r.get::<_, i64>(4)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (host, name, plain, encrypted, expires_utc) = row?;
        let value = if !plain.is_empty() || encrypted.is_empty() {
            plain
        } else {
            decrypt_value(&host, &encrypted, keys)?
        };
        out.push(Cookie {
            host,
            name,
            value,
            expires: expires(expires_utc),
        });
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::browser_cookies::tests::temp_home;
    use crate::browser_cookies::{ImportError, cookie_header};
    use aes::cipher::BlockEncryptMut as _;

    /// What Chromium writes: tag + CBC(PKCS#7) of `plain`, with the host hash first
    /// when `hashed`.
    pub(crate) fn seal_cbc(
        tag: &[u8],
        key: &Key,
        host: &str,
        plain: &str,
        hashed: bool,
    ) -> Vec<u8> {
        let Key::Cbc(k) = key else { unreachable!() };
        let mut msg = Vec::new();
        if hashed {
            msg.extend_from_slice(&Sha256::digest(host.as_bytes()));
        }
        msg.extend_from_slice(plain.as_bytes());
        let len = msg.len();
        msg.resize(len + 16, 0);
        let sealed = cbc::Encryptor::<aes::Aes128>::new(k.into(), &[b' '; 16].into())
            .encrypt_padded_mut::<Pkcs7>(&mut msg, len)
            .unwrap()
            .to_vec();
        [tag, &sealed].concat()
    }

    fn seal_gcm(key: &[u8; 32], host: &str, plain: &str) -> Vec<u8> {
        use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
        let nonce = [7u8; 12];
        let k = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).unwrap());
        let mut msg = Sha256::digest(host.as_bytes()).to_vec();
        msg.extend_from_slice(plain.as_bytes());
        k.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut msg)
            .unwrap();
        [b"v10".as_slice(), &nonce, &msg].concat()
    }

    /// A Chromium `Cookies` database with `rows` (host, name, plain value,
    /// encrypted value).
    pub(crate) fn fixture_db(path: &Path, rows: &[(&str, &str, &str, Vec<u8>)]) {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE meta(key LONGVARCHAR NOT NULL UNIQUE PRIMARY KEY, value LONGVARCHAR);
             INSERT INTO meta VALUES('version', '24');
             CREATE TABLE cookies(creation_utc INTEGER NOT NULL, host_key TEXT NOT NULL,
               top_frame_site_key TEXT NOT NULL DEFAULT '', name TEXT NOT NULL,
               value TEXT NOT NULL, encrypted_value BLOB NOT NULL DEFAULT '',
               path TEXT NOT NULL DEFAULT '/', expires_utc INTEGER NOT NULL DEFAULT 0,
               is_secure INTEGER NOT NULL DEFAULT 1, is_httponly INTEGER NOT NULL DEFAULT 1);",
        )
        .unwrap();
        // A year from now, in Chromium's epoch.
        let later =
            (crate::browser_cookies::unix_now() + 365 * 86_400 + 11_644_473_600) * 1_000_000;
        for (host, name, value, encrypted) in rows {
            conn.execute(
                "INSERT INTO cookies(creation_utc, host_key, name, value, encrypted_value, expires_utc)
                 VALUES (1, ?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![host, name, value, encrypted, later],
            )
            .unwrap();
        }
    }

    #[test]
    fn macos_v10_with_the_safe_storage_key() {
        let home = temp_home("chromium-mac");
        let db = home.join("Cookies");
        let keys = Keys::mac(b"test-safe-storage-password");
        let key = &keys.v10[0];
        fixture_db(
            &db,
            &[
                (
                    ".youtube.com",
                    "SAPISID",
                    "",
                    seal_cbc(b"v10", key, ".youtube.com", "sap-1", true),
                ),
                (
                    ".youtube.com",
                    "SID",
                    "",
                    seal_cbc(b"v10", key, ".youtube.com", "sid-1", false),
                ),
                ("music.youtube.com", "PREF", "f6=8", Vec::new()),
                (
                    ".google.com",
                    "SID",
                    "",
                    seal_cbc(b"v10", key, ".google.com", "not-read", true),
                ),
            ],
        );
        let cookies = read(&db, &keys).unwrap();
        assert_eq!(cookies.len(), 3, "google.com isn't read");
        assert_eq!(
            cookie_header(&cookies).unwrap(),
            "PREF=f6=8; SAPISID=sap-1; SID=sid-1"
        );
        assert!(
            cookies
                .iter()
                .all(|c| c.expires.unwrap() > crate::browser_cookies::unix_now())
        );
        // Another key: the import fails instead of returning a broken session.
        assert_eq!(read(&db, &Keys::mac(b"wrong")), Err(ImportError::Decrypt));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn linux_v10_peanuts_and_v11_keyring() {
        let home = temp_home("chromium-linux");
        let db = home.join("Cookies");
        let keyring = b"keyring-secret";
        let keys = Keys::linux(Some(keyring));
        fixture_db(
            &db,
            &[
                (
                    ".youtube.com",
                    "SAPISID",
                    "",
                    seal_cbc(b"v11", &keys.v11[0], ".youtube.com", "sap-11", true),
                ),
                (
                    ".youtube.com",
                    "HSID",
                    "",
                    seal_cbc(b"v10", &Key::peanuts(), ".youtube.com", "hsid-10", false),
                ),
            ],
        );
        let header = cookie_header(&read(&db, &keys).unwrap()).unwrap();
        assert!(header.contains("SAPISID=sap-11") && header.contains("HSID=hsid-10"));
        // Without the keyring password the empty one is tried, and fails here.
        assert_eq!(read(&db, &Keys::linux(None)), Err(ImportError::Decrypt));
        // A browser that ran without a keyring used the empty password.
        let empty = Key::derive(b"", LINUX_ROUNDS);
        let db2 = home.join("Cookies2");
        fixture_db(
            &db2,
            &[(
                ".youtube.com",
                "SAPISID",
                "",
                seal_cbc(b"v11", &empty, ".youtube.com", "e", false),
            )],
        );
        assert_eq!(read(&db2, &Keys::linux(None)).unwrap()[0].value, "e");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn windows_gcm_and_app_bound() {
        let home = temp_home("chromium-win");
        let key = [42u8; 32];
        let db = home.join("Cookies");
        fixture_db(
            &db,
            &[(
                ".youtube.com",
                "SAPISID",
                "",
                seal_gcm(&key, ".youtube.com", "sap-gcm"),
            )],
        );
        let cookies = read(&db, &Keys::windows(key)).unwrap();
        assert_eq!(cookies[0].value, "sap-gcm");
        assert_eq!(
            read(&db, &Keys::windows([1; 32])),
            Err(ImportError::Decrypt)
        );
        // Chrome 127+: v20 can't be opened outside the browser; said so.
        let db2 = home.join("Cookies-v20");
        let mut v20 = seal_gcm(&key, ".youtube.com", "x");
        v20[..3].copy_from_slice(b"v20");
        fixture_db(&db2, &[(".youtube.com", "SAPISID", "", v20)]);
        assert_eq!(read(&db2, &Keys::windows(key)), Err(ImportError::AppBound));

        let state = format!(
            r#"{{"os_crypt":{{"encrypted_key":"{}"}}}}"#,
            base64::engine::general_purpose::STANDARD.encode(b"DPAPIsealed")
        );
        assert_eq!(windows_sealed_key(&state).unwrap(), b"sealed");
        assert!(matches!(
            windows_sealed_key("{}"),
            Err(ImportError::NoKey(_))
        ));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn keys_never_show_in_debug() {
        let k = Keys::mac(b"pw");
        let Key::Cbc(bytes) = &k.v10[0] else {
            unreachable!()
        };
        let shown = format!("{k:?}");
        assert!(shown.contains("<redacted>"), "{shown}");
        assert!(!shown.contains(&format!("{}, {}", bytes[0], bytes[1])));
        assert_eq!(expires(0), None);
        assert_eq!(expires(11_644_473_600 * 1_000_000 + 5_000_000), Some(5));
    }
}
