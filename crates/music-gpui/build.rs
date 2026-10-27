//! Exposes `packaging/app.env` (the app's name and ids, see `identity.rs`) to the
//! code, and on Windows embeds the icon + version info (`packaging/windows/app.rc.in`).

use std::path::Path;

fn main() {
    let env_file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/app.env");
    println!("cargo:rerun-if-changed={}", env_file.display());
    let text = std::fs::read_to_string(&env_file).expect("packaging/app.env");
    for key in [
        "APP_NAME",
        "APP_ID",
        "BUNDLE_ID",
        "APP_SUMMARY",
        "UPDATE_REPO",
    ] {
        let value = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.starts_with('#'))
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
            .unwrap_or_else(|| panic!("{key} missing in packaging/app.env"))
            .trim()
            .trim_matches('"');
        println!("cargo:rustc-env=MUSIC_{key}={value}");
    }

    #[cfg(windows)]
    windows_resources(&text);
}

/// Icon and version resource for the .exe (native Windows builds; GPUI embeds its own
/// manifest, so the .rc has none). The template's `@KEY@` placeholders are filled in
/// here, so no string macros have to survive rc.exe's command line.
#[cfg(windows)]
fn windows_resources(env_text: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/windows");
    let template = dir.join("app.rc.in");
    let icon = dir.join("app.ico");
    println!("cargo:rerun-if-changed={}", template.display());
    println!("cargo:rerun-if-changed={}", icon.display());
    let get = |key: &str| {
        env_text
            .lines()
            .find_map(|l| l.trim().strip_prefix(key)?.strip_prefix('='))
            .unwrap_or("")
            .trim()
            .trim_matches('"')
            .replace('"', "'")
    };
    let version = env!("CARGO_PKG_VERSION");
    let mut parts = version
        .split(['.', '-'])
        .map(|p| p.parse::<u16>().unwrap_or(0))
        .chain(std::iter::repeat(0));
    let numeric = format!(
        "{},{},{},0",
        parts.next().unwrap(),
        parts.next().unwrap(),
        parts.next().unwrap()
    );
    let icon_path = std::fs::canonicalize(&icon)
        .expect("packaging/windows/app.ico")
        .display()
        .to_string()
        .trim_start_matches(r"\\?\")
        .replace('\\', "\\\\");
    let rc = std::fs::read_to_string(&template)
        .expect("packaging/windows/app.rc.in")
        .replace("@APP_NAME@", &get("APP_NAME"))
        .replace("@APP_ID@", &get("APP_ID"))
        .replace("@APP_SUMMARY@", &get("APP_SUMMARY"))
        .replace("@APP_VERSION@", version)
        .replace("@APP_VERSION_NUM@", &numeric)
        .replace("@ICON_PATH@", &icon_path);
    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("app.rc");
    std::fs::write(&out, rc).expect("writing app.rc");
    embed_resource::compile(&out, embed_resource::NONE)
        .manifest_optional()
        .expect("embedding the Windows resources failed");
}
