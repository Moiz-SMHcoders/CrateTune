//! Keeps the computer from falling asleep while music plays or a Listen together
//! session runs (the display may still sleep): a sleeping laptop drops its network
//! links, and a session dies with them.
//!
//! * macOS: an IOKit power assertion (`PreventUserIdleSystemSleep`).
//! * Windows: `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)` on the
//!   calling thread (call [`KeepAwake::set`] from one long-lived thread, the UI's).
//! * Linux: a `systemd-inhibit --what=idle` child process while held (logind's idle
//!   inhibitor: no automatic suspend; an explicit suspend still works), where
//!   `systemd-inhibit` exists.
//! * Elsewhere nothing happens.
//!
//! [`KeepAwake`] holds the state: it asks the system only when what is wanted changes.

/// Whether the computer should stay awake: the setting, and music playing or a
/// Listen together session running (host or guest, playing or not).
pub fn wanted(setting: bool, playing: bool, in_session: bool) -> bool {
    setting && (playing || in_session)
}

/// What asks the system (a fake in tests).
pub trait Inhibitor {
    /// `false` when the system refused or can't do it.
    fn acquire(&mut self) -> bool;
    fn release(&mut self);
}

/// Holds the system's "stay awake" while wanted ([`Self::set`] with every poll).
pub struct KeepAwake<I: Inhibitor = SystemInhibitor> {
    inhibitor: I,
    held: bool,
    /// The system said no: not asked again until it is released and wanted anew.
    refused: bool,
}

impl KeepAwake<SystemInhibitor> {
    /// `app`: the app's name, `reason`: why (shown by some systems).
    pub fn new(app: &str, reason: &str) -> Self {
        Self::with(SystemInhibitor::new(app, reason))
    }
}

impl<I: Inhibitor> KeepAwake<I> {
    pub fn with(inhibitor: I) -> Self {
        Self {
            inhibitor,
            held: false,
            refused: false,
        }
    }

    /// Holds or releases as `wanted` says; returns whether it changed.
    pub fn set(&mut self, wanted: bool) -> bool {
        match (wanted, self.held) {
            (true, false) if !self.refused => {
                if self.inhibitor.acquire() {
                    self.held = true;
                    log::info!("keep awake: on");
                    true
                } else {
                    self.refused = true;
                    log::warn!("keep awake: the system refused");
                    false
                }
            }
            (false, true) => {
                self.inhibitor.release();
                self.held = false;
                log::info!("keep awake: off");
                true
            }
            (false, false) => {
                self.refused = false;
                false
            }
            _ => false,
        }
    }

    pub fn held(&self) -> bool {
        self.held
    }
}

impl<I: Inhibitor> Drop for KeepAwake<I> {
    fn drop(&mut self) {
        if self.held {
            self.inhibitor.release();
        }
    }
}

/// The platform's way (see the module docs).
pub struct SystemInhibitor {
    #[allow(dead_code)]
    app: String,
    #[allow(dead_code)]
    reason: String,
    #[cfg(target_os = "macos")]
    assertion: Option<u32>,
    #[cfg(target_os = "linux")]
    child: Option<std::process::Child>,
}

impl SystemInhibitor {
    pub fn new(app: &str, reason: &str) -> Self {
        Self {
            app: app.to_owned(),
            reason: reason.to_owned(),
            #[cfg(target_os = "macos")]
            assertion: None,
            #[cfg(target_os = "linux")]
            child: None,
        }
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::{CString, c_char, c_void};

    pub type CFStringRef = *const c_void;
    pub const UTF8: u32 = 0x0800_0100;
    pub const LEVEL_ON: u32 = 255;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            text: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
        fn CFRelease(object: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        pub fn IOPMAssertionCreateWithName(
            kind: CFStringRef,
            level: u32,
            name: CFStringRef,
            id: *mut u32,
        ) -> i32;
        pub fn IOPMAssertionRelease(id: u32) -> i32;
    }

    /// A CoreFoundation string (released on drop).
    pub struct CfString(pub CFStringRef);

    impl CfString {
        pub fn new(text: &str) -> Option<Self> {
            let text = CString::new(text).ok()?;
            // SAFETY: a valid NUL-terminated UTF-8 string; the default allocator.
            let s = unsafe { CFStringCreateWithCString(std::ptr::null(), text.as_ptr(), UTF8) };
            (!s.is_null()).then_some(Self(s))
        }
    }

    impl Drop for CfString {
        fn drop(&mut self) {
            // SAFETY: created by CFStringCreateWithCString, released once.
            unsafe { CFRelease(self.0) }
        }
    }
}

#[cfg(target_os = "macos")]
impl Inhibitor for SystemInhibitor {
    fn acquire(&mut self) -> bool {
        let (Some(kind), Some(name)) = (
            mac::CfString::new("PreventUserIdleSystemSleep"),
            mac::CfString::new(&format!("{}: {}", self.app, self.reason)),
        ) else {
            return false;
        };
        let mut id = 0u32;
        // SAFETY: valid CF strings; `id` is written on success.
        let ok =
            unsafe { mac::IOPMAssertionCreateWithName(kind.0, mac::LEVEL_ON, name.0, &mut id) };
        if ok == 0 {
            self.assertion = Some(id);
            true
        } else {
            false
        }
    }

    fn release(&mut self) {
        if let Some(id) = self.assertion.take() {
            // SAFETY: an assertion this inhibitor created, released once.
            unsafe { mac::IOPMAssertionRelease(id) };
        }
    }
}

#[cfg(target_os = "windows")]
mod win {
    pub const ES_CONTINUOUS: u32 = 0x8000_0000;
    pub const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn SetThreadExecutionState(flags: u32) -> u32;
    }
}

#[cfg(target_os = "windows")]
impl Inhibitor for SystemInhibitor {
    fn acquire(&mut self) -> bool {
        // SAFETY: plain flags; the state belongs to the calling thread.
        unsafe { win::SetThreadExecutionState(win::ES_CONTINUOUS | win::ES_SYSTEM_REQUIRED) != 0 }
    }

    fn release(&mut self) {
        // SAFETY: as above; back to normal.
        unsafe { win::SetThreadExecutionState(win::ES_CONTINUOUS) };
    }
}

#[cfg(target_os = "linux")]
impl Inhibitor for SystemInhibitor {
    fn acquire(&mut self) -> bool {
        let child = std::process::Command::new("systemd-inhibit")
            .arg("--what=idle")
            .arg("--mode=block")
            .arg(format!("--who={}", self.app))
            .arg(format!("--why={}", self.reason))
            .args(["sleep", "infinity"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match child {
            Ok(child) => {
                self.child = Some(child);
                true
            }
            Err(e) => {
                log::debug!("keep awake: systemd-inhibit: {e}");
                false
            }
        }
    }

    fn release(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
impl Inhibitor for SystemInhibitor {
    fn acquire(&mut self) -> bool {
        false
    }

    fn release(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        acquired: u32,
        released: u32,
        refuse: bool,
    }

    impl Inhibitor for &mut Fake {
        fn acquire(&mut self) -> bool {
            self.acquired += 1;
            !self.refuse
        }

        fn release(&mut self) {
            self.released += 1;
        }
    }

    #[test]
    fn holds_only_while_wanted_and_asks_only_on_changes() {
        // Wanted: the setting, and playing or in a session.
        assert!(wanted(true, true, false));
        assert!(wanted(true, false, true));
        assert!(!wanted(true, false, false));
        assert!(!wanted(false, true, true));

        let mut fake = Fake::default();
        {
            let mut keep = KeepAwake::with(&mut fake);
            assert!(keep.set(true));
            // Every poll says the same: the system isn't asked again.
            for _ in 0..10 {
                assert!(!keep.set(true));
            }
            assert!(keep.held());
            assert!(keep.set(false));
            assert!(!keep.set(false));
            assert!(keep.set(true));
            // Dropped while held (the app quits): released.
        }
        assert_eq!((fake.acquired, fake.released), (2, 2));

        // Refused: not asked again until it is no longer wanted.
        let mut fake = Fake {
            refuse: true,
            ..Fake::default()
        };
        {
            let mut keep = KeepAwake::with(&mut fake);
            assert!(!keep.set(true));
            assert!(!keep.set(true));
            assert!(!keep.held());
            keep.set(false);
            keep.set(true);
        }
        assert_eq!((fake.acquired, fake.released), (2, 0));
    }
}
