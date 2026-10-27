//! Lucide icons beyond gpui-component's 101 defaults (skip, volume), embedded
//! individually so the binary doesn't carry the whole 1,800-icon catalog.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};
use gpui_kit_assets::{IconName, IconNamed};

gpui_kit_assets::icon_assets!(
    ExtraIcons,
    [SkipBack, SkipForward, Volume1, Volume2, VolumeX]
);

#[derive(Clone, Copy)]
pub enum AppIcon {
    Previous,
    Next,
    Volume,
    VolumeLow,
    Muted,
}

impl IconNamed for AppIcon {
    fn path(self) -> SharedString {
        match self {
            AppIcon::Previous => IconName::SkipBack,
            AppIcon::Next => IconName::SkipForward,
            AppIcon::Volume => IconName::Volume2,
            AppIcon::VolumeLow => IconName::Volume1,
            AppIcon::Muted => IconName::VolumeX,
        }
        .path()
    }
}

/// gpui-component's default icons plus [`ExtraIcons`].
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        match ExtraIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui_kit_assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        let mut paths = gpui_kit_assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}
