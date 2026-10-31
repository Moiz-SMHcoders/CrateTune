//! Editable theme files: `<config>/themes/<id>.toml`, listed next to the
//! built-in presets (Appearance → Customize theme), applied like a preset and
//! applied again whenever the file is saved (`theme_library.rs` watches the
//! folder). A file names a base theme and the Customize-theme options, and may
//! set any of gpui-component's colour tokens directly (`[colors]`,
//! [`crate::theme::TOKENS`]); everything it leaves out is derived as usual.
//!
//! ```toml
//! name = "Nord"
//! base = "dark"            # dark | black | dynamic
//! accent = "#88C0D0"
//! corners = "round"
//!
//! [colors]
//! background = "#2E3440"
//! foreground = "#ECEFF4"
//! ```
//!
//! A file that doesn't parse (TOML syntax, an unknown key, a value of the wrong
//! kind, an unknown colour name, an unreadable colour) is listed with the reason
//! and ignored; one that parses but leaves text hard to read gets a warning
//! (foreground < 4.5:1 or muted text < 3:1 on the background). The commented
//! example and four starter themes are written once, when the folder is first
//! created (`themes/*.toml` in this crate); deleted ones stay deleted.
//!
//! Pure functions (parsing, validation, export) are tested here; the folder
//! watching and the UI are in `theme_library.rs` and `theme_ui.rs`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::file::Settings;
use super::theme_prefs::{
    Contrast, Corners, CustomTheme, Darkness, Density, DynamicStyle, color_hex, parse_color,
};
use crate::theme::{Look, ThemeChoice};

/// The folder in the config directory.
pub const DIR: &str = "themes";
const EXTENSION: &str = "toml";
/// Files larger than this aren't read (a theme is a few KiB).
const MAX_BYTES: u64 = 256 * 1024;
/// Text on the background must reach these ratios (WCAG) or the file is flagged.
pub const MIN_TEXT: f32 = 4.5;
pub const MIN_MUTED: f32 = 3.0;

/// The commented example and the starter themes, written on first use.
pub const SEED: &[(&str, &str)] = &[
    ("example.toml", include_str!("../../themes/example.toml")),
    ("nord.toml", include_str!("../../themes/nord.toml")),
    (
        "catppuccin-mocha.toml",
        include_str!("../../themes/catppuccin-mocha.toml"),
    ),
    (
        "gruvbox-dark.toml",
        include_str!("../../themes/gruvbox-dark.toml"),
    ),
    ("dracula.toml", include_str!("../../themes/dracula.toml")),
];

/// The file as written (every key optional, nothing else allowed).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    name: Option<String>,
    base: Option<String>,
    accent: Option<String>,
    surface_tint: Option<f64>,
    darkness: Option<String>,
    contrast: Option<String>,
    corners: Option<String>,
    density: Option<String>,
    font: Option<String>,
    dynamic_style: Option<String>,
    #[serde(default)]
    colors: BTreeMap<String, String>,
}

/// Why a file was ignored.
#[derive(Debug, Clone, PartialEq)]
pub enum Problem {
    /// Couldn't be read (permissions, not UTF-8, too large).
    Read(String),
    /// TOML syntax, an unknown key or a value of the wrong kind (the parser's message).
    Syntax { line: usize, message: String },
    /// `key = "value"` where value isn't one of `expected`.
    Choice {
        line: usize,
        key: String,
        value: String,
        expected: String,
    },
    /// A number outside its range.
    Range { line: usize, key: String },
    /// `[colors]` names a token that doesn't exist.
    UnknownColor { line: usize, key: String },
    /// A colour that isn't `#rgb`, `#rrggbb` or `#rrggbbaa`.
    BadColor {
        line: usize,
        key: String,
        value: String,
    },
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Problem::Read(e) => t!("themes.error.read", error = e),
            Problem::Syntax { line, message } => {
                t!("themes.error.syntax", line = line, message = message)
            }
            Problem::Choice {
                line,
                key,
                value,
                expected,
            } => t!(
                "themes.error.choice",
                line = line,
                key = key,
                value = value,
                expected = expected
            ),
            Problem::Range { line, key } => t!("themes.error.range", line = line, key = key),
            Problem::UnknownColor { line, key } => {
                t!("themes.error.unknown_color", line = line, key = key)
            }
            Problem::BadColor { line, key, value } => {
                t!(
                    "themes.error.bad_color",
                    line = line,
                    key = key,
                    value = value
                )
            }
        };
        f.write_str(&text)
    }
}

/// A file that was ignored, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Invalid {
    /// The file name (`nord.toml`).
    pub file: String,
    pub problem: Problem,
}

/// Text that is hard to read with this theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LowContrast {
    /// `foreground` or `muted_foreground`.
    pub token: &'static str,
    pub ratio: f32,
    pub min: f32,
}

/// A theme file that parsed.
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeFile {
    /// The file name without `.toml` (stored in `custom_theme.file`).
    pub id: String,
    pub name: String,
    pub choice: ThemeChoice,
    accent: String,
    tint: f32,
    darkness: Darkness,
    contrast: Contrast,
    corners: Corners,
    /// Kept as the user has them when the file doesn't say.
    density: Option<Density>,
    font: Option<String>,
    style: DynamicStyle,
    pub colors: BTreeMap<String, String>,
    pub warnings: Vec<LowContrast>,
}

/// The first line (1-based) that sets `key`, after `[section]` if given (for
/// messages; 0 when not found).
fn line_of(text: &str, section: Option<&str>, key: &str) -> usize {
    let mut in_section = section.is_none();
    for (i, line) in text.lines().enumerate() {
        let l = line.trim();
        if l.starts_with('[') {
            in_section = section.is_some_and(|s| l.trim_start_matches('[').starts_with(s));
            continue;
        }
        if in_section
            && let Some(rest) = l.strip_prefix(key)
            && rest.trim_start().starts_with('=')
        {
            return i + 1;
        }
    }
    0
}

fn choice_of<T: Copy>(
    text: &str,
    key: &str,
    value: Option<&String>,
    all: &[T],
    key_of: fn(T) -> &'static str,
    default: T,
) -> Result<T, Problem> {
    let Some(value) = value else {
        return Ok(default);
    };
    let v = value.trim().to_lowercase();
    all.iter()
        .copied()
        .find(|c| key_of(*c) == v)
        .ok_or_else(|| Problem::Choice {
            line: line_of(text, None, key),
            key: key.to_owned(),
            value: value.clone(),
            expected: all
                .iter()
                .map(|c| key_of(*c))
                .collect::<Vec<_>>()
                .join(", "),
        })
}

fn base_key(c: ThemeChoice) -> &'static str {
    match c {
        ThemeChoice::Dark => "dark",
        ThemeChoice::Black => "black",
        ThemeChoice::Dynamic => "dynamic",
    }
}

/// Parses one file's text (`id` = its name without `.toml`).
pub fn parse(id: &str, text: &str) -> Result<ThemeFile, Problem> {
    let raw: Raw = toml::from_str(text).map_err(|e| Problem::Syntax {
        line: e.span().map_or(0, |s| {
            text[..s.start.min(text.len())].lines().count().max(1)
        }),
        message: e.message().trim().to_owned(),
    })?;
    let choice = choice_of(
        text,
        "base",
        raw.base.as_ref(),
        &ThemeChoice::ALL,
        base_key,
        ThemeChoice::Dark,
    )?;
    let darkness = choice_of(
        text,
        "darkness",
        raw.darkness.as_ref(),
        Darkness::ALL,
        Darkness::key,
        Darkness::Standard,
    )?;
    let contrast = choice_of(
        text,
        "contrast",
        raw.contrast.as_ref(),
        Contrast::ALL,
        Contrast::key,
        Contrast::Standard,
    )?;
    let corners = choice_of(
        text,
        "corners",
        raw.corners.as_ref(),
        Corners::ALL,
        Corners::key,
        Corners::Regular,
    )?;
    let style = choice_of(
        text,
        "dynamic_style",
        raw.dynamic_style.as_ref(),
        &DynamicStyle::ALL,
        DynamicStyle::key,
        DynamicStyle::Vibrant,
    )?;
    let density = match &raw.density {
        Some(_) => Some(choice_of(
            text,
            "density",
            raw.density.as_ref(),
            Density::ALL,
            Density::key,
            Density::Regular,
        )?),
        None => None,
    };
    let accent = match raw.accent.as_deref().map(str::trim) {
        None | Some("") => String::new(),
        Some(v) => match super::theme_prefs::parse_hex(v) {
            Some(rgb) => super::theme_prefs::to_hex(rgb),
            None => {
                return Err(Problem::BadColor {
                    line: line_of(text, None, "accent"),
                    key: "accent".into(),
                    value: v.to_owned(),
                });
            }
        },
    };
    let tint = match raw.surface_tint {
        None => 0.,
        Some(v) if v.is_finite() && (0. ..=1.).contains(&v) => v as f32,
        Some(_) => {
            return Err(Problem::Range {
                line: line_of(text, None, "surface_tint"),
                key: "surface_tint".into(),
            });
        }
    };
    let mut colors = BTreeMap::new();
    for (key, value) in &raw.colors {
        if !crate::theme::TOKENS.contains(&key.as_str()) {
            return Err(Problem::UnknownColor {
                line: line_of(text, Some("colors"), key),
                key: key.clone(),
            });
        }
        let Some(c) = parse_color(value) else {
            return Err(Problem::BadColor {
                line: line_of(text, Some("colors"), key),
                key: format!("colors.{key}"),
                value: value.clone(),
            });
        };
        colors.insert(key.clone(), color_hex(c));
    }
    let name = raw
        .name
        .map(|n| n.trim().chars().take(60).collect::<String>())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| id.to_owned());
    let mut file = ThemeFile {
        id: id.to_owned(),
        name,
        choice,
        accent,
        tint,
        darkness,
        contrast,
        corners,
        density,
        font: raw.font.map(|f| f.trim().to_owned()),
        style,
        colors,
        warnings: Vec::new(),
    };
    file.warnings = readability(&file.custom(&CustomTheme::default()), choice);
    Ok(file)
}

/// The text tokens that miss their contrast level on the background.
pub fn readability(custom: &CustomTheme, choice: ThemeChoice) -> Vec<LowContrast> {
    let look = Look::plain(choice, custom.clone());
    let bg = crate::theme::background_of(&look);
    // Translucent text: what it looks like over the background.
    let solid = |c: gpui::Hsla| crate::components::app_backdrop::over(bg, c.opacity(1.), c.a);
    let (fg, muted) = crate::theme::text_of(&look);
    let (fg, muted) = (solid(fg), solid(muted));
    let mut out: Vec<LowContrast> = [
        ("foreground", fg, MIN_TEXT),
        ("muted_foreground", muted, MIN_MUTED),
    ]
    .into_iter()
    .filter_map(|(token, c, min)| {
        let ratio = crate::theme::contrast(c, bg);
        (ratio < min - 0.005).then_some(LowContrast { token, ratio, min })
    })
    .collect();
    // Text on a filled colour, where the file sets both (badges, buttons, menus).
    let color = |name: &str| {
        custom
            .colors
            .get(name)
            .and_then(|v| crate::theme::token_color(v))
    };
    for (fill, text) in PAIRS {
        if let (Some(f), Some(t)) = (color(fill), color(text)) {
            let ratio = crate::theme::contrast(solid(t), solid(f));
            if ratio < MIN_MUTED - 0.005 {
                out.push(LowContrast {
                    token: text,
                    ratio,
                    min: MIN_MUTED,
                });
            }
        }
    }
    out
}

/// Filled tokens and the text drawn on them.
const PAIRS: [(&str, &str); 8] = [
    ("primary", "primary_foreground"),
    ("secondary", "secondary_foreground"),
    ("popover", "popover_foreground"),
    ("sidebar", "sidebar_foreground"),
    ("danger", "danger_foreground"),
    ("success", "success_foreground"),
    ("warning", "warning_foreground"),
    ("info", "info_foreground"),
];

impl ThemeFile {
    /// `base` with this file's look (density and font stay the user's unless
    /// the file sets them; the dynamic strength / transitions too).
    pub fn custom(&self, base: &CustomTheme) -> CustomTheme {
        let mut c = base.clone();
        c.accent = self.accent.clone();
        c.surface_tint = self.tint;
        c.darkness = self.darkness;
        c.contrast = self.contrast;
        c.corners = self.corners;
        c.dynamic.style = self.style;
        if let Some(d) = self.density {
            c.density = d;
        }
        if let Some(f) = &self.font {
            c.font_family = f.clone();
        }
        c.colors = self.colors.clone();
        c.file = self.id.clone();
        c.normalize();
        c
    }

    /// Whether `settings` currently show this file's look.
    pub fn matches(&self, settings: &Settings) -> bool {
        settings.custom_theme.file == self.id
            && settings.appearance.theme == self.choice
            && self.custom(&settings.custom_theme) == settings.custom_theme
    }
}

/// `<config>/themes`.
pub fn dir() -> Option<PathBuf> {
    super::file::config_dir().map(|d| d.join(DIR))
}

/// Writes the example and starter themes when the folder doesn't exist yet
/// (so files the user deleted stay deleted). Returns whether it did.
pub fn seed(dir: &Path) -> std::io::Result<bool> {
    if dir.exists() {
        return Ok(false);
    }
    std::fs::create_dir_all(dir)?;
    for (name, text) in SEED {
        std::fs::write(dir.join(name), text)?;
    }
    Ok(true)
}

/// Every `*.toml` in `dir`, by file name: the ones that parse and the ones
/// that don't (with the reason). A missing folder is empty.
pub fn scan(dir: &Path) -> (Vec<ThemeFile>, Vec<Invalid>) {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == EXTENSION) && p.is_file())
                .collect()
        })
        .unwrap_or_default();
    paths.sort();
    let (mut ok, mut bad) = (Vec::new(), Vec::new());
    for path in paths {
        let file = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let id = path
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = match std::fs::metadata(&path) {
            Ok(m) if m.len() > MAX_BYTES => Err(t!("themes.error.too_large").to_owned()),
            _ => std::fs::read_to_string(&path).map_err(|e| e.to_string()),
        };
        match text.map_err(Problem::Read).and_then(|t| parse(&id, &t)) {
            Ok(theme) => ok.push(theme),
            Err(problem) => bad.push(Invalid { file, problem }),
        }
    }
    (ok, bad)
}

/// A file name for a new theme called `name` in `dir` (`my-theme.toml`,
/// `my-theme-2.toml`, …).
pub fn new_path(dir: &Path, name: &str) -> PathBuf {
    let base = match super::profiles::slug(name) {
        s if s.is_empty() => "theme".to_owned(), // i18n-ignore
        s => s,
    };
    let mut n = 1;
    loop {
        let stem = if n == 1 {
            base.clone()
        } else {
            format!("{base}-{n}")
        };
        let path = dir.join(format!("{stem}.{EXTENSION}"));
        if !path.exists() {
            return path;
        }
        n += 1;
    }
}

fn quoted(s: &str) -> String {
    let mut out = String::from('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The current look as a theme file: the settings, plus every colour token
/// (`shown`: token → the colour on screen now) — the ones the current theme file
/// sets as values, the rest commented out, ready to be uncommented and edited.
pub fn export(name: &str, settings: &Settings, shown: &[(&str, String)]) -> String {
    use std::fmt::Write as _;
    let c = &settings.custom_theme;
    let mut out = String::new();
    // The file's comments are for people editing it: English, like the example.
    let app = crate::identity::APP_NAME;
    let _ = writeln!(
        out,
        "# {app} theme file, exported from Settings → Appearance."
    );
    let _ = writeln!(
        out,
        "# Edit and save it: the app applies the change at once."
    );
    let _ = writeln!(
        out,
        "# Every key is optional; see example.toml in this folder."
    );
    out.push('\n');
    let _ = writeln!(out, "name = {}", quoted(name));
    let base = quoted(base_key(settings.appearance.theme));
    let _ = writeln!(out, "base = {base}             # dark | black | dynamic"); // i18n-ignore
    let _ = writeln!(out, "accent = {}", quoted(&c.accent));
    let _ = writeln!(out, "surface_tint = {:.2}", c.surface_tint);
    let _ = writeln!(out, "darkness = {}", quoted(c.darkness.key()));
    let _ = writeln!(out, "contrast = {}", quoted(c.contrast.key()));
    let _ = writeln!(out, "corners = {}", quoted(c.corners.key()));
    let _ = writeln!(out, "density = {}", quoted(c.density.key()));
    let _ = writeln!(out, "font = {}", quoted(&c.font_family));
    let _ = writeln!(out, "dynamic_style = {}", quoted(c.dynamic.style.key()));
    out.push_str("\n[colors]\n");
    let _ = writeln!(
        out,
        "# Uncomment a line to set that colour (#RRGGBB, or #RRGGBBAA)."
    );
    let _ = writeln!(
        out,
        "# The values are the ones on screen when this was exported."
    );
    for (token, value) in shown {
        match c.colors.get(*token) {
            Some(set) => {
                let _ = writeln!(out, "{token} = {}", quoted(set));
            }
            None => {
                let _ = writeln!(out, "# {token} = {}", quoted(value));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(text: &str) -> ThemeFile {
        parse("t", text).unwrap_or_else(|e| panic!("{e:?}"))
    }

    #[test]
    fn a_full_file_parses() {
        let f = parse_ok(
            r##"
            name = "Nord"
            base = "black"
            accent = "#88c0d0"
            surface_tint = 0.3
            darkness = "deep"
            contrast = "medium"
            corners = "round"
            density = "compact"
            font = " Inter "
            dynamic_style = "muted"

            [colors]
            background = "#2E3440"
            sidebar = "#2e344080"
            "##,
        );
        assert_eq!(f.name, "Nord");
        assert_eq!(f.choice, ThemeChoice::Black);
        let c = f.custom(&CustomTheme::default());
        assert_eq!(c.accent, "#88C0D0");
        assert_eq!(c.surface_tint, 0.3);
        assert_eq!(c.darkness, Darkness::Deep);
        assert_eq!(c.contrast, Contrast::Medium);
        assert_eq!(c.corners, Corners::Round);
        assert_eq!(c.density, Density::Compact);
        assert_eq!(c.font_family, "Inter");
        assert_eq!(c.dynamic.style, DynamicStyle::Muted);
        assert_eq!(c.colors["background"], "#2E3440");
        assert_eq!(c.colors["sidebar"], "#2E344080");
        assert_eq!(c.file, "t");
    }

    #[test]
    fn an_empty_file_is_the_stock_look_and_keeps_density_and_font() {
        let f = parse_ok("");
        assert_eq!(f.name, "t", "the file name when there's no name");
        let user = CustomTheme {
            density: Density::Comfortable,
            font_family: "Avenir".into(),
            accent: "#FF0000".into(),
            ..CustomTheme::default()
        };
        let c = f.custom(&user);
        assert_eq!(c.density, Density::Comfortable);
        assert_eq!(c.font_family, "Avenir");
        assert_eq!(c.accent, "", "the file's (default) accent");
        assert!(f.warnings.is_empty());
    }

    #[test]
    fn problems_name_the_line_and_the_reason() {
        type Check = fn(&Problem) -> bool;
        let cases: [(&str, Check); 7] = [
            (
                "name = \"x\nbase = 1",
                |p| matches!(p, Problem::Syntax { line, .. } if *line >= 1),
            ),
            (
                "nmae = \"x\"",
                |p| matches!(p, Problem::Syntax { message, .. } if message.contains("nmae")),
            ),
            ("name = \"x\"\ndarkness = \"pitch\"", |p| {
                matches!(p, Problem::Choice { line: 2, key, expected, .. }
                    if key == "darkness" && expected.contains("deep"))
            }),
            ("surface_tint = 3", |p| {
                matches!(p, Problem::Range { line: 1, .. })
            }),
            (
                "accent = \"tomato\"",
                |p| matches!(p, Problem::BadColor { line: 1, value, .. } if value == "tomato"),
            ),
            (
                "[colors]\n\nbackgrond = \"#000\"",
                |p| matches!(p, Problem::UnknownColor { line: 3, key } if key == "backgrond"),
            ),
            (
                "[colors]\nborder = \"#12\"",
                |p| matches!(p, Problem::BadColor { line: 2, key, .. } if key == "colors.border"),
            ),
        ];
        for (text, check) in cases {
            let e = parse("t", text).expect_err(text);
            assert!(check(&e), "{text:?} → {e:?}");
            assert!(!e.to_string().is_empty());
        }
    }

    #[test]
    fn hard_to_read_text_is_flagged() {
        let f = parse_ok(
            "[colors]\nbackground = \"#FFFFFF\"\nforeground = \"#EEEEEE\"\nmuted_foreground = \"#777777\"",
        );
        let tokens: Vec<_> = f.warnings.iter().map(|w| w.token).collect();
        assert_eq!(tokens, ["foreground"], "{:?}", f.warnings);
        let f = parse_ok("[colors]\nwarning = \"#FE8019\"\nwarning_foreground = \"#F08010\"");
        assert_eq!(f.warnings.len(), 1);
        assert_eq!(f.warnings[0].token, "warning_foreground");
        let f = parse_ok("[colors]\nmuted_foreground = \"#333333\"");
        assert_eq!(f.warnings.len(), 1);
        assert_eq!(f.warnings[0].token, "muted_foreground");
        assert!(f.warnings[0].ratio < MIN_MUTED);
    }

    #[test]
    fn bundled_themes_parse_without_warnings() {
        for (name, text) in SEED {
            let id = name.trim_end_matches(".toml");
            let f = parse(id, text).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert!(f.warnings.is_empty(), "{name}: {:?}", f.warnings);
            assert_ne!(f.name, id, "{name} has a name");
        }
    }

    #[test]
    fn matching_and_presets_clear_the_file() {
        let f = parse_ok("accent = \"#5B8CFF\"\n[colors]\nbackground = \"#101820\"");
        let mut s = Settings::default();
        assert!(!f.matches(&s));
        s.custom_theme = f.custom(&s.custom_theme);
        assert!(f.matches(&s));
        // A built-in preset replaces the file's colours.
        let preset = super::super::portable::ThemePreset::find("rose").unwrap();
        s.custom_theme = preset.custom(&s.custom_theme);
        assert!(s.custom_theme.colors.is_empty() && s.custom_theme.file.is_empty());
        assert!(!f.matches(&s));
    }

    #[test]
    fn scan_seed_and_export_round_trip() {
        let dir = std::env::temp_dir().join(format!(
            "cratetune-theme-files-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert!(seed(&dir).unwrap());
        assert!(!seed(&dir).unwrap(), "only once");
        std::fs::write(dir.join("broken.toml"), "base = \"sepia\"").unwrap();
        std::fs::write(dir.join("notes.txt"), "not a theme").unwrap();
        let (ok, bad) = scan(&dir);
        assert_eq!(ok.len(), SEED.len());
        assert_eq!(bad.len(), 1);
        assert_eq!(bad[0].file, "broken.toml");
        // Deleted starters stay deleted.
        std::fs::remove_file(dir.join("nord.toml")).unwrap();
        assert!(!seed(&dir).unwrap());
        assert_eq!(scan(&dir).0.len(), SEED.len() - 1);

        // Export → parse gives the same look.
        let mut s = Settings::default();
        s.custom_theme = ok[0].custom(&s.custom_theme);
        s.custom_theme.colors.clear();
        s.custom_theme
            .colors
            .insert("border".into(), "#334455".into());
        let shown = [
            ("background", "#0A0A0A".to_owned()),
            ("border", "#334455".to_owned()),
        ];
        let text = export("Mine \"quoted\"", &s, &shown);
        assert!(text.contains("# background = \"#0A0A0A\""));
        let path = new_path(&dir, "Mine \"quoted\"");
        assert!(path.ends_with("mine-quoted.toml"));
        std::fs::write(&path, &text).unwrap();
        assert!(new_path(&dir, "Mine \"quoted\"").ends_with("mine-quoted-2.toml"));
        let back = parse("mine-quoted", &text).unwrap();
        assert_eq!(back.name, "Mine \"quoted\"");
        let mut expected = s.custom_theme.clone();
        expected.file = "mine-quoted".into();
        assert_eq!(back.custom(&s.custom_theme), expected);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn settings_drop_unknown_tokens_and_bad_colours() {
        let mut c: CustomTheme = serde_json::from_str(
            r##"{"colors": {"background": "#abc", "glow": "#fff", "border": "red"}, "file": " x "}"##,
        )
        .unwrap();
        c.normalize();
        assert_eq!(c.colors.len(), 1);
        assert_eq!(c.colors["background"], "#AABBCC");
        assert_eq!(c.file, "x");
        // Empty values aren't written to settings.json.
        let json = serde_json::to_string(&CustomTheme::default()).unwrap();
        assert!(!json.contains("colors") && !json.contains("\"file\""));
    }
}
