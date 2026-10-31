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
        // music videos, theater mode
        Video,
        VideoOff,
        RectangleHorizontal,
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
        // integrations: Chromecast
        Cast,
        Pin,
        PinOff,
        X,
        // Now Playing: sleep timer, lyrics reload, layout menu, speed
        Moon,
        RotateCcw,
        LayoutTemplate,
        Gauge,
        // lyrics cards
        Share2,
        ImageDown,
        // settings
        SunMoon,
        SlidersHorizontal,
        ScrollText,
        Languages,
        Wifi,
        Shield,
        Type,
        Keyboard,
        ArchiveRestore,
        FilePlus,
        ImageUp,
        ZoomIn,
        // library / account
        Pencil,
        LogOut,
        Upload,
        FolderPlus,
        ListChecks,
        Trophy,
        Cookie,
        CalendarDays,
        ArrowUpDown,
        HardDriveDownload,
        Bookmark,
        UserPlus,
        Smartphone,
        // device hand-off
        MonitorSmartphone,
        Laptop,
        // browse & search
        CloudOff,
        ArrowUpLeft,
        // modules
        Puzzle,
        FolderOpen,
        Link,
        LibraryBig,
        Settings2,
        Stethoscope,
        RefreshCw,
        ArrowUp,
        ArrowDown,
        ClipboardCopy,
        // recognition / onboarding
        AudioWaveform,
        CircleStop,
    ]
);

#[derive(Clone, Copy)]
pub enum AppIcon {
    Previous,
    Next,
    Volume,
    VolumeLow,
    Muted,
    /// Filled heart (liked); the outline is `Lucide::Heart`.
    HeartFilled,
    /// Filled thumbs down (disliked); the outline is `Lucide::ThumbsDown`.
    ThumbsDownFilled,
}

/// Lucide's thumbs down, filled.
const THUMBS_DOWN_FILLED_PATH: &str = "icons/app-thumbs-down-filled.svg";
const THUMBS_DOWN_FILLED_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="currentColor" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 18.12 10 14H4.17a2 2 0 0 1-1.92-2.56l2.33-8A2 2 0 0 1 6.5 2H20a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2h-2.76a2 2 0 0 0-1.79 1.11L12 22a3.13 3.13 0 0 1-3-3.88Z"/></svg>"#;

/// Lucide's heart, filled.
const HEART_FILLED_PATH: &str = "icons/app-heart-filled.svg";
const HEART_FILLED_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="currentColor" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M2 9.5a5.5 5.5 0 0 1 9.591-3.676.56.56 0 0 0 .818 0A5.49 5.49 0 0 1 22 9.5c0 2.29-1.5 4-3 5.5l-5.492 5.313a2 2 0 0 1-3 .019L5 15c-1.5-1.5-3-3.2-3-5.5"/></svg>"#;

impl IconNamed for AppIcon {
    fn path(self) -> SharedString {
        match self {
            AppIcon::Previous => Lucide::SkipBack,
            AppIcon::Next => Lucide::SkipForward,
            AppIcon::Volume => Lucide::Volume2,
            AppIcon::VolumeLow => Lucide::Volume1,
            AppIcon::Muted => Lucide::VolumeX,
            AppIcon::HeartFilled => return HEART_FILLED_PATH.into(),
            AppIcon::ThumbsDownFilled => return THUMBS_DOWN_FILLED_PATH.into(),
        }
        .path()
    }
}

/// gpui-component's default icons plus [`ExtraIcons`].
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        if path == HEART_FILLED_PATH {
            return Ok(Some(Cow::Borrowed(HEART_FILLED_SVG)));
        }
        if path == crate::turntable::TONEARM_PATH {
            return Ok(Some(Cow::Borrowed(crate::turntable::TONEARM_SVG)));
        }
        if path == THUMBS_DOWN_FILLED_PATH {
            return Ok(Some(Cow::Borrowed(THUMBS_DOWN_FILLED_SVG)));
        }
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
