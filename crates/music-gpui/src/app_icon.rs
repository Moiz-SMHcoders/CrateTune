//! Appearance → Window → App icon: the ク logo in a few colours
//! (`packaging/icons/variants/*.png`, rendered by `scripts/make-icons.sh`) or
//! the user's own image (copied to `<config>/app-icon.png`, fitted into
//! 512 × 512). Applied while the app runs:
//!
//! * **macOS** – the Dock and app switcher (`NSApplication
//!   setApplicationIconImage:`); "Midnight" (the default) restores the
//!   bundle's icon. The Dock shows the bundle's icon again once the app quits.
//! * **Windows** – the main window's big and small icon (`WM_SETICON`: title
//!   bar, Alt+Tab, taskbar button); the default restores the .exe's icon.
//!   Shortcuts and the Start menu keep the installed icon.
//! * **Linux** – X11's `_NET_WM_ICON`, which GPUI sets only when a window
//!   opens: the choice applies on the next start. Wayland compositors show the
//!   `.desktop` file's icon.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::settings::theme_prefs::choice_enum;

choice_enum! {
    /// Which app icon is shown.
    AppIconChoice {
        /// The navy tile (the bundle's icon).
        #[default]
        Default = "default" => "app_icon.default",
        Light = "light" => "app_icon.light",
        Sunset = "sunset" => "app_icon.sunset",
        Mint = "mint" => "app_icon.mint",
        Mono = "mono" => "app_icon.mono",
        /// The user's image (`<config>/app-icon.png`).
        Custom = "custom" => "app_icon.custom",
    }
}

const DEFAULT_PNG: &[u8] = include_bytes!("../../../packaging/icons/app-icon-512.png");
const LIGHT_PNG: &[u8] = include_bytes!("../../../packaging/icons/variants/light.png");
const SUNSET_PNG: &[u8] = include_bytes!("../../../packaging/icons/variants/sunset.png");
const MINT_PNG: &[u8] = include_bytes!("../../../packaging/icons/variants/mint.png");
const MONO_PNG: &[u8] = include_bytes!("../../../packaging/icons/variants/mono.png");
/// The size a user's image is fitted into.
const CUSTOM_SIZE: u32 = 512;
/// Images larger than this aren't read.
const MAX_INPUT_BYTES: u64 = 32 * 1024 * 1024;

/// `<config>/app-icon.png`.
pub fn custom_path() -> Option<PathBuf> {
    crate::settings::file::config_dir().map(|d| d.join("app-icon.png"))
}

/// The icon's PNG (512 px). The custom choice without a readable file falls
/// back to the default.
pub fn png(choice: AppIconChoice) -> Cow<'static, [u8]> {
    match choice {
        AppIconChoice::Default => Cow::Borrowed(DEFAULT_PNG),
        AppIconChoice::Light => Cow::Borrowed(LIGHT_PNG),
        AppIconChoice::Sunset => Cow::Borrowed(SUNSET_PNG),
        AppIconChoice::Mint => Cow::Borrowed(MINT_PNG),
        AppIconChoice::Mono => Cow::Borrowed(MONO_PNG),
        AppIconChoice::Custom => custom_path()
            .and_then(|p| std::fs::read(p).ok())
            .map_or(Cow::Borrowed(DEFAULT_PNG), Cow::Owned),
    }
}

/// What the platform's icon becomes: `None` = its own (bundle / .exe) icon.
pub fn effective(choice: AppIconChoice) -> Option<Cow<'static, [u8]>> {
    let has_custom = || custom_path().is_some_and(|p| p.is_file());
    match choice {
        AppIconChoice::Default => None,
        AppIconChoice::Custom if !has_custom() => None,
        other => Some(png(other)),
    }
}

/// Reads an image (PNG, JPEG, WebP), fits it into a transparent 512 × 512
/// square and writes it as `dest` (PNG). Runs on a worker thread.
pub fn import_custom(src: &Path, dest: &Path) -> Result<(), String> {
    use image::{GenericImageView as _, ImageFormat, RgbaImage, imageops};
    let len = std::fs::metadata(src).map_err(|e| e.to_string())?.len();
    if len > MAX_INPUT_BYTES {
        return Err(t!("app_icon.error.not_png").to_owned());
    }
    let bytes = std::fs::read(src).map_err(|e| e.to_string())?;
    let format = image::guess_format(&bytes)
        .ok()
        .filter(|f| matches!(f, ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP))
        .ok_or_else(|| t!("app_icon.error.not_png").to_owned())?;
    let image = image::load_from_memory_with_format(&bytes, format).map_err(|e| e.to_string())?;
    let (w, h) = image.dimensions();
    if w == 0 || h == 0 {
        return Err(t!("app_icon.error.not_png").to_owned());
    }
    let scale = CUSTOM_SIZE as f32 / w.max(h) as f32;
    let (nw, nh) = (
        ((w as f32 * scale).round() as u32).max(1),
        ((h as f32 * scale).round() as u32).max(1),
    );
    let fitted = imageops::resize(&image.to_rgba8(), nw, nh, imageops::FilterType::Lanczos3);
    let mut out = RgbaImage::new(CUSTOM_SIZE, CUSTOM_SIZE);
    imageops::overlay(
        &mut out,
        &fitted,
        ((CUSTOM_SIZE - nw) / 2).into(),
        ((CUSTOM_SIZE - nh) / 2).into(),
    );
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = dest.with_extension("png.tmp");
    out.save_with_format(&tmp, ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dest).map_err(|e| e.to_string())
}

/// X11: the icon GPUI sets when the main window opens (`None`: the default,
/// left to the `.desktop` file as before).
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub fn window_icon(choice: AppIconChoice) -> Option<std::sync::Arc<image::RgbaImage>> {
    let png = effective(choice)?;
    let image = image::load_from_memory_with_format(&png, image::ImageFormat::Png).ok()?;
    // 128 px is plenty for title bars and task switchers (the property is 4 bytes a pixel).
    Some(std::sync::Arc::new(
        image
            .resize(128, 128, image::imageops::FilterType::Triangle)
            .to_rgba8(),
    ))
}

/// Shows `choice` as the running app's icon (see the module docs).
pub fn apply(choice: AppIconChoice, window: &gpui::Window) {
    let png = effective(choice);
    #[cfg(target_os = "macos")]
    {
        let _ = window;
        crate::logo::set_dock_icon_png(png.as_deref());
    }
    #[cfg(windows)]
    if let Some(hwnd) = crate::services::hwnd(window) {
        crate::win::set_window_icon(hwnd.get(), png.as_deref());
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = (window, png);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_is_a_512_png_and_custom_falls_back() {
        for choice in AppIconChoice::ALL {
            let png = png(*choice);
            let image = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
                .unwrap_or_else(|e| panic!("{choice:?}: {e}"));
            assert_eq!((image.width(), image.height()), (512, 512), "{choice:?}");
        }
        assert!(effective(AppIconChoice::Default).is_none());
        assert!(effective(AppIconChoice::Sunset).is_some());
        assert_eq!(AppIconChoice::parse("mint"), Some(AppIconChoice::Mint));
        let s: crate::settings::file::Settings =
            serde_json::from_str(r#"{"appearance": {"app_icon": "neon"}}"#).unwrap();
        assert_eq!(s.appearance.app_icon, AppIconChoice::Default);
        let s: crate::settings::file::Settings =
            serde_json::from_str(r#"{"appearance": {"app_icon": "sunset"}}"#).unwrap();
        assert_eq!(s.appearance.app_icon, AppIconChoice::Sunset);
    }

    #[test]
    fn a_users_image_is_fitted_into_a_square() {
        let dir = std::env::temp_dir().join(format!("cratetune-icon-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("wide.png");
        image::RgbaImage::from_pixel(300, 100, image::Rgba([200, 30, 30, 255]))
            .save(&src)
            .unwrap();
        let dest = dir.join("out/app-icon.png");
        import_custom(&src, &dest).unwrap();
        let out = image::open(&dest).unwrap().to_rgba8();
        assert_eq!(out.dimensions(), (512, 512));
        assert_eq!(out.get_pixel(256, 256).0, [200, 30, 30, 255]);
        assert_eq!(
            out.get_pixel(256, 10).0[3],
            0,
            "letterboxed with transparency"
        );
        let text = dir.join("notes.png");
        std::fs::write(&text, "not an image").unwrap();
        assert!(import_custom(&text, &dir.join("x.png")).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
