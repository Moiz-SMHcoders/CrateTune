//! Packs a provider module directory into an `.arch` file.
//!
//! ```sh
//! cargo run -p music-core --example pack_module -- path/to/provider-dir out.arch
//! ```
//!
//! The directory holds `manifest.json`, `provider.js` and (when the manifest
//! declares `module.icon`) `icon.png`. `sha256` / `iconSha256` are added to the
//! packaged manifest; the result is validated like an import would.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [dir, out] = args.as_slice() else {
        eprintln!("usage: pack_module <dir> <out.arch>");
        std::process::exit(2);
    };
    let bytes = match music_core::modules::package::pack_dir(std::path::Path::new(dir)) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("pack failed: {e}");
            std::process::exit(1);
        }
    };
    match music_core::modules::package::read(&bytes, env!("CARGO_PKG_VERSION")) {
        Ok(pkg) => println!(
            "{} {} ({}): {} bytes, sha256 {}",
            pkg.manifest.id,
            pkg.manifest.version,
            pkg.manifest.capabilities.join(", "),
            bytes.len(),
            pkg.sha256
        ),
        Err(e) => {
            eprintln!("invalid package: {e}");
            std::process::exit(1);
        }
    }
    if let Err(e) = std::fs::write(out, bytes) {
        eprintln!("write failed: {e}");
        std::process::exit(1);
    }
}
