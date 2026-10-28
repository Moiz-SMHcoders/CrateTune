//! The app's display name from `packaging/app.env` (as `MUSIC_APP_NAME`), for the
//! relay's help text, log and health answer.

use std::path::Path;

fn main() {
    let env_file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/app.env");
    println!("cargo:rerun-if-changed={}", env_file.display());
    let text = std::fs::read_to_string(&env_file).unwrap_or_default();
    let name = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| l.strip_prefix("APP_NAME="))
        .map(|n| n.trim().trim_matches('"'))
        .filter(|n| !n.is_empty())
        .unwrap_or("Music");
    println!("cargo:rustc-env=MUSIC_APP_NAME={name}");
}
