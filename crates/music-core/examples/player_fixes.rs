//! Keys and signatures for `packaging/player-fixes.json` (see `docs/RELEASING.md`;
//! `scripts/sign-player-fixes.sh` wraps this):
//!
//! ```sh
//! cargo run -p music-core --example player_fixes -- keygen ~/.config/cratetune/player-fixes.key
//! cargo run -p music-core --example player_fixes -- sign   ~/.config/cratetune/player-fixes.key packaging/player-fixes.json
//! cargo run -p music-core --example player_fixes -- verify packaging/player-fixes.json
//! ```
//!
//! Keys and signatures are minisign's format: `minisign -Vm packaging/player-fixes.json
//! -P <public key>` verifies too. `keygen` never overwrites a key.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use music_core::youtube::fixes::{self, minisign};

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["keygen", key] => {
            let path = Path::new(key);
            if path.exists() {
                return Err(format!("{key} exists; not overwriting it").into());
            }
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let sk = minisign::SecretKey::generate()?;
            write_private(path, &sk.to_file())?;
            let pk = sk.public();
            std::fs::write(path.with_extension("pub"), pk.to_file())?;
            println!("secret key: {key} (mode 0600; keep it out of the repository)");
            println!("public key ({}): {}", pk.id_hex(), pk.encode());
            println!("put the public key into crates/music-core/src/youtube/fixes.rs (PUBLIC_KEY)");
        }
        ["sign", key, manifest] => {
            let sk = minisign::SecretKey::from_file(&std::fs::read_to_string(key)?)?;
            let bytes = std::fs::read(manifest)?;
            // Refuse to sign what the app would refuse to use.
            let parsed = fixes::parse(&bytes)?;
            let name = Path::new(manifest)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("player-fixes.json");
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
            let comment = format!(
                "timestamp:{now}\tfile:{name}\tserial:{}\thashed",
                parsed.serial
            );
            let sig = sk.sign(&bytes, &comment)?;
            let out = format!("{manifest}.minisig");
            std::fs::write(&out, &sig)?;
            if sk.public().encode() != fixes::PUBLIC_KEY {
                eprintln!(
                    "warning: this key ({}) is not the one built into the app",
                    sk.public().encode()
                );
            }
            println!("signed serial {} → {out}", parsed.serial);
        }
        ["verify", manifest] | ["verify", manifest, _] => {
            let key = args.get(2).map_or(fixes::PUBLIC_KEY, String::as_str);
            let bytes = std::fs::read(manifest)?;
            let sig = std::fs::read_to_string(format!("{manifest}.minisig"))?;
            let parsed = fixes::verify_and_parse(&bytes, &sig, key)?;
            println!(
                "ok: serial {} issued {} (signature verified)",
                parsed.serial, parsed.issued
            );
        }
        _ => {
            return Err(
                "usage: player_fixes keygen <key> | sign <key> <manifest> | verify <manifest> [public key]"
                    .into(),
            );
        }
    }
    Ok(())
}

#[cfg(unix)]
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(text.as_bytes())
}

#[cfg(not(unix))]
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(text.as_bytes())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
