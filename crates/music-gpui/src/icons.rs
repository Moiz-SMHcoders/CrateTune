//! Lucide icons beyond gpui-component's 101 defaults, embedded individually so
//! the binary doesn't carry the whole 1,800-icon catalog.
//!
//! Use [`Lucide`] (the full catalog enum) for any icon; names that are neither
//! in gpui-component's default set nor listed in [`ExtraIcons`] render blank,
//! so add new ones to the `icon_assets!` list below.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};
pub use gpui_kit_assets::IconName as Lucide;
use gpui_kit_assets::IconNamed;

gpui_kit_assets::icon_assets!(
    ExtraIcons,
    [
        SkipBack,
        SkipForward,
        Volume1,
        Volume2,
        VolumeX,
        // shell
        House,
        Compass,
        ChartNoAxesColumn,
        Shapes,
        Flame,
        Sparkles,
        Library,
        Clock,
        ChartColumn,
        Download,
        FileMusic,
        // player / queue
        Shuffle,
        Repeat,
        Repeat1,
        ListMusic,
        PictureInPicture2,
        ListPlus,
        ListStart,
        ListEnd,
        ListX,
        GripVertical,
        Trash,
        AudioLines,
        Music,
        Disc3,
        MicVocal,
        UsersRound,
        Radio,
        Pin,
        PinOff,
        X,
        // Now Playing: sleep timer, lyrics reload, layout menu, speed
        Moon,
        RotateCcw,
        LayoutTemplate,
        Gauge,
        // settings
        SlidersHorizontal,
        ScrollText,
        Languages,
        Wifi,
        Shield,
        Type,
        // browse & search
        CloudOff,
        ArrowUpLeft,
    ]
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
            AppIcon::Previous => Lucide::SkipBack,
            AppIcon::Next => Lucide::SkipForward,
            AppIcon::Volume => Lucide::Volume2,
            AppIcon::VolumeLow => Lucide::Volume1,
            AppIcon::Muted => Lucide::VolumeX,
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
