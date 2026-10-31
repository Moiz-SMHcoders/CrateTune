//! The settings search index: every setting the sections render, as
//! translation keys (the same `k!` keys the rows render with, so titles and
//! descriptions never drift from the page and are searched in the UI
//! language), plus English keywords and synonyms that are always searched.
//!
//! Each entry's `anchor` is the id its row registers (`row_at`, `group_at`,
//! `field_at`, `slider_row_at` or `.anchor(…)`, see `reveal.rs`); picking a
//! result opens `section` and scrolls to that row. Ids are dotted paths: when
//! a row isn't on screen (it only shows with some option on), the reveal
//! falls back to the nearest registered parent (`appearance.bar.gradient_direction`
//! → `appearance.bar`).
//!
//! Besides [`ENTRIES`], [`all`] adds one entry per section (unless an entry
//! already carries the section's title) and one per keyboard shortcut (from
//! `crate::shortcuts::ACTIONS`, anchored by the action id in
//! `shortcuts_ui.rs`).
//!
//! The tests check that every key exists in every locale and that every anchor
//! is registered by exactly one row in `src/settings/*.rs`.

use std::sync::OnceLock;

use super::{SECTIONS, SectionId};
use crate::i18n::Key;

/// What a result is called.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Label {
    Key(Key),
    /// A product name that isn't translated (Last.fm, SponsorBlock).
    Name(&'static str),
}

impl Label {
    pub fn text(self) -> &'static str {
        match self {
            Label::Key(key) => crate::i18n::text(key),
            Label::Name(name) => name,
        }
    }

    pub fn english(self) -> &'static str {
        match self {
            Label::Key(key) => crate::i18n::english(key),
            Label::Name(name) => name,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A row or group: scroll to it and flash it.
    Setting,
    /// A whole section: open it at the top.
    Section,
}

#[derive(Debug)]
pub struct Entry {
    pub anchor: &'static str,
    pub section: SectionId,
    /// The group the row sits in ("Player bar"), for the result's path.
    pub group: Option<Key>,
    pub title: Label,
    pub description: Option<Key>,
    /// English keywords and synonyms (single lowercase words).
    pub keywords: &'static [&'static str],
    pub kind: Kind,
}

macro_rules! key_opt {
    (-) => {
        None
    };
    ($key:literal) => {
        Some(k!($key))
    };
}

/// `entry!(anchor, Section, group key or -, title key, description key or -, keywords…)`
macro_rules! entry {
    ($anchor:literal, $section:ident, $group:tt, $title:literal, $desc:tt $(, $kw:literal)* $(,)?) => {
        Entry {
            anchor: $anchor,
            section: SectionId::$section,
            group: key_opt!($group),
            title: Label::Key(k!($title)),
            description: key_opt!($desc),
            keywords: &[$($kw),*],
            kind: Kind::Setting,
        }
    };
}

/// [`entry!`] with an untranslated name as the title.
macro_rules! named {
    ($anchor:literal, $section:ident, $group:tt, $name:literal, $desc:tt $(, $kw:literal)* $(,)?) => {
        Entry {
            anchor: $anchor,
            section: SectionId::$section,
            group: key_opt!($group),
            title: Label::Name($name),
            description: key_opt!($desc),
            keywords: &[$($kw),*],
            kind: Kind::Setting,
        }
    };
}

/// Shown for an empty query (in this order).
pub const SUGGESTED: &[&str] = &[
    "appearance.theme",
    "playback.quality",
    "eq.enabled",
    "lyrics.style",
    "appearance.bar.progress_style",
    "shortcuts.main",
    "about.updates.check",
    "appearance.profile",
];

/// Every setting row and group, in page order.
pub const ENTRIES: &[Entry] = &[
    // ---- Appearance
    entry!(
        "appearance.profile",
        Appearance,
        -,
        "profiles.group",
        "profiles.note",
        "profiles",
        "switch",
        "preset",
        "look"
    ),
    entry!(
        "appearance.language.interface",
        Appearance,
        "settings.language.group",
        "settings.language.label",
        "settings.language.description",
        "language",
        "locale",
        "translation",
        "ui"
    ),
    entry!(
        "appearance.theme",
        Appearance,
        -,
        "settings.appearance.theme",
        "settings.appearance.theme_note",
        "dark",
        "black",
        "oled",
        "night",
        "light",
        "colour",
        "color",
        "mode",
        "dynamic"
    ),
    entry!(
        "appearance.custom_theme",
        Appearance,
        -,
        "theme.custom.group",
        "theme.custom.intro",
        "custom",
        "preset",
        "colour",
        "color",
        "palette"
    ),
    entry!(
        "appearance.accent",
        Appearance,
        "theme.custom.group",
        "theme.custom.accent",
        "theme.custom.accent_note",
        "colour",
        "color",
        "highlight",
        "hex"
    ),
    entry!(
        "appearance.accent.hue",
        Appearance,
        "theme.custom.accent",
        "theme.custom.hue",
        -,
        "colour",
        "color"
    ),
    entry!(
        "appearance.accent.saturation",
        Appearance,
        "theme.custom.accent",
        "theme.custom.saturation",
        -,
        "colour",
        "color",
        "vivid"
    ),
    entry!(
        "appearance.surfaces.darkness",
        Appearance,
        "theme.custom.surfaces",
        "theme.custom.darkness",
        "theme.custom.darkness_detail",
        "dark",
        "black",
        "background",
        "brightness"
    ),
    entry!(
        "appearance.surfaces.tint",
        Appearance,
        "theme.custom.surfaces",
        "theme.custom.tint",
        "theme.custom.tint_note",
        "colour",
        "color",
        "background"
    ),
    entry!(
        "appearance.surfaces.contrast",
        Appearance,
        "theme.custom.surfaces",
        "theme.custom.contrast",
        "theme.custom.contrast_detail",
        "accessibility",
        "readable",
        "wcag"
    ),
    entry!(
        "appearance.surfaces.corners",
        Appearance,
        "theme.custom.surfaces",
        "theme.custom.corners",
        -,
        "rounded",
        "radius",
        "square"
    ),
    entry!(
        "appearance.surfaces.density",
        Appearance,
        "theme.custom.surfaces",
        "theme.custom.density",
        "theme.custom.density_detail",
        "compact",
        "spacing",
        "padding"
    ),
    entry!(
        "appearance.surfaces.font",
        Appearance,
        "theme.custom.surfaces",
        "theme.custom.font",
        "theme.custom.font_detail",
        "typeface",
        "typography"
    ),
    entry!(
        "appearance.dynamic",
        Appearance,
        -,
        "theme.dynamic.group",
        "theme.dynamic.active_note",
        "artwork",
        "colour",
        "color",
        "accent"
    ),
    entry!(
        "appearance.dynamic.style",
        Appearance,
        "theme.dynamic.group",
        "theme.dynamic.style",
        -,
        "colour",
        "color",
        "vibrant"
    ),
    entry!(
        "appearance.dynamic.strength",
        Appearance,
        "theme.dynamic.group",
        "theme.dynamic.strength",
        -,
        "intensity",
        "colour",
        "color"
    ),
    entry!(
        "appearance.dynamic.transitions",
        Appearance,
        "theme.dynamic.group",
        "theme.dynamic.transitions",
        "theme.dynamic.transitions_detail",
        "fade",
        "animation"
    ),
    entry!(
        "appearance.look.style",
        Appearance,
        "settings.look.group",
        "settings.look.style",
        "settings.look.style_note",
        "artwork",
        "background",
        "gradient",
        "glow",
        "tint",
        "colour",
        "color"
    ),
    entry!(
        "appearance.look.follow_bar",
        Appearance,
        "settings.look.group",
        "settings.look.follow_bar",
        "settings.look.follow_bar_note"
    ),
    entry!(
        "appearance.look.direction",
        Appearance,
        "settings.look.group",
        "settings.look.direction",
        -,
        "gradient"
    ),
    entry!(
        "appearance.look.intensity",
        Appearance,
        "settings.look.group",
        "settings.look.intensity",
        -,
        "strength"
    ),
    entry!(
        "appearance.look.dim",
        Appearance,
        "settings.look.group",
        "settings.look.dim_when_paused",
        -,
        "pause"
    ),
    entry!(
        "appearance.look.tint_surfaces",
        Appearance,
        "settings.look.group",
        "settings.look.tint_surfaces",
        "settings.look.tint_surfaces_note",
        "sidebar",
        "menus"
    ),
    entry!(
        "appearance.look.animate",
        Appearance,
        "settings.look.group",
        "settings.look.animate",
        "settings.look.animate_note",
        "drift",
        "animation"
    ),
    entry!(
        "appearance.motion.animations",
        Appearance,
        "settings.motion.group",
        "settings.motion.animations",
        "settings.motion.animations_note",
        "motion",
        "reduce",
        "reduced",
        "accessibility",
        "animation"
    ),
    entry!(
        "appearance.motion.page_transitions",
        Appearance,
        "settings.motion.group",
        "settings.motion.page_transitions",
        "settings.motion.page_transitions_note",
        "fade",
        "animation"
    ),
    entry!(
        "appearance.motion.card_hover",
        Appearance,
        "settings.motion.group",
        "settings.motion.card_hover",
        "settings.motion.card_hover_note",
        "cards",
        "tiles"
    ),
    entry!(
        "appearance.motion.artwork_headers",
        Appearance,
        "settings.motion.group",
        "settings.motion.artwork_headers",
        "settings.motion.artwork_headers_note",
        "colour",
        "color",
        "header"
    ),
    entry!(
        "appearance.motion.artwork_corners",
        Appearance,
        "settings.motion.group",
        "settings.motion.artwork_corners",
        -,
        "rounded",
        "radius",
        "cover"
    ),
    entry!(
        "appearance.motion.artwork_shadow",
        Appearance,
        "settings.motion.group",
        "settings.motion.artwork_shadow",
        -,
        "cover"
    ),
    entry!(
        "appearance.motion.card_size",
        Appearance,
        "settings.motion.group",
        "settings.motion.card_size",
        -,
        "cards",
        "grid",
        "large",
        "small"
    ),
    #[cfg(target_os = "macos")]
    entry!(
        "appearance.motion.translucent_sidebar",
        Appearance,
        "settings.motion.group",
        "settings.motion.translucent_sidebar",
        "settings.motion.translucent_sidebar_note",
        "blur",
        "vibrancy",
        "transparent"
    ),
    entry!(
        "appearance.text_size",
        Appearance,
        -,
        "settings.appearance.text_size",
        "settings.appearance.text_size_note",
        "font",
        "zoom",
        "scale",
        "bigger",
        "smaller"
    ),
    entry!(
        "appearance.window",
        Appearance,
        -,
        "settings.appearance.window_group",
        -,
        "titlebar",
        "chrome",
        "frame"
    ),
    entry!(
        "appearance.window.system_title_bar",
        Appearance,
        "settings.appearance.window_group",
        "settings.appearance.system_title_bar",
        "settings.appearance.system_title_bar_note",
        "titlebar",
        "title",
        "native",
        "window",
        "controls",
        "header",
        "decorations",
        "frame"
    ),
    entry!(
        "appearance.now_playing",
        Appearance,
        -,
        "common.now_playing",
        "settings.appearance.np_note",
        "layout",
        "fullscreen"
    ),
    entry!(
        "appearance.now_playing.background",
        Appearance,
        "common.now_playing",
        "settings.appearance.background",
        "settings.appearance.np_note",
        "blur",
        "gradient",
        "glow"
    ),
    entry!(
        "appearance.now_playing.fullscreen",
        Appearance,
        "common.now_playing",
        "settings.appearance.np_fullscreen",
        "settings.appearance.np_fullscreen_note",
        "fullscreen",
        "full",
        "screen",
        "maximize",
        "immersive"
    ),
    entry!(
        "appearance.mini_player",
        Appearance,
        -,
        "settings.appearance.mini_player",
        -,
        "miniplayer",
        "floating",
        "window"
    ),
    entry!(
        "appearance.mini_player.background",
        Appearance,
        "settings.appearance.mini_player",
        "settings.appearance.background",
        -,
        "artwork"
    ),
    entry!(
        "appearance.bar.progress_style",
        Appearance,
        "settings.bar.group",
        "settings.bar.progress_style",
        -,
        "player",
        "seekbar",
        "wavy",
        "waveform",
        "equalizer",
        "thick"
    ),
    entry!(
        "appearance.bar.artwork",
        Appearance,
        "settings.bar.group",
        "settings.bar.artwork_style",
        "settings.bar.artwork_style_note",
        "cover",
        "edge"
    ),
    entry!(
        "appearance.bar.peak_caps",
        Appearance,
        "settings.bar.group",
        "settings.bar.peak_caps",
        "settings.bar.peak_caps_note",
        "peaks"
    ),
    entry!(
        "appearance.bar.background_effect",
        Appearance,
        "settings.bar.group",
        "settings.bar.background_effect",
        "settings.bar.background_effect_note",
        "glow",
        "pulse",
        "gradient"
    ),
    entry!(
        "appearance.bar.background_style",
        Appearance,
        "settings.bar.group",
        "settings.bar.background_style",
        -,
        "glow",
        "pulse",
        "gradient"
    ),
    entry!(
        "appearance.bar.dim",
        Appearance,
        "settings.bar.group",
        "settings.bar.dim_when_paused",
        -,
        "pause"
    ),
    entry!(
        "appearance.bar.gradient_direction",
        Appearance,
        "settings.bar.group",
        "settings.bar.gradient_direction",
        -
    ),
    entry!(
        "appearance.bar.gradient_intensity",
        Appearance,
        "settings.bar.group",
        "settings.bar.gradient_intensity",
        -,
        "strength"
    ),
    entry!(
        "appearance.bar.animate_gradient",
        Appearance,
        "settings.bar.group",
        "settings.bar.animate_gradient",
        "settings.bar.animate_gradient_note",
        "animation",
        "drift"
    ),
    entry!(
        "appearance.bar.show_remaining",
        Appearance,
        "settings.bar.group",
        "settings.bar.show_remaining",
        -,
        "time",
        "elapsed",
        "duration",
        "countdown"
    ),
    entry!(
        "appearance.bar.scroll_volume",
        Appearance,
        "settings.bar.group",
        "settings.bar.scroll_volume",
        -,
        "wheel",
        "mouse",
        "loudness"
    ),
    entry!(
        "appearance.bar.mini_follows",
        Appearance,
        "settings.bar.group",
        "settings.bar.mini_follows",
        -,
        "miniplayer"
    ),
    entry!(
        "appearance.bar.np_visualizer",
        Appearance,
        "settings.bar.group",
        "settings.bar.np_visualizer",
        "settings.bar.np_visualizer_note",
        "spectrum",
        "analyzer",
        "visualiser"
    ),
    entry!(
        "appearance.bar.items",
        Appearance,
        "settings.bar.group",
        "bar_items.title",
        "bar_items.note",
        "buttons",
        "hide",
        "controls",
        "customize"
    ),
    // ---- Playback
    entry!(
        "playback.shuffle",
        Playback,
        "settings.playback.queue",
        "settings.playback.shuffle",
        "settings.playback.shuffle_desc",
        "random"
    ),
    entry!(
        "playback.repeat",
        Playback,
        "settings.playback.queue",
        "settings.playback.repeat",
        "settings.playback.repeat_desc",
        "loop"
    ),
    entry!(
        "playback.streams",
        Playback,
        "settings.playback.audio",
        "settings.playback.streams",
        -,
        "source",
        "potoken",
        "client",
        "tv",
        "visionos",
        "route"
    ),
    entry!(
        "playback.stream_status",
        Playback,
        "settings.playback.audio",
        "settings.playback.stream_status",
        -,
        "client",
        "visionos",
        "breaker",
        "failing",
        "status"
    ),
    entry!(
        "playback.player_fixes",
        Playback,
        "settings.playback.audio",
        "settings.playback.player_fixes",
        "settings.playback.player_fixes_desc",
        "fix",
        "update",
        "signature",
        "manifest",
        "cipher",
        "player"
    ),
    entry!(
        "playback.quality",
        Playback,
        "settings.playback.audio",
        "settings.playback.quality",
        "settings.playback.quality_desc",
        "bitrate",
        "kbps",
        "opus",
        "aac",
        "data",
        "sound",
        "audio",
        "hifi"
    ),
    entry!(
        "playback.normalize",
        Playback,
        "settings.playback.audio",
        "settings.playback.normalize",
        "settings.playback.normalize_desc",
        "volume",
        "loudness",
        "replaygain",
        "normalise"
    ),
    entry!(
        "playback.skip_silence",
        Playback,
        "settings.playback.audio",
        "settings.playback.skip_silence",
        "settings.playback.skip_silence_desc",
        "quiet",
        "gap"
    ),
    entry!(
        "playback.auto_skip",
        Playback,
        "settings.playback.audio",
        "settings.playback.auto_skip",
        "settings.playback.auto_skip_desc",
        "error",
        "unavailable"
    ),
    entry!(
        "playback.crossfade",
        Playback,
        "settings.playback.transitions",
        "settings.playback.crossfade",
        "settings.playback.transitions_note",
        "gapless",
        "fade",
        "blend",
        "transition"
    ),
    entry!(
        "playback.speed",
        Playback,
        "settings.playback.speed_pitch",
        "settings.playback.speed",
        "settings.playback.speed_pitch_note",
        "tempo",
        "faster",
        "slower",
        "rate"
    ),
    entry!(
        "playback.pitch",
        Playback,
        "settings.playback.speed_pitch",
        "settings.playback.pitch",
        "settings.playback.speed_pitch_note",
        "key",
        "semitone",
        "transpose"
    ),
    entry!(
        "playback.output",
        Playback,
        -,
        "settings.playback.output",
        "settings.playback.output_note",
        "speaker",
        "headphones",
        "device",
        "sound",
        "audio",
        "bluetooth"
    ),
    entry!(
        "playback.sleep",
        Playback,
        -,
        "settings.playback.sleep",
        "settings.playback.sleep_note",
        "timer",
        "stop",
        "night",
        "bedtime"
    ),
    entry!(
        "playback.sleep.custom",
        Playback,
        "settings.playback.sleep",
        "settings.playback.custom_length",
        -,
        "timer",
        "minutes"
    ),
    // ---- Modules
    entry!(
        "modules.source",
        Modules,
        -,
        "modules.source.group",
        "modules.source.note",
        "provider",
        "plugin",
        "extension",
        "arch"
    ),
    entry!(
        "modules.add",
        Modules,
        -,
        "modules.add.group",
        "modules.add.note",
        "install",
        "plugin",
        "extension",
        "arch"
    ),
    entry!(
        "modules.catalogs",
        Modules,
        -,
        "modules.catalogs",
        -,
        "repository",
        "store"
    ),
    // ---- Equalizer
    entry!(
        "eq.enabled",
        Equalizer,
        -,
        "settings.section.equalizer",
        "settings.eq.note",
        "eq",
        "equaliser",
        "bass",
        "treble",
        "vocal",
        "preset",
        "sound",
        "audio"
    ),
    // ---- Lyrics
    entry!(
        "lyrics.style",
        Lyrics,
        "settings.lyrics.look",
        "settings.lyrics.animation",
        "settings.lyrics.animation_note",
        "style",
        "karaoke",
        "apple",
        "slide",
        "glow",
        "fade"
    ),
    entry!(
        "lyrics.text_size",
        Lyrics,
        "settings.lyrics.look",
        "settings.lyrics.text_size",
        -,
        "font",
        "bigger",
        "smaller"
    ),
    entry!(
        "lyrics.line_spacing",
        Lyrics,
        "settings.lyrics.look",
        "settings.lyrics.line_spacing",
        -,
        "spacing",
        "leading"
    ),
    entry!(
        "lyrics.alignment",
        Lyrics,
        "settings.lyrics.look",
        "settings.lyrics.alignment",
        -,
        "left",
        "center",
        "centre",
        "right"
    ),
    entry!(
        "lyrics.bg_vocals",
        Lyrics,
        "settings.lyrics.look",
        "settings.lyrics.bg_vocals",
        "settings.lyrics.bg_vocals_desc",
        "duet",
        "backing"
    ),
    entry!(
        "lyrics.providers",
        Lyrics,
        -,
        "settings.lyrics.providers",
        "settings.lyrics.providers_note",
        "source",
        "lrclib",
        "musixmatch",
        "order"
    ),
    entry!(
        "lyrics.prefer_word",
        Lyrics,
        "settings.lyrics.providers",
        "settings.lyrics.prefer_word",
        "settings.lyrics.prefer_word_desc",
        "synced",
        "karaoke",
        "word"
    ),
    entry!(
        "lyrics.providers.keys",
        Lyrics,
        "settings.lyrics.providers",
        "settings.lyrics.api_key",
        "settings.lyrics.keys_note",
        "paxsenix",
        "betterlyrics",
        "token"
    ),
    entry!(
        "lyrics.translate",
        Lyrics,
        "settings.lyrics.translation",
        "settings.lyrics.translate",
        "settings.lyrics.translate_desc",
        "translation",
        "language"
    ),
    entry!(
        "lyrics.translator",
        Lyrics,
        "settings.lyrics.translation",
        "settings.lyrics.translator",
        -,
        "google",
        "ai",
        "openai"
    ),
    entry!(
        "lyrics.translate_language",
        Lyrics,
        "settings.lyrics.translation",
        "settings.lyrics.language",
        -,
        "target"
    ),
    entry!(
        "lyrics.translation.endpoint",
        Lyrics,
        "settings.lyrics.translation",
        "settings.lyrics.endpoint",
        -,
        "ai",
        "openai",
        "url"
    ),
    entry!(
        "lyrics.translation.model",
        Lyrics,
        "settings.lyrics.translation",
        "settings.lyrics.model",
        -,
        "ai",
        "openai",
        "llm"
    ),
    entry!(
        "lyrics.translation.api_key",
        Lyrics,
        "settings.lyrics.translation",
        "settings.lyrics.api_key",
        -,
        "ai",
        "openai",
        "token"
    ),
    entry!(
        "lyrics.romanization",
        Lyrics,
        -,
        "settings.lyrics.romanization",
        "settings.lyrics.romanization_note",
        "romaji",
        "romanisation",
        "latin",
        "japanese",
        "korean",
        "chinese",
        "pinyin",
        "kanji"
    ),
    // ---- Content & region
    entry!(
        "content.language",
        Content,
        "settings.content.group_region",
        "settings.content.language",
        "settings.content.language_description",
        "locale"
    ),
    entry!(
        "content.country",
        Content,
        "settings.content.group_region",
        "settings.content.country",
        "settings.content.country_description",
        "region",
        "location"
    ),
    entry!(
        "content.hide_explicit",
        Content,
        "settings.content.group_filters",
        "settings.content.hide_explicit",
        "settings.content.hide_explicit_description",
        "explicit",
        "clean",
        "parental",
        "kids",
        "family"
    ),
    entry!(
        "content.hide_videos",
        Content,
        "settings.content.group_filters",
        "settings.content.hide_videos",
        "settings.content.hide_videos_description",
        "video"
    ),
    entry!(
        "content.autoplay",
        Content,
        "settings.content.group_radio",
        "settings.content.autoplay",
        "settings.content.autoplay_description",
        "radio",
        "continue",
        "endless"
    ),
    // ---- Content & region → Home
    entry!(
        "content.home.quick_picks",
        Content,
        "settings.home.group",
        "settings.home.quick_picks",
        "settings.home.quick_picks_description",
        "home",
        "recommendations",
        "discover",
        "mix",
        "personal"
    ),
    entry!(
        "content.home.display",
        Content,
        "settings.home.group",
        "settings.home.display",
        "settings.home.display_description",
        "home",
        "layout",
        "cards",
        "list",
        "grid"
    ),
    entry!(
        "content.home.chips",
        Content,
        "settings.home.group",
        "settings.home.chips",
        "settings.home.chips_description",
        "home",
        "categories",
        "moods",
        "filters"
    ),
    entry!(
        "content.home.backdrop",
        Content,
        "settings.home.group",
        "settings.home.backdrop",
        "settings.home.backdrop_description",
        "home",
        "tonal",
        "gradient",
        "background",
        "wash"
    ),
    entry!(
        "content.home.sections",
        Content,
        "settings.home.group",
        "settings.home.sections",
        "settings.home.sections_description",
        "home",
        "sections",
        "shelves",
        "hide",
        "speed",
        "dial",
        "forgotten",
        "favorites",
        "favourites",
        "similar",
        "community",
        "feed",
        "playlists"
    ),
    // ---- Network
    entry!(
        "network.proxy",
        Network,
        -,
        "settings.network.proxy",
        "settings.network.proxy_note",
        "socks",
        "http",
        "vpn",
        "firewall"
    ),
    entry!(
        "network.dns",
        Network,
        -,
        "settings.network.dns",
        "settings.network.dns_note",
        "doh",
        "https",
        "cloudflare",
        "resolver"
    ),
    // ---- Privacy
    entry!(
        "privacy.pause_listen",
        Privacy,
        "settings.privacy.listen_history",
        "settings.privacy.pause_listen",
        "settings.privacy.pause_listen_detail",
        "history",
        "incognito",
        "private"
    ),
    entry!(
        "privacy.clear_listen",
        Privacy,
        "settings.privacy.listen_history",
        "settings.privacy.clear_listen",
        "settings.privacy.clear_listen_detail",
        "history",
        "delete",
        "erase"
    ),
    entry!(
        "privacy.pause_search",
        Privacy,
        "settings.privacy.search_history",
        "settings.privacy.pause_search",
        "settings.privacy.pause_search_detail",
        "history",
        "incognito",
        "private"
    ),
    entry!(
        "privacy.clear_search",
        Privacy,
        "settings.privacy.search_history",
        "settings.privacy.clear_search",
        "settings.privacy.clear_search_detail",
        "history",
        "delete",
        "erase",
        "recent"
    ),
    // ---- Storage
    entry!(
        "storage.files",
        Storage,
        -,
        "settings.storage.files",
        -,
        "folder",
        "path",
        "location",
        "config"
    ),
    entry!(
        "storage.cache_limit",
        Storage,
        "settings.storage.play_cache",
        "settings.storage.cache_limit",
        -,
        "disk",
        "space",
        "size"
    ),
    entry!(
        "storage.cache_played",
        Storage,
        "settings.storage.play_cache",
        "settings.storage.cache_played",
        "settings.storage.cache_played_detail",
        "offline"
    ),
    entry!(
        "storage.clear_cache",
        Storage,
        "settings.storage.play_cache",
        "settings.storage.clear_cache_row",
        -,
        "delete",
        "free",
        "space"
    ),
    entry!(
        "storage.artwork_memory",
        Storage,
        "settings.storage.memory",
        "settings.storage.artwork_memory",
        "settings.storage.artwork_memory_detail",
        "ram",
        "images",
        "covers"
    ),
    entry!(
        "storage.download_folder",
        Storage,
        "settings.storage.downloads",
        "settings.storage.download_folder",
        "settings.storage.download_folder_detail",
        "offline",
        "location",
        "path"
    ),
    entry!(
        "storage.auto_download_liked",
        Storage,
        "settings.storage.downloads",
        "downloads.auto_liked",
        "downloads.auto_liked_detail",
        "offline",
        "likes",
        "automatic"
    ),
    entry!(
        "storage.remove_unliked",
        Storage,
        "settings.storage.downloads",
        "downloads.remove_unliked",
        "downloads.remove_unliked_detail",
        "delete",
        "offline"
    ),
    entry!(
        "storage.backup.export",
        Storage,
        "settings.storage.backup",
        "settings.storage.export",
        "settings.storage.export_detail",
        "backup",
        "zip",
        "save"
    ),
    entry!(
        "storage.backup.restore",
        Storage,
        "settings.storage.backup",
        "settings.storage.restore",
        "settings.storage.restore_detail",
        "backup",
        "import",
        "zip"
    ),
    // ---- Account
    entry!(
        "account.youtube",
        Account,
        -,
        "common.youtube_music",
        "settings.account.not_signed_in",
        "login",
        "signin",
        "sign",
        "google",
        "cookie",
        "account"
    ),
    entry!(
        "account.list",
        Account,
        -,
        "settings.account.list_group",
        "settings.account.add_desc",
        "accounts",
        "switch",
        "add",
        "several",
        "multiple",
        "browser",
        "import",
        "chrome",
        "firefox",
        "safari"
    ),
    entry!(
        "account.sync",
        Account,
        "settings.account.sync_group",
        "settings.account.sync_row",
        "settings.account.sync_row_desc",
        "sync",
        "library",
        "likes",
        "playlists"
    ),
    entry!(
        "account.sync.auto",
        Account,
        "settings.account.sync_group",
        "settings.account.auto_sync",
        "settings.account.auto_sync_desc",
        "sync",
        "automatic",
        "background",
        "likes",
        "liked",
        "youtube"
    ),
    entry!(
        "account.sync.report_plays",
        Account,
        "settings.account.sync_group",
        "settings.account.report_plays",
        "settings.account.report_plays_desc",
        "history",
        "plays",
        "devices",
        "phone",
        "scrobble",
        "youtube"
    ),
    entry!(
        "account.sync.skip_disliked",
        Account,
        "settings.account.sync_group",
        "settings.account.skip_disliked",
        "settings.account.skip_disliked_desc",
        "dislike",
        "thumbs",
        "down",
        "skip",
        "radio"
    ),
    // ---- Scrobbling & Discord
    named!(
        "integrations.lastfm",
        Integrations,
        -,
        "Last.fm",
        -,
        "lastfm",
        "scrobble",
        "scrobbling",
        "audioscrobbler"
    ),
    named!(
        "integrations.listenbrainz",
        Integrations,
        -,
        "ListenBrainz",
        -,
        "scrobble",
        "scrobbling",
        "musicbrainz"
    ),
    entry!(
        "integrations.rules",
        Integrations,
        -,
        "scrobble.rules.title",
        "scrobble.rules.note",
        "scrobble",
        "percent",
        "threshold"
    ),
    entry!(
        "integrations.discord",
        Integrations,
        -,
        "settings.integrations.discord_title",
        -,
        "discord",
        "presence",
        "status",
        "rpc"
    ),
    entry!(
        "integrations.discord.activity",
        Integrations,
        "settings.integrations.discord_title",
        "discord.activity.title",
        "discord.activity.custom_note",
        "discord",
        "presence",
        "buttons"
    ),
    entry!(
        "integrations.discord.activity.show_paused",
        Integrations,
        "settings.integrations.discord_title",
        "discord.activity.show_paused",
        "discord.activity.show_paused_desc",
        "discord",
        "pause"
    ),
    // ---- SponsorBlock
    entry!(
        "sponsorblock.enabled",
        SponsorBlock,
        -,
        "settings.sponsorblock.skip_segments",
        "settings.sponsorblock.skip_segments_desc",
        "sponsorblock",
        "sponsor",
        "intro",
        "outro",
        "ads"
    ),
    entry!(
        "sponsorblock.mode",
        SponsorBlock,
        -,
        "settings.sponsorblock.when",
        "settings.sponsorblock.when_desc",
        "sponsorblock",
        "automatic",
        "ask"
    ),
    entry!(
        "sponsorblock.notify",
        SponsorBlock,
        -,
        "settings.sponsorblock.notify",
        -,
        "sponsorblock",
        "notification",
        "toast"
    ),
    entry!(
        "sponsorblock.categories",
        SponsorBlock,
        -,
        "settings.sponsorblock.categories",
        -,
        "sponsorblock",
        "sponsor",
        "intro",
        "outro",
        "filler"
    ),
    entry!(
        "sponsorblock.server",
        SponsorBlock,
        -,
        "settings.sponsorblock.server",
        -,
        "sponsorblock",
        "api",
        "url"
    ),
    // ---- Music Together
    entry!(
        "together.display_name",
        Together,
        "settings.together.you",
        "settings.together.display_name",
        "settings.together.display_name_note",
        "nickname",
        "name",
        "together"
    ),
    entry!(
        "together.hosting",
        Together,
        -,
        "settings.together.hosting",
        "settings.together.hosting_note",
        "together",
        "session",
        "party",
        "host"
    ),
    entry!(
        "together.host_mode",
        Together,
        "settings.together.hosting",
        "settings.together.host_mode",
        "settings.together.host_mode_desc",
        "together",
        "internet",
        "relay",
        "lan",
        "direct",
        "network"
    ),
    entry!(
        "together.allow_add",
        Together,
        "settings.together.hosting",
        "together.room.allow_add",
        "settings.together.allow_add_desc",
        "together",
        "guests",
        "queue"
    ),
    entry!(
        "together.allow_control",
        Together,
        "settings.together.hosting",
        "together.room.allow_control",
        "settings.together.allow_control_desc",
        "together",
        "guests",
        "remote"
    ),
    entry!(
        "together.approval",
        Together,
        "settings.together.hosting",
        "settings.together.approval",
        -,
        "together",
        "guests",
        "approve"
    ),
    entry!(
        "together.relay",
        Together,
        "settings.together.across_networks",
        "settings.together.relay",
        "settings.together.relay_note",
        "together",
        "internet",
        "remote",
        "server",
        "code"
    ),
    entry!(
        "together.relay_privacy",
        Together,
        "settings.together.across_networks",
        "settings.together.relay",
        "settings.together.relay_privacy",
        "together",
        "privacy",
        "relay",
        "titles",
        "names"
    ),
    // ---- Spotify import
    entry!(
        "spotify.account",
        Spotify,
        -,
        "settings.spotify.account",
        "settings.spotify.steps",
        "spotify",
        "import",
        "playlists",
        "connect"
    ),
    entry!(
        "spotify.without_account",
        Spotify,
        -,
        "settings.spotify.without_account",
        -,
        "spotify",
        "exportify",
        "csv",
        "import"
    ),
    // ---- AI content filter
    entry!(
        "ai_filter.enabled",
        AiFilter,
        -,
        "settings.ai_filter.enable",
        "settings.ai_filter.enable_desc",
        "ai",
        "filter",
        "llm",
        "openai",
        "ollama"
    ),
    entry!(
        "ai_filter.model",
        AiFilter,
        -,
        "settings.ai_filter.model",
        "settings.ai_filter.api_note",
        "ai",
        "endpoint",
        "openai",
        "ollama",
        "key"
    ),
    entry!(
        "ai_filter.categories",
        AiFilter,
        -,
        "settings.ai_filter.hide",
        "settings.ai_filter.custom_note",
        "ai",
        "filter",
        "categories"
    ),
    entry!(
        "ai_filter.privacy",
        AiFilter,
        -,
        "settings.ai_filter.privacy",
        "settings.ai_filter.privacy_note",
        "ai"
    ),
    // ---- Chromecast
    entry!(
        "cast.devices",
        Cast,
        -,
        "settings.cast.devices",
        "settings.cast.note",
        "chromecast",
        "cast",
        "tv",
        "speaker",
        "nest"
    ),
    entry!(
        "cast.direct",
        Cast,
        "settings.cast.streaming",
        "settings.cast.direct",
        "settings.cast.direct_desc",
        "chromecast",
        "cast"
    ),
    // ---- Backup
    entry!(
        "backup.export_settings",
        Backup,
        "portable.settings_group",
        "portable.export_settings",
        "portable.export_settings_detail",
        "export",
        "json",
        "transfer",
        "backup"
    ),
    entry!(
        "backup.import_settings",
        Backup,
        "portable.settings_group",
        "portable.import_settings",
        "portable.import_settings_detail",
        "import",
        "json",
        "transfer",
        "restore"
    ),
    entry!(
        "backup.export_theme",
        Backup,
        "portable.theme_group",
        "theme.file.export",
        "portable.export_theme_detail",
        "export",
        "share",
        "theme"
    ),
    entry!(
        "backup.import_theme",
        Backup,
        "portable.theme_group",
        "theme.file.import",
        "portable.import_theme_detail",
        "import",
        "theme"
    ),
    // ---- Keyboard shortcuts (one entry per action is added by `all`)
    entry!(
        "shortcuts.main",
        Shortcuts,
        -,
        "settings.section.shortcuts",
        "shortcuts.intro",
        "shortcut",
        "hotkey",
        "hotkeys",
        "keybinding",
        "keybindings",
        "keyboard",
        "keys",
        "bindings"
    ),
    // ---- About
    entry!(
        "about.updates.check",
        About,
        "update.group",
        "menu.check_updates",
        "update.auto_check_detail",
        "update",
        "upgrade",
        "version",
        "new",
        "release"
    ),
    entry!(
        "about.updates.auto_check",
        About,
        "update.group",
        "update.auto_check",
        "update.auto_check_detail",
        "update",
        "automatic"
    ),
    entry!(
        "about.updates.auto_install",
        About,
        "update.group",
        "update.auto_install",
        "update.auto_install_detail",
        "update",
        "automatic"
    ),
    entry!(
        "about.updates.prereleases",
        About,
        "update.group",
        "update.prereleases",
        "update.prereleases_detail",
        "update",
        "beta",
        "nightly",
        "preview"
    ),
    entry!(
        "about.welcome",
        About,
        -,
        "settings.about.show_welcome",
        -,
        "onboarding",
        "tour",
        "intro",
        "guide"
    ),
    entry!(
        "about.license",
        About,
        -,
        "settings.about.license",
        -,
        "gpl",
        "licence",
        "copyright"
    ),
    entry!(
        "about.credits",
        About,
        -,
        "settings.about.credits",
        -,
        "thanks",
        "libraries",
        "acknowledgements"
    ),
];

/// English synonyms for whole sections.
fn section_keywords(id: SectionId) -> &'static [&'static str] {
    match id {
        SectionId::Appearance => &["look", "theme", "design"],
        SectionId::Playback => &["audio", "sound", "player"],
        SectionId::Modules => &["plugins", "extensions", "providers"],
        SectionId::Equalizer => &["eq", "equaliser"],
        SectionId::Lyrics => &["words", "karaoke"],
        SectionId::Content => &["region", "country", "language", "home"],
        SectionId::Network => &["proxy", "internet", "dns"],
        SectionId::Privacy => &["history", "tracking"],
        SectionId::Storage => &["cache", "disk", "downloads"],
        SectionId::Account => &["login", "google", "youtube"],
        SectionId::Integrations => &["lastfm", "scrobble", "discord"],
        SectionId::SponsorBlock => &["sponsor", "skip"],
        SectionId::Together => &["party", "session", "friends"],
        SectionId::Spotify => &["import", "playlists"],
        SectionId::AiFilter => &["ai", "filter"],
        SectionId::Cast => &["chromecast", "tv"],
        SectionId::Backup => &["export", "import"],
        SectionId::About => &["version", "info"],
        SectionId::Shortcuts => &["keyboard", "hotkeys"],
    }
}

/// [`ENTRIES`], one entry per section without one and one per keyboard
/// shortcut, built once.
pub fn all() -> &'static [Entry] {
    static ALL: OnceLock<Vec<Entry>> = OnceLock::new();
    ALL.get_or_init(|| {
        let mut out: Vec<Entry> = Vec::with_capacity(ENTRIES.len() + 64);
        for section in SECTIONS {
            let title = Label::Key(section.title);
            if ENTRIES
                .iter()
                .any(|e| e.section == section.id && e.title == title)
            {
                continue;
            }
            // Built once: "section.eq" (shortcut ids like "about" are anchors too).
            let anchor: &'static str =
                Box::leak(format!("section.{}", section.id.key()).into_boxed_str());
            out.push(Entry {
                anchor,
                section: section.id,
                group: None,
                title,
                description: None,
                keywords: section_keywords(section.id),
                kind: Kind::Section,
            });
        }
        out.extend(ENTRIES.iter().map(|e| Entry {
            anchor: e.anchor,
            section: e.section,
            group: e.group,
            title: e.title,
            description: e.description,
            keywords: e.keywords,
            kind: e.kind,
        }));
        for action in crate::shortcuts::ACTIONS {
            out.push(Entry {
                anchor: action.id,
                section: SectionId::Shortcuts,
                group: Some(action.group.title_key()),
                title: Label::Key(action.label),
                description: None,
                keywords: &["shortcut", "hotkey", "keys", "keyboard"],
                kind: Kind::Setting,
            });
        }
        out
    })
}

/// The entry with this anchor.
pub fn find(anchor: &str) -> Option<&'static Entry> {
    all().iter().find(|e| e.anchor == anchor)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::i18n::Locale;

    /// Index entries whose title or description the row doesn't render itself:
    /// the button says "Check now", the menu item "Check for Updates…"; the
    /// panel draws the section titles.
    const OWN_WORDS: &[(&str, &str)] = &[
        ("about.updates.check", "menu.check_updates"),
        ("shortcuts.main", "settings.section.shortcuts"),
    ];

    fn keys(entry: &Entry) -> Vec<Key> {
        let mut keys = row_keys(entry);
        keys.extend(entry.group);
        keys
    }

    /// The keys the row itself renders (its group title comes from the caller).
    fn row_keys(entry: &Entry) -> Vec<Key> {
        let mut keys: Vec<Key> = entry.description.into_iter().collect();
        if let Label::Key(key) = entry.title {
            keys.push(key);
        }
        keys
    }

    #[test]
    fn every_key_exists_in_every_locale() {
        let mut missing = Vec::new();
        for entry in all() {
            for key in keys(entry) {
                for locale in Locale::all() {
                    if crate::i18n::own_text(locale, key).is_none_or(str::is_empty) {
                        missing.push(format!(
                            "{}: {} ({})",
                            locale.code(),
                            key.name(),
                            entry.anchor
                        ));
                    }
                }
            }
        }
        assert!(missing.is_empty(), "untranslated:\n{}", missing.join("\n"));
    }

    #[test]
    fn anchors_are_unique_and_keywords_plain() {
        let mut seen = BTreeSet::new();
        for entry in all() {
            assert!(
                seen.insert(entry.anchor),
                "duplicate anchor {}",
                entry.anchor
            );
            for kw in entry.keywords {
                assert!(
                    !kw.is_empty()
                        && kw
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
                    "{}: keyword {kw:?} should be one lowercase word",
                    entry.anchor
                );
            }
        }
        for anchor in SUGGESTED {
            assert!(
                find(anchor).is_some(),
                "suggested {anchor} isn't in the index"
            );
        }
        // Every section can be found, by an entry of its own or its title.
        for section in SECTIONS {
            assert!(
                all()
                    .iter()
                    .any(|e| e.section == section.id && e.title == Label::Key(section.title)),
                "{:?}",
                section.id
            );
        }
        // Every shortcut action has an entry.
        for action in crate::shortcuts::ACTIONS {
            assert!(find(action.id).is_some(), "{}", action.id);
        }
    }

    /// The ids registered in `src/settings/*.rs` (`row_at("id"`, `.anchor("id")`, …)
    /// and the file each one is in.
    fn registered() -> BTreeMap<String, BTreeSet<String>> {
        const CALLS: &[&str] = &[
            "row_at(",
            "group_at(",
            "field_at(",
            "slider_row_at(",
            ".anchor(",
        ];
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/settings");
        let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for file in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = file.path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let text: String = std::fs::read_to_string(&path)
                .unwrap()
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            for call in CALLS {
                for (at, _) in text.match_indices(call) {
                    let rest = text[at + call.len()..].trim_start();
                    let Some(rest) = rest.strip_prefix('"') else {
                        continue;
                    };
                    let id = &rest[..rest.find('"').unwrap()];
                    out.entry(id.to_owned()).or_default().insert(name.clone());
                }
            }
        }
        out
    }

    #[test]
    fn every_anchor_is_registered_by_a_rendered_row() {
        let registered = registered();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/settings");
        let mut problems = Vec::new();
        for entry in ENTRIES {
            let Some(files) = registered.get(entry.anchor) else {
                problems.push(format!("{}: no row registers this anchor", entry.anchor));
                continue;
            };
            // One row (or alternatives of one row in the same file).
            if files.len() != 1 {
                problems.push(format!("{}: registered in {files:?}", entry.anchor));
                continue;
            }
            let file = files.first().unwrap();
            let text = std::fs::read_to_string(dir.join(file)).unwrap();
            // The row renders the keys the index searches.
            for key in row_keys(entry) {
                let name = key.name();
                let exempt = OWN_WORDS.contains(&(entry.anchor, name));
                if !exempt && !text.contains(&format!("\"{name}\"")) {
                    problems.push(format!("{}: {file} doesn't use {name}", entry.anchor));
                }
            }
        }
        // Shortcut entries: each row of the list anchors itself by the action id.
        let shortcuts = std::fs::read_to_string(dir.join("shortcuts_ui.rs")).unwrap();
        assert!(shortcuts.contains(".anchor(action.id)"));
        // Group anchors of the shortcut list.
        for group in crate::shortcuts::Group::ALL {
            assert!(group.anchor().starts_with("shortcuts."));
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    #[test]
    fn hidden_rows_fall_back_to_a_registered_parent() {
        // Rows that only show with some option on: their parent is an anchor too.
        let registered = registered();
        for anchor in [
            "appearance.look.direction",
            "appearance.look.animate",
            "appearance.bar.gradient_direction",
            "appearance.surfaces.font",
            "lyrics.translation.endpoint",
            "about.updates.check",
        ] {
            let (parent, _) = anchor.rsplit_once('.').unwrap();
            assert!(registered.contains_key(parent), "{anchor}: {parent}");
        }
    }
}
