//! Microphone permission (macOS TCC) for song recognition.
//!
//! macOS asks the user the first time an app records; an app bundle must carry
//! `NSMicrophoneUsageDescription` (see `scripts/bundle-macos.sh`). [`ensure`] reads
//! the status through AVFoundation (`AVCaptureDevice`) and, if it was never asked,
//! shows the system prompt and waits for the answer. Other platforms have no such
//! gate here (Linux / Windows privacy settings surface as capture errors).

/// What the OS says about recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicPermission {
    Granted,
    Denied,
    /// Blocked by policy (parental controls, MDM).
    Restricted,
    /// Not asked yet.
    Undetermined,
}

/// The current status without prompting.
pub fn status() -> MicPermission {
    imp::status()
}

/// Blocks until permission is known, prompting if needed (call off the UI thread).
pub fn ensure() -> MicPermission {
    match status() {
        MicPermission::Undetermined => imp::request(),
        other => other,
    }
}

/// Opens the OS privacy settings for the microphone.
pub fn open_settings(cx: &mut gpui::App) {
    if cfg!(target_os = "macos") {
        cx.open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone");
    } else if cfg!(target_os = "windows") {
        cx.open_url("ms-settings:privacy-microphone");
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::sync::Mutex;
    use std::sync::mpsc;
    use std::time::Duration;

    use block2::RcBlock;
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, Bool};
    use objc2_foundation::NSString;

    use super::MicPermission;

    #[link(name = "AVFoundation", kind = "framework")]
    unsafe extern "C" {
        static AVMediaTypeAudio: &'static NSString;
    }

    fn device_class() -> Option<&'static AnyClass> {
        AnyClass::get(c"AVCaptureDevice")
    }

    pub fn status() -> MicPermission {
        let Some(class) = device_class() else {
            return MicPermission::Granted;
        };
        // AVAuthorizationStatus: 0 not determined, 1 restricted, 2 denied, 3 authorized.
        let status: isize =
            unsafe { msg_send![class, authorizationStatusForMediaType: AVMediaTypeAudio] };
        match status {
            0 => MicPermission::Undetermined,
            1 => MicPermission::Restricted,
            2 => MicPermission::Denied,
            _ => MicPermission::Granted,
        }
    }

    pub fn request() -> MicPermission {
        let Some(class) = device_class() else {
            return MicPermission::Granted;
        };
        let (tx, rx) = mpsc::channel::<bool>();
        let tx = Mutex::new(Some(tx));
        let block = RcBlock::new(move |granted: Bool| {
            if let Some(tx) = tx.lock().ok().and_then(|mut t| t.take()) {
                let _ = tx.send(granted.as_bool());
            }
        });
        unsafe {
            let _: () = msg_send![
                class,
                requestAccessForMediaType: AVMediaTypeAudio,
                completionHandler: &*block
            ];
        }
        // The user may leave the prompt open; give up (as "not determined") after 2 min.
        match rx.recv_timeout(Duration::from_secs(120)) {
            Ok(true) => MicPermission::Granted,
            Ok(false) => MicPermission::Denied,
            Err(_) => MicPermission::Undetermined,
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::MicPermission;

    pub fn status() -> MicPermission {
        MicPermission::Granted
    }

    pub fn request() -> MicPermission {
        MicPermission::Granted
    }
}
