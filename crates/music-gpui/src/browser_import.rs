// Browser list and the "pick a browser" flow after refryt crates/refryt/src/browser_cookies.rs
// (detected, label_for, import) — © gridoe and refryt contributors, GPL-3.0.
//! "Sign in from your browser": imports the YouTube cookies of a browser profile on
//! this computer where the user is signed in to music.youtube.com, then signs in
//! with them like a pasted cookie (the account is checked with YouTube first).
//!
//! The reading and decrypting is `music_core::browser_cookies`; this module adds
//! the OS key each Chromium browser needs and the dialog:
//!
//! * macOS: the "<Browser> Safe Storage" password from the login Keychain. macOS asks
//!   the user whether this app may use it; the dialog says so beforehand. Safari
//!   needs Full Disk Access.
//! * Windows: `Local State`'s key opened with DPAPI (`CryptUnprotectData`, this
//!   user's). App-bound encryption (Chrome 127+ and others) can't be opened: those
//!   profiles are shown as blocked, with the reason.
//! * Linux: the password from libsecret (`secret-tool lookup application <name>`)
//!   or KWallet (`kwallet-query`), else Chromium's built-in fallbacks.
//!
//! Only the user's click starts an import; nothing runs in the background. Cookie
//! values are never logged.
//!
//! **Not verified against real browsers**: the readers are tested with fixture
//! databases and test keys; the Keychain prompt, DPAPI, libsecret / KWallet and real
//! stores were not exercised.

use gpui::{
    AnyElement, App, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, button::Button, h_flex,
    v_flex,
};
use music_core::browser_cookies::{
    self, Blocked, Browser, Engine, FoundProfile, ImportError, Os, chromium,
};

use crate::account::Busy;
use crate::app::{notify, update_app};
use crate::util::off_thread;

/// The browser profiles with a cookie store on this computer, most recently used
/// first (file names and times only; nothing is opened).
pub(crate) fn found_profiles() -> Vec<FoundProfile> {
    directories::BaseDirs::new()
        .map(|d| browser_cookies::discover(d.home_dir(), Os::CURRENT))
        .unwrap_or_default()
}

/// The keys for a Chromium profile's cookies (may show the OS's permission prompt).
fn chromium_keys(profile: &FoundProfile) -> Result<chromium::Keys, ImportError> {
    #[cfg(target_os = "macos")]
    {
        let (service, account) = profile
            .browser
            .safe_storage()
            .ok_or_else(|| ImportError::NoKey("no Keychain item known".into()))?; // i18n-ignore
        let entry = keyring::Entry::new(service, account)
            .map_err(|e| ImportError::NoKey(format!("Keychain: {e}")))?; // i18n-ignore
        let password = entry
            .get_password()
            .map_err(|e| ImportError::NoKey(format!("Keychain: {e}")))?; // i18n-ignore
        Ok(chromium::Keys::mac(password.as_bytes()))
    }
    #[cfg(target_os = "windows")]
    {
        let state = std::fs::read_to_string(profile.root.join("Local State")) // i18n-ignore
            .map_err(|_| ImportError::NoKey("no Local State".into()))?; // i18n-ignore
        let sealed = chromium::windows_sealed_key(&state)?;
        let key = dpapi_unprotect(&sealed)
            .ok_or_else(|| ImportError::NoKey("DPAPI refused the key".into()))?; // i18n-ignore
        let key: [u8; 32] = key
            .try_into()
            .map_err(|_| ImportError::NoKey("the key isn't 32 bytes".into()))?; // i18n-ignore
        Ok(chromium::Keys::windows(key))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let password = profile
            .browser
            .linux_keyring()
            .and_then(|(app, entry)| libsecret_password(app).or_else(|| kwallet_password(entry)));
        Ok(chromium::Keys::linux(password.as_deref()))
    }
}

/// DPAPI (`CryptUnprotectData`) of the current user. Windows only.
#[cfg(target_os = "windows")]
fn dpapi_unprotect(sealed: &[u8]) -> Option<Vec<u8>> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptUnprotectData};
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(sealed.len()).ok()?,
        pbData: sealed.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: `input` points at `sealed` for the call; DPAPI allocates `output` with
    // LocalAlloc, copied out and freed below.
    unsafe {
        CryptUnprotectData(&input, None, None, None, None, 0, &mut output).ok()?;
        if output.pbData.is_null() {
            return None;
        }
        let plain = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData as *mut core::ffi::c_void)));
        Some(plain)
    }
}

/// Linux: the cookie password in libsecret (`secret-tool`, if installed).
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn libsecret_password(application: &str) -> Option<Vec<u8>> {
    let out = std::process::Command::new("secret-tool")
        .args(["lookup", "application", application])
        .output()
        .ok()?;
    let value = out.stdout.trim_ascii_end().to_vec();
    (out.status.success() && !value.is_empty()).then_some(value)
}

/// Linux: the cookie password in KWallet (`kwallet-query`, if installed).
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn kwallet_password(entry: &str) -> Option<Vec<u8>> {
    let folder = entry.replace(" Safe Storage", " Keys");
    let out = std::process::Command::new("kwallet-query")
        .args(["--read-password", entry, "--folder", &folder, "kdewallet"])
        .output()
        .ok()?;
    let value = out.stdout.trim_ascii_end().to_vec();
    let missing = value.is_empty() || value.starts_with(b"Failed to read");
    (out.status.success() && !missing).then_some(value)
}

/// Reads `profile`'s YouTube cookies into a `Cookie` header. Blocking (and may show
/// the OS's permission prompt for the key).
pub(crate) fn import(profile: &FoundProfile) -> Result<String, ImportError> {
    let cookies = match profile.browser.engine() {
        Engine::Firefox => browser_cookies::firefox::read(&profile.cookies)?,
        Engine::Safari => browser_cookies::safari::read(&profile.cookies)?,
        Engine::Chromium => {
            if profile.blocked == Some(Blocked::AppBound) {
                return Err(ImportError::AppBound);
            }
            let keys = chromium_keys(profile)?;
            chromium::read(&profile.cookies, &keys)?
        }
    };
    browser_cookies::cookie_header(&cookies)
}

/// What the user reads when an import of `profile` failed.
pub(crate) fn describe(e: &ImportError, profile: &FoundProfile) -> String {
    let browser = profile.browser.label();
    match e {
        ImportError::NotFound => t!("browser_import.error.not_found", browser = browser),
        ImportError::Locked => t!("browser_import.error.locked", browser = browser),
        ImportError::PermissionDenied if profile.browser == Browser::Safari => {
            t!("browser_import.error.full_disk_access").to_owned()
        }
        ImportError::PermissionDenied => {
            t!("browser_import.error.permission", browser = browser)
        }
        ImportError::NoKey(_) if cfg!(target_os = "macos") => {
            t!("browser_import.error.keychain", browser = browser)
        }
        ImportError::NoKey(_) => t!("browser_import.error.no_key", browser = browser),
        ImportError::AppBound => t!("browser_import.error.app_bound", browser = browser),
        ImportError::Decrypt => t!("browser_import.error.decrypt", browser = browser),
        ImportError::NotSignedIn => t!(
            "browser_import.error.not_signed_in",
            browser = browser,
            profile = profile.profile
        ),
        ImportError::Format(_) => t!("browser_import.error.format", browser = browser),
    }
}

/// Imports `profile` in the background and signs in with the result.
fn start_import(profile: FoundProfile, window: &mut Window, cx: &mut App) {
    update_app(cx, |app, cx| {
        app.store.account.status = Busy::Working(t!(
            "browser_import.reading",
            browser = profile.browser.label()
        ));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let which = profile.clone();
            let result = off_thread(move || import(&which)).await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(cookie) => {
                    this.store.account.status = Busy::Idle;
                    this.sign_in_from(cookie, 0, None, window, cx);
                }
                Err(e) => {
                    eprintln!(
                        "music-gpui: browser import from {} failed: {e}",
                        profile.browser.label()
                    );
                    let message = describe(&e, &profile);
                    this.store.account.status = Busy::Failed(message.clone());
                    notify(window, cx, message);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    });
}

/// The note for a blocked profile.
fn blocked_text(blocked: Blocked) -> &'static str {
    match blocked {
        Blocked::AppBound => t!("browser_import.blocked.app_bound"),
        Blocked::NoKey => t!("browser_import.blocked.no_key"),
        Blocked::FullDiskAccess => t!("browser_import.blocked.full_disk_access"),
    }
}

/// One row per browser profile.
fn rows(found: &[FoundProfile], cx: &App) -> Vec<AnyElement> {
    let muted = cx.theme().muted_foreground;
    found
        .iter()
        .enumerate()
        .map(|(ix, p)| {
            let label = if p.profile == p.browser.label() {
                p.browser.label().to_owned()
            } else {
                format!("{} · {}", p.browser.label(), p.profile)
            };
            let detail = match (p.blocked, p.used) {
                (Some(b), _) => blocked_text(b).to_owned(),
                (None, Some(at)) => {
                    let ms = at
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or_default();
                    t!(
                        "browser_import.last_used",
                        date = crate::util::pretty_date(&crate::util::local_date(ms))
                    )
                }
                (None, None) => String::new(),
            };
            let profile = p.clone();
            h_flex()
                .gap_3()
                .py_1()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .child(SharedString::from(label)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .whitespace_normal()
                                .child(SharedString::from(detail)),
                        ),
                )
                .child(
                    Button::new(("browser-import", ix))
                        .small()
                        .outline()
                        .label(t!("browser_import.import"))
                        .disabled(p.blocked == Some(Blocked::AppBound))
                        .on_click(move |_, window, cx| {
                            window.close_dialog(cx);
                            start_import(profile.clone(), window, cx);
                        }),
                )
                .into_any_element()
        })
        .collect()
}

/// Opens the "Sign in from your browser" dialog.
pub fn open_dialog(window: &mut Window, cx: &mut App) {
    let found = found_profiles();
    window.open_dialog(cx, move |dialog, _, cx| {
        let muted = cx.theme().muted_foreground;
        let note = |text: String| {
            div()
                .text_sm()
                .text_color(muted)
                .whitespace_normal()
                .child(SharedString::from(text))
        };
        let mut body = v_flex()
            .gap_3()
            .child(note(t!("browser_import.intro").to_owned()));
        if cfg!(target_os = "macos") && found.iter().any(|p| p.browser.engine() == Engine::Chromium)
        {
            body = body.child(note(t!("browser_import.note.keychain").to_owned()));
        }
        if cfg!(windows) {
            body = body.child(note(t!("browser_import.note.app_bound").to_owned()));
        }
        if found.is_empty() {
            body = body.child(note(t!("browser_import.none_found").to_owned()));
        } else {
            body = body.child(
                v_flex()
                    .id("browser-import-list")
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .gap_1()
                    .children(rows(&found, cx)),
            );
        }
        dialog
            .title(div().child(t!("browser_import.title")))
            .w(px(560.))
            .child(body)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(browser: Browser, blocked: Option<Blocked>) -> FoundProfile {
        FoundProfile {
            browser,
            profile: "Work".into(),
            cookies: "/nonexistent/Cookies".into(),
            root: "/nonexistent".into(),
            used: None,
            blocked,
        }
    }

    /// Every failure reads as a sentence naming the browser, never a path or value.
    #[test]
    fn failures_are_explained() {
        let chrome = profile(Browser::Chrome, None);
        for e in [
            ImportError::NotFound,
            ImportError::Locked,
            ImportError::PermissionDenied,
            ImportError::NoKey("Keychain: denied".into()),
            ImportError::AppBound,
            ImportError::Decrypt,
            ImportError::NotSignedIn,
            ImportError::Format("x".into()),
        ] {
            let text = describe(&e, &chrome);
            assert!(text.contains("Chrome"), "{e:?}: {text}");
            assert!(!text.contains("/nonexistent"), "{text}");
        }
        let safari = profile(Browser::Safari, None);
        assert!(describe(&ImportError::PermissionDenied, &safari).contains("Full Disk Access"));
        // An app-bound profile isn't even opened.
        let sealed = profile(Browser::Edge, Some(Blocked::AppBound));
        assert_eq!(import(&sealed), Err(ImportError::AppBound));
        // A Firefox profile that's gone.
        let gone = profile(Browser::Firefox, None);
        assert_eq!(import(&gone), Err(ImportError::NotFound));
    }
}
