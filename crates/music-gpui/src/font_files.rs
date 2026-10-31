//! Fonts from files (after Limusic): `.ttf`, `.otf` and `.ttc` files in
//! `<config>/fonts` are registered with GPUI's text system at launch
//! (`TextSystem::add_fonts`), so Customize theme → Font and Heading font can
//! pick them like installed ones. Their family names come from the files'
//! `name` tables (fontdb, already in the graph through resvg).
//!
//! Cost: nothing without files. With them, their bytes stay in memory for
//! the session (as any registered font); at most [`MAX_FILES`] files of up to
//! [`MAX_BYTES`] each are read. A file added later shows after a restart.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use gpui::{App, Global};

/// The folder in the config directory.
pub const DIR: &str = "fonts";
const MAX_FILES: usize = 32;
const MAX_BYTES: u64 = 24 * 1024 * 1024;

/// The families the fonts folder added (sorted, no duplicates).
#[derive(Default)]
pub struct FontFiles {
    pub families: Vec<String>,
}

impl Global for FontFiles {}

/// `<config>/fonts`.
pub fn dir() -> Option<PathBuf> {
    crate::settings::file::config_dir().map(|d| d.join(DIR))
}

/// The font files in `dir` (by name, capped), with their bytes.
fn read(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                        matches!(e.to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc")
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    paths.sort();
    paths.truncate(MAX_FILES);
    paths
        .into_iter()
        .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() <= MAX_BYTES))
        .filter_map(|p| Some((p.clone(), std::fs::read(&p).ok()?)))
        .collect()
}

/// The family names in one font file (empty when it isn't a font).
pub fn families(bytes: &[u8]) -> Vec<String> {
    let mut db = resvg::usvg::fontdb::Database::new();
    db.load_font_data(bytes.to_vec());
    let mut out: Vec<String> = db
        .faces()
        .filter_map(|f| f.families.first().map(|(name, _)| name.clone()))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Registers the fonts folder's files (once, at launch).
pub fn load(cx: &mut App) {
    let Some(dir) = dir() else {
        return;
    };
    let mut names = Vec::new();
    let mut fonts = Vec::new();
    for (path, bytes) in read(&dir) {
        let found = families(&bytes);
        if found.is_empty() {
            eprintln!(
                "music-gpui: fonts: {} is not a font (ignored)",
                path.display()
            );
            continue;
        }
        names.extend(found);
        fonts.push(Cow::Owned(bytes));
    }
    if fonts.is_empty() {
        return;
    }
    let count = fonts.len();
    match cx.text_system().add_fonts(fonts) {
        Ok(()) => {
            names.sort_by_key(|n| n.to_lowercase());
            names.dedup();
            eprintln!(
                "music-gpui: fonts: {count} file(s) from {}: {}",
                dir.display(),
                names.join(", ")
            );
            cx.set_global(FontFiles { families: names });
        }
        Err(e) => eprintln!("music-gpui: fonts: {e}"),
    }
}

/// The families added from files (for the font pickers).
pub fn loaded(cx: &App) -> Vec<String> {
    cx.try_global::<FontFiles>()
        .map(|f| f.families.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_fonts_have_no_families() {
        assert!(families(b"not a font").is_empty());
        assert!(families(&[]).is_empty());
        let dir = std::env::temp_dir().join(format!("cratetune-fonts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        std::fs::write(dir.join("b.TTF"), "x").unwrap();
        let found = read(&dir);
        assert_eq!(found.len(), 1, "by extension, any case");
        assert!(families(&found[0].1).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
