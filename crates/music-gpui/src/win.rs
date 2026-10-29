//! Windows-only shell pieces (plain Win32, no GPUI types, so `cargo check` for a
//! Windows target needs none of GPUI's C dependencies):
//!
//! * [`attach_console`] – release builds are GUI-subsystem programs (no console
//!   window behind the app); `MUSIC_CONSOLE=1` gets the logs back.
//! * [`single_instance`] / [`InstanceGuard::listen`] – a second launch brings the
//!   running app's window to the front instead of starting another app.
//! * [`set_topmost`] – the mini player stays above other windows (as the floating
//!   panel does on macOS).
//! * [`reduce_motion`] – the system's "Show animations in Windows" switch.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, HWND, LPARAM, LRESULT,
    WAIT_ABANDONED, WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE,
    STD_OUTPUT_HANDLE, SetStdHandle,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, FindWindowExW,
    GetWindowThreadProcessId, HWND_MESSAGE, HWND_TOPMOST, IsIconic, RegisterClassW,
    RegisterWindowMessageW, SMTO_ABORTIFHUNG, SPI_GETCLIENTAREAANIMATION, SW_RESTORE, SW_SHOW,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    SendMessageTimeoutW, SetForegroundWindow, SetWindowPos, ShowWindow, SystemParametersInfoW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW,
};
use windows::core::{BOOL, HSTRING, PCWSTR, w};

// ---- console -----------------------------------------------------------------------

/// `MUSIC_CONSOLE=1`: attach to the console of the terminal that started the app,
/// or open a new console window, and point stdout / stderr at it (a GUI-subsystem
/// process starts without them). Output already redirected to a file or pipe is
/// left alone. Closing that console also ends the app, as for any console program.
pub fn attach_console() {
    if std::env::var_os("MUSIC_CONSOLE").is_none_or(|v| v == "0") {
        return;
    }
    // SAFETY: plain Win32 calls on this process's own console / std handles.
    unsafe {
        if has_handle(STD_ERROR_HANDLE) && has_handle(STD_OUTPUT_HANDLE) {
            return;
        }
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() && AllocConsole().is_err() {
            return;
        }
        for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            if has_handle(which) {
                continue;
            }
            let console = CreateFileW(
                w!("CONOUT$"),
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            );
            if let Ok(console) = console {
                let _ = SetStdHandle(which, console);
            }
        }
    }
}

/// Whether a std handle points somewhere (a GUI process may have none).
unsafe fn has_handle(which: STD_HANDLE) -> bool {
    // SAFETY: a plain query of this process's std handle table.
    unsafe { GetStdHandle(which) }.is_ok_and(|h| !h.is_invalid() && !h.0.is_null())
}

// ---- single instance -----------------------------------------------------------------

/// Holds the instance mutex for the process's lifetime (released when it exits).
pub struct InstanceGuard {
    _mutex: HANDLE,
    key: String,
}

/// What a launch should do.
pub enum Instance {
    /// No other copy runs (for this settings folder): start normally.
    First(InstanceGuard),
    /// Another copy runs and was asked to show its window: exit.
    Forwarded,
}

/// The running app's main window, answered to a second launch.
static MAIN_WINDOW: AtomicIsize = AtomicIsize::new(0);

/// `key` names the instance (one per settings folder, so a test copy with its own
/// `MUSIC_CONFIG_DIR` still starts). If another copy holds the mutex, its hidden
/// "instance" window is asked for the main window, which is restored and brought to
/// the front. A copy that is still starting (no window yet) or just quitting (an
/// update relaunch) is waited for up to 5 s; after that this copy starts anyway.
pub fn single_instance(key: &str) -> Instance {
    let name = HSTRING::from(format!("Local\\{key}"));
    // SAFETY: plain Win32 calls; the handle is kept (or dropped with the process).
    let mutex = match unsafe { CreateMutexW(None, true, &name) } {
        Ok(handle) => handle,
        // No mutex (very unusual): don't stand in the way of starting.
        Err(_) => {
            return Instance::First(InstanceGuard {
                _mutex: HANDLE::default(),
                key: key.to_owned(),
            });
        }
    };
    // SAFETY: reads the error of the call just made on this thread.
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    let guard = InstanceGuard {
        _mutex: mutex,
        key: key.to_owned(),
    };
    if !existed {
        return Instance::First(guard);
    }
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if forward_to_running(key) {
            return Instance::Forwarded;
        }
        // SAFETY: waits on our own handle to the named mutex.
        let wait = unsafe { WaitForSingleObject(mutex, 100) };
        if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
            // The other copy has quit: this one is the instance now.
            return Instance::First(guard);
        }
    }
    Instance::First(guard)
}

fn class_name(key: &str) -> HSTRING {
    HSTRING::from(format!("{key}.instance"))
}

fn activate_message() -> u32 {
    // SAFETY: registers (or looks up) a message name; no pointers kept.
    unsafe { RegisterWindowMessageW(w!("CrateTune.ShowMainWindow")) }
}

/// Asks the running copy for its main window and brings it forward. `false` if no
/// running copy answered.
fn forward_to_running(key: &str) -> bool {
    let class = class_name(key);
    // SAFETY: Win32 window lookups / messages; nothing is dereferenced.
    unsafe {
        let Ok(listener) = FindWindowExW(Some(HWND_MESSAGE), None, &class, PCWSTR::null()) else {
            return false;
        };
        // Let the running copy take the foreground (this launch has the right to).
        let mut pid = 0u32;
        GetWindowThreadProcessId(listener, Some(&mut pid));
        if pid != 0 {
            let _ = AllowSetForegroundWindow(pid);
        }
        let mut answer = 0usize;
        let sent = SendMessageTimeoutW(
            listener,
            activate_message(),
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            2000,
            Some(&mut answer),
        );
        if sent.0 == 0 || answer == 0 {
            return false;
        }
        show_and_focus(HWND(answer as *mut _));
        true
    }
}

/// Restores a minimized window and brings it to the front.
fn show_and_focus(hwnd: HWND) {
    // SAFETY: Win32 calls on a window handle (stale handles just fail).
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        } else {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

unsafe extern "system" fn listener_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == activate_message() {
        let main = MAIN_WINDOW.load(Ordering::Relaxed);
        if main != 0 {
            // The second launch focuses it too; doing it here as well covers the case
            // where Windows only lets this (the owning) process do it.
            show_and_focus(HWND(main as *mut _));
        }
        return LRESULT(main);
    }
    // SAFETY: default handling for everything else.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

impl InstanceGuard {
    /// Answers later launches with `main` (the main window's HWND): a hidden
    /// message-only window on this (the UI) thread, served by GPUI's message loop.
    pub fn listen(&self, main: isize) {
        MAIN_WINDOW.store(main, Ordering::Relaxed);
        let class = class_name(&self.key);
        // SAFETY: registers a window class whose procedure touches no Rust state but
        // an atomic, and creates one message-only window of it (kept for the
        // process's lifetime).
        unsafe {
            let Ok(module) = GetModuleHandleW(None) else {
                return;
            };
            let wc = WNDCLASSW {
                lpfnWndProc: Some(listener_proc),
                hInstance: module.into(),
                lpszClassName: PCWSTR(class.as_ptr()),
                ..Default::default()
            };
            if RegisterClassW(&wc) == 0 {
                return;
            }
            let _ = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                &class,
                &class,
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(module.into()),
                None,
            );
        }
    }
}

/// A short, stable name for the settings folder (FNV-1a of its path), so each
/// settings folder has its own instance.
pub fn instance_key(app_id: &str, config_dir: Option<&std::path::Path>) -> String {
    let path = config_dir
        .map(|p| p.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{app_id}-{hash:016x}")
}

// ---- windows ---------------------------------------------------------------------------

/// Keeps `hwnd` above normal windows (the mini player).
pub fn set_topmost(hwnd: isize) {
    // SAFETY: a style change on a window of this process.
    unsafe {
        let _ = SetWindowPos(
            HWND(hwnd as *mut _),
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

/// Settings → Accessibility → Visual effects → "Animation effects" is off.
pub fn reduce_motion() -> bool {
    let mut enabled = BOOL(1);
    // SAFETY: SPI_GETCLIENTAREAANIMATION writes one BOOL to the pointer.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some((&mut enabled as *mut BOOL).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    ok.is_ok() && !enabled.as_bool()
}

#[cfg(test)]
mod tests {
    #[test]
    fn instance_keys_differ_per_folder() {
        let a = super::instance_key("app", Some(std::path::Path::new(r"C:\Users\a\cfg")));
        let b = super::instance_key("app", Some(std::path::Path::new(r"C:\Users\b\cfg")));
        let a2 = super::instance_key("app", Some(std::path::Path::new(r"c:\users\A\CFG")));
        assert_ne!(a, b);
        assert_eq!(a, a2);
        assert!(a.starts_with("app-"));
    }
}
