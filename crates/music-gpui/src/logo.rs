//! The app logo (packaging/icons/app-icon.svg, rendered by scripts/make-icons.sh):
//! the in-app mark (sidebar, About, welcome guide) and, when running outside an app
//! bundle (`cargo run`, the bare binary), the Dock icon.

use std::sync::{Arc, OnceLock};

use gpui::{Image, ImageFormat, IntoElement, ObjectFit, Styled as _, StyledImage as _, img, px};

/// The tile edge to edge, no shadow, 256 px.
const LOGO_PNG: &[u8] = include_bytes!("../../../packaging/icons/app-logo-256.png");

/// The logo as an element of `size` × `size` points.
pub fn logo(size: f32) -> impl IntoElement {
    static IMAGE: OnceLock<Arc<Image>> = OnceLock::new();
    let image = IMAGE
        .get_or_init(|| Arc::new(Image::from_bytes(ImageFormat::Png, LOGO_PNG.to_vec())))
        .clone();
    img(image)
        .size(px(size))
        .flex_none()
        .object_fit(ObjectFit::Contain)
}

/// Shows the app icon in the Dock and the app switcher when the binary runs outside
/// `CrateTune.app` (an app bundle gets it from `Info.plist`). Call after the
/// application has started.
#[cfg(target_os = "macos")]
pub fn set_dock_icon() {
    if !bundled() {
        set_dock_icon_png(None);
    }
}

#[cfg(target_os = "macos")]
fn bundled() -> bool {
    std::env::current_exe().is_ok_and(|exe| exe.to_string_lossy().contains(".app/Contents/MacOS/"))
}

/// Sets the Dock / app switcher icon (Appearance → Window → App icon, `app_icon.rs`):
/// `png`, or with `None` the app's own (the bundle's icon; outside a bundle the
/// full icon with its margin and shadow, like the bundle's `.icns`).
#[cfg(target_os = "macos")]
pub fn set_dock_icon_png(png: Option<&[u8]>) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    const ICON_PNG: &[u8] = include_bytes!("../../../packaging/icons/app-icon-512.png");

    let (Some(data_class), Some(image_class), Some(app_class)) = (
        AnyClass::get(c"NSData"),
        AnyClass::get(c"NSImage"),
        AnyClass::get(c"NSApplication"),
    ) else {
        return;
    };
    let bytes = match png {
        Some(bytes) => bytes,
        None if bundled() => {
            // SAFETY: main thread; `setApplicationIconImage:` with nil restores the
            // bundle's icon.
            unsafe {
                let app: *mut AnyObject = msg_send![app_class, sharedApplication];
                let none: *mut AnyObject = std::ptr::null_mut();
                let _: () = msg_send![app, setApplicationIconImage: none];
            }
            return;
        }
        None => ICON_PNG,
    };
    // SAFETY: main thread (called from the app's run callback or a render);
    // standard Foundation / AppKit messages. `dataWithBytes:length:` copies the
    // bytes; the NSImage is retained by NSApp after `setApplicationIconImage:`, so
    // our +1 is released.
    unsafe {
        let data: *mut AnyObject = msg_send![
            data_class,
            dataWithBytes: bytes.as_ptr().cast::<std::ffi::c_void>(),
            length: bytes.len()
        ];
        if data.is_null() {
            return;
        }
        let image: *mut AnyObject = msg_send![image_class, alloc];
        let image: *mut AnyObject = msg_send![image, initWithData: data];
        if image.is_null() {
            return;
        }
        let app: *mut AnyObject = msg_send![app_class, sharedApplication];
        let _: () = msg_send![app, setApplicationIconImage: image];
        let _: () = msg_send![image, release];
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set_dock_icon() {}
