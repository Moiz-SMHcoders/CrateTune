//! `SHA256SUMS.txt`: the checksums a release publishes for its assets.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

/// The asset name the release workflow uploads (`shasum -a 256` / `sha256sum` format).
pub const SUMS_FILE: &str = "SHA256SUMS.txt";

/// Parses checksum lines into asset name → lower-case hex digest.
///
/// Accepts the GNU / `shasum` format (`<hex>  <name>`, `<hex> *<name>` in binary
/// mode) and the BSD tag format (`SHA256 (<name>) = <hex>`). Directory parts of a
/// name are dropped (`dist/x.dmg` → `x.dmg`); blank lines, `#` comments and lines
/// that aren't checksums are skipped. A name listed twice with different digests
/// is dropped, so it can't be verified at all.
pub fn parse_sums(text: &str) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut conflicting = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let entry = if let Some(rest) = line.strip_prefix("SHA256 (") {
            rest.rsplit_once(") = ")
                .map(|(name, hash)| (hash.trim(), name))
        } else {
            line.split_once(char::is_whitespace).map(|(hash, name)| {
                let name = name.trim_start();
                (hash, name.strip_prefix('*').unwrap_or(name))
            })
        };
        let Some((hash, name)) = entry else { continue };
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let name = name.trim().rsplit(['/', '\\']).next().unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let hash = hash.to_ascii_lowercase();
        match out.get(name) {
            Some(existing) if *existing != hash => conflicting.push(name.to_owned()),
            _ => {
                out.insert(name.to_owned(), hash);
            }
        }
    }
    for name in conflicting {
        out.remove(&name);
    }
    out
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-256 of a file, lower-case hex.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "a3f1c0ffee00000000000000000000000000000000000000000000000000beef";
    const B: &str = "0000000000000000000000000000000000000000000000000000000000000001";

    #[test]
    fn parses_shasum_output() {
        let text = format!(
            "{A}  cratetune-0.2.0-macos-arm64.app.zip\n\
             {}  *cratetune-0.2.0-windows-x64-setup.exe\r\n\
             \n# comment\n\
             {B}  dist/cratetune-0.2.0-x86_64.AppImage\n\
             SHA256 (cratetune-0.2.0-linux-x86_64.tar.gz) = {B}\n\
             nonsense line\n\
             abc  short-hash.zip\n",
            A.to_uppercase()
        );
        let sums = parse_sums(&text);
        assert_eq!(sums["cratetune-0.2.0-macos-arm64.app.zip"], A);
        // Upper-case digests and the binary-mode `*` marker.
        assert_eq!(sums["cratetune-0.2.0-windows-x64-setup.exe"], A);
        assert_eq!(sums["cratetune-0.2.0-x86_64.AppImage"], B);
        assert_eq!(sums["cratetune-0.2.0-linux-x86_64.tar.gz"], B);
        assert_eq!(sums.len(), 4);
    }

    #[test]
    fn conflicting_entries_are_dropped() {
        let text = format!("{A}  x.zip\n{B}  x.zip\n{A}  y.zip\n{A}  y.zip\n");
        let sums = parse_sums(&text);
        assert!(!sums.contains_key("x.zip"));
        assert_eq!(sums["y.zip"], A);
    }

    #[test]
    fn hashes_files() {
        let dir = std::env::temp_dir().join(format!("updater-sums-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("abc");
        std::fs::write(&file, b"abc").unwrap();
        assert_eq!(
            sha256_file(&file).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
