//! Kanji readings for Japanese romanization from a dictionary downloaded on demand.
//!
//! Without the `japanese-dict` feature (which embeds IPADIC, ~48 MB), the app can
//! fetch lindera's prebuilt IPADIC once ([`DictionarySource::ipadic`]: a 10.5 MB zip,
//! 45 MB unpacked), verify its SHA-256, unpack it into a cache directory and
//! [`load`] it from there (memory-mapped). Until then, and if anything fails,
//! romanization stays kana-only.
//!
//! ```no_run
//! use std::sync::atomic::AtomicBool;
//! use music_core::lyrics::romanize::dictionary::{self, DictionarySource};
//! let source = DictionarySource::ipadic();
//! let dir = std::path::Path::new("/tmp/music-cache/ja-dict");
//! let path = match dictionary::installed(&source, dir) {
//!     Some(p) => p,
//!     None => dictionary::install(&source, dir, &AtomicBool::new(false), |done, total| {
//!         eprintln!("{done} / {total}");
//!     })?,
//! };
//! dictionary::load(&path)?;
//! # Ok::<(), music_core::Error>(())
//! ```

use std::borrow::Cow;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use lindera::dictionary::load_fs_dictionary;
use lindera::mode::Mode;
use lindera::segmenter::Segmenter;
use sha2::{Digest, Sha256};

use super::japanese::kana_to_romaji;
use crate::{Error, Result};

/// A downloadable dictionary (a zip with one top-level directory).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictionarySource {
    /// Shown in the UI.
    pub name: String,
    pub url: String,
    /// Lowercase hex SHA-256 of the zip.
    pub sha256: String,
    /// Size of the zip in bytes.
    pub size: u64,
    /// Unpacked size in bytes (an upper bound is enforced while unpacking).
    pub unpacked_size: u64,
    /// The zip's top-level directory, also the installed directory's name.
    pub dir_name: String,
}

impl DictionarySource {
    /// lindera's prebuilt IPADIC for the lindera version this crate uses (6.2): the
    /// smallest of its Japanese dictionaries (UniDic is 46 MB zipped) and the one
    /// `japanese-dict` embeds. Readings come from mecab-ipadic (see its NOTICE.txt).
    pub fn ipadic() -> Self {
        Self {
            name: "IPADIC (lindera 6.2)".into(),
            url: "https://github.com/lindera/lindera/releases/download/v6.2.0/lindera-ipadic-6.2.0.zip"
                .into(),
            sha256: "5ed4bba6b429030b0387df67d40d5751d35dedef0306f4bac6d957cad5f04b72".into(),
            size: 10_519_550,
            unpacked_size: 47_528_940,
            dir_name: "lindera-ipadic".into(),
        }
    }

    /// Installed directory under `dir`, versioned by the checksum.
    fn install_dir(&self, dir: &Path) -> PathBuf {
        dir.join(format!(
            "{}-{}",
            self.dir_name,
            &self.sha256[..self.sha256.len().min(12)]
        ))
    }
}

const COMPLETE_MARKER: &str = ".complete";

/// The installed dictionary under `dir`, if a complete one is there.
pub fn installed(source: &DictionarySource, dir: &Path) -> Option<PathBuf> {
    let path = source.install_dir(dir);
    let marker = fs::read_to_string(path.join(COMPLETE_MARKER)).ok()?;
    (marker.trim() == source.sha256).then_some(path)
}

/// Bytes used by the installed dictionary (0 when absent).
pub fn installed_size(source: &DictionarySource, dir: &Path) -> u64 {
    let Some(path) = installed(source, dir) else {
        return 0;
    };
    fs::read_dir(path)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

/// Downloads, verifies and unpacks `source` into `dir`. Blocking; `progress(done,
/// total)` is called as bytes arrive; setting `cancel` stops it (`Err`). Returns the
/// installed directory, ready for [`load`]. Partial files are removed on failure.
pub fn install(
    source: &DictionarySource,
    dir: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64),
) -> Result<PathBuf> {
    if let Some(path) = installed(source, dir) {
        return Ok(path);
    }
    fs::create_dir_all(dir)?;
    let zip_path = dir.join(format!("{}.zip.part", source.dir_name));
    let result = download(source, &zip_path, cancel, &mut progress)
        .and_then(|()| unpack(source, &zip_path, dir));
    let _ = fs::remove_file(&zip_path);
    result
}

fn download(
    source: &DictionarySource,
    to: &Path,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(u64, u64),
) -> Result<()> {
    let resp = crate::http::agent()
        .get(&source.url)
        .config()
        .http_status_as_error(false)
        .timeout_global(None)
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(Duration::from_secs(30 * 60)))
        .build()
        .call()?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(Error::Parse(format!(
            "dictionary download failed: HTTP {status}"
        )));
    }
    let total = source.size;
    let mut reader = resp
        .into_body()
        .into_with_config()
        .limit(total + 1)
        .reader();
    let mut file = fs::File::create(to)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    progress(0, total);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Parse("dictionary download cancelled".into()));
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])?;
        done += n as u64;
        if done > total {
            return Err(Error::Parse(
                "dictionary download is larger than expected".into(),
            ));
        }
        progress(done, total);
    }
    file.sync_all()?;
    if done != total {
        return Err(Error::Parse(format!(
            "dictionary download incomplete ({done} of {total} bytes)"
        )));
    }
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if digest != source.sha256 {
        return Err(Error::Parse(
            "dictionary download failed the checksum check".into(),
        ));
    }
    Ok(())
}

fn unpack(source: &DictionarySource, zip_path: &Path, dir: &Path) -> Result<PathBuf> {
    let bad = |m: &str| Error::Parse(format!("dictionary archive: {m}"));
    let mut archive =
        zip::ZipArchive::new(fs::File::open(zip_path)?).map_err(|e| bad(&e.to_string()))?;
    let final_dir = source.install_dir(dir);
    let tmp = dir.join(format!("{}.unpacking", source.dir_name));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp)?;
    let prefix = format!("{}/", source.dir_name);
    let limit = source.unpacked_size + source.unpacked_size / 10 + 1024 * 1024;
    let mut written = 0u64;
    let result = (|| {
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).map_err(|e| bad(&e.to_string()))?;
            if entry.is_dir() {
                continue;
            }
            let name = entry.name().map_err(|e| bad(&e.to_string()))?.into_owned();
            // Only plain files directly inside the expected directory.
            let Some(file_name) = name.strip_prefix(&prefix) else {
                return Err(bad(&format!("unexpected entry {name}")));
            };
            if file_name.is_empty()
                || file_name.contains(['/', '\\'])
                || file_name.starts_with('.')
                || file_name.contains("..")
            {
                return Err(bad(&format!("unexpected entry {name}")));
            }
            let mut out = fs::File::create(tmp.join(file_name))?;
            let copied = std::io::copy(&mut (&mut entry).take(limit - written + 1), &mut out)?;
            written += copied;
            if written > limit {
                return Err(bad("larger than expected"));
            }
        }
        if !tmp.join("metadata.json").exists() {
            return Err(bad("no metadata.json"));
        }
        fs::write(tmp.join(COMPLETE_MARKER), &source.sha256)?;
        let _ = fs::remove_dir_all(&final_dir);
        fs::rename(&tmp, &final_dir)?;
        Ok(final_dir.clone())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&tmp);
    }
    result
}

/// Deletes the installed dictionary (and unloads it if it was loaded from there).
pub fn remove(source: &DictionarySource, dir: &Path) -> Result<()> {
    let path = source.install_dir(dir);
    let mut loaded = LOADED.write().unwrap_or_else(|e| e.into_inner());
    if loaded.as_ref().is_some_and(|l| l.path == path) {
        *loaded = None;
    }
    drop(loaded);
    match fs::remove_dir_all(&path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

// ---- the loaded dictionary -----------------------------------------------------------

struct Loaded {
    path: PathBuf,
    segmenter: Arc<Mutex<Segmenter>>,
}

static LOADED: RwLock<Option<Loaded>> = RwLock::new(None);

/// Loads a dictionary directory (an [`install`]ed one, or any lindera dictionary)
/// and uses it for kanji readings from now on. Memory-mapped: cheap to keep loaded.
pub fn load(path: &Path) -> Result<()> {
    let dictionary = load_fs_dictionary(path)
        .map_err(|e| Error::Parse(format!("cannot load the dictionary: {e}")))?;
    let segmenter = Segmenter::new(Mode::Normal, dictionary, None);
    *LOADED.write().unwrap_or_else(|e| e.into_inner()) = Some(Loaded {
        path: path.to_owned(),
        segmenter: Arc::new(Mutex::new(segmenter)),
    });
    Ok(())
}

/// Stops using a loaded dictionary (kana-only again unless one is embedded).
pub fn unload() {
    *LOADED.write().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Where the loaded dictionary came from.
pub fn loaded_path() -> Option<PathBuf> {
    LOADED
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|l| l.path.clone())
}

/// True when kanji can be read: a dictionary is embedded (`japanese-dict`) or loaded.
pub fn kanji_readings_available() -> bool {
    super::HAS_KANJI_READINGS || loaded_path().is_some()
}

fn active_segmenter() -> Option<Arc<Mutex<Segmenter>>> {
    if let Some(l) = LOADED.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return Some(l.segmenter.clone());
    }
    embedded()
}

#[cfg(feature = "japanese-dict")]
fn embedded() -> Option<Arc<Mutex<Segmenter>>> {
    use std::sync::OnceLock;
    static SEG: OnceLock<Option<Arc<Mutex<Segmenter>>>> = OnceLock::new();
    SEG.get_or_init(|| {
        let dictionary = lindera::dictionary::load_dictionary("embedded://ipadic")
            .map_err(|e| log::warn!("lindera ipadic: {e}"))
            .ok()?;
        Some(Arc::new(Mutex::new(Segmenter::new(
            Mode::Normal,
            dictionary,
            None,
        ))))
    })
    .clone()
}

#[cfg(not(feature = "japanese-dict"))]
fn embedded() -> Option<Arc<Mutex<Segmenter>>> {
    None
}

/// Tokenizes with the active dictionary and romanizes each token's reading
/// (IPADIC detail 7); the particles は/へ/を read wa/e/o. `None` without a dictionary.
pub(super) fn romanize(text: &str) -> Option<String> {
    let segmenter = active_segmenter()?;
    let seg = segmenter.lock().ok()?;
    let mut tokens = seg.segment(Cow::Borrowed(text)).ok()?;
    let mut parts: Vec<(String, String, bool)> = Vec::with_capacity(tokens.len());
    for token in tokens.iter_mut() {
        let surface = token.surface.to_string();
        let details: Vec<String> = token.details().iter().map(|s| s.to_string()).collect();
        let particle = details.first().is_some_and(|p| p == "助詞");
        let reading = details
            .get(7)
            .filter(|r| !r.is_empty() && *r != "*")
            .cloned()
            .unwrap_or_else(|| surface.clone());
        parts.push((surface, reading, particle));
    }
    let mut out = String::new();
    for (i, (surface, reading, particle)) in parts.iter().enumerate() {
        if surface.trim().is_empty() {
            continue;
        }
        let romaji = match (particle, surface.as_str()) {
            (true, "は") => "wa".to_owned(),
            (true, "へ") => "e".to_owned(),
            (true, "を") => "o".to_owned(),
            _ => kana_to_romaji(reading, parts.get(i + 1).map(|p| p.1.as_str())),
        };
        let is_punct = romaji.chars().all(|c| !c.is_alphanumeric());
        if !out.is_empty() && !is_punct {
            out.push(' ');
        }
        out.push_str(&romaji);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::romanize::{Script, romanize_line};
    use crate::testutil::{Reply, TestServer};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "music-core-jadict-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Builds the tiny source dictionary in `tests/fixtures/lyrics/jadict` with lindera.
    fn build_fixture_dictionary(out: &Path) {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lyrics/jadict");
        lindera::dictionary::DictionaryBuilder::new(lindera::dictionary::Metadata::default())
            .build_dictionary(&src, out)
            .expect("builds the fixture dictionary");
    }

    /// A zip with `entries` (name, bytes), stored uncompressed.
    fn zip_of(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut w = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, data) in entries {
            w.start_file(name.as_str(), opts).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap();
        buf.into_inner()
    }

    fn sha256_hex(data: &[u8]) -> String {
        Sha256::digest(data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn source_for(server: &TestServer, zip: &[u8], unpacked: u64) -> DictionarySource {
        DictionarySource {
            name: "test".into(),
            url: format!("{}/dict.zip", server.base),
            sha256: sha256_hex(zip),
            size: zip.len() as u64,
            unpacked_size: unpacked,
            dir_name: "lindera-ipadic".into(),
        }
    }

    #[test]
    fn download_verify_load_and_romanize() {
        let work = temp_dir("build");
        let built = work.join("built");
        build_fixture_dictionary(&built);
        let mut entries = Vec::new();
        let mut unpacked = 0;
        for e in fs::read_dir(&built).unwrap().flatten() {
            let data = fs::read(e.path()).unwrap();
            unpacked += data.len() as u64;
            entries.push((
                format!("lindera-ipadic/{}", e.file_name().to_string_lossy()),
                data,
            ));
        }
        let zip = zip_of(&entries);
        let server = TestServer::file(zip.clone().into());
        let source = source_for(&server, &zip, unpacked);
        let cache = work.join("cache");
        assert!(installed(&source, &cache).is_none());

        let mut seen = Vec::new();
        let path = install(&source, &cache, &AtomicBool::new(false), |d, t| {
            seen.push((d, t))
        })
        .expect("installs");
        assert_eq!(seen.last(), Some(&(zip.len() as u64, zip.len() as u64)));
        assert_eq!(installed(&source, &cache).as_deref(), Some(path.as_path()));
        assert!(installed_size(&source, &cache) >= unpacked);
        // No leftovers next to the installed directory.
        let names: Vec<String> = fs::read_dir(&cache)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        // Installing again is a no-op (no request).
        let requests = server.requests.lock().unwrap().len();
        install(&source, &cache, &AtomicBool::new(false), |_, _| {}).unwrap();
        assert_eq!(server.requests.lock().unwrap().len(), requests);

        load(&path).expect("loads from disk");
        assert!(kanji_readings_available());
        assert_eq!(
            romanize_line("東京", Script::Japanese).as_deref(),
            Some("toukyou")
        );
        assert_eq!(
            romanize_line("君の名は", Script::Japanese).as_deref(),
            Some("kimi no na wa")
        );
        remove(&source, &cache).unwrap();
        assert!(loaded_path().is_none() || super::super::HAS_KANJI_READINGS);
        assert!(installed(&source, &cache).is_none());
        if !super::super::HAS_KANJI_READINGS {
            // Kana-only again.
            assert_eq!(
                romanize_line("君の名は", Script::Japanese).as_deref(),
                Some("君 no 名 ha")
            );
        }
        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn rejects_bad_downloads() {
        let work = temp_dir("bad");
        let good = zip_of(&[("lindera-ipadic/metadata.json".into(), b"{}".to_vec())]);

        // Checksum mismatch.
        let server = TestServer::file(good.clone().into());
        let mut source = source_for(&server, &good, 2);
        source.sha256 = "00".repeat(32);
        let err = install(&source, &work, &AtomicBool::new(false), |_, _| {}).unwrap_err();
        assert!(err.to_string().contains("checksum"), "{err}");

        // Wrong size.
        let mut source = source_for(&server, &good, 2);
        source.size += 10;
        assert!(install(&source, &work, &AtomicBool::new(false), |_, _| {}).is_err());

        // Paths outside the dictionary directory.
        for name in [
            "lindera-ipadic/../evil",
            "other/metadata.json",
            "lindera-ipadic/a/b",
        ] {
            let zip = zip_of(&[(name.into(), b"x".to_vec())]);
            let server = TestServer::file(zip.clone().into());
            let source = source_for(&server, &zip, 1);
            let err = install(&source, &work, &AtomicBool::new(false), |_, _| {}).unwrap_err();
            assert!(
                err.to_string().contains("unexpected entry"),
                "{name}: {err}"
            );
        }

        // More data than declared (zip bomb guard).
        let big = zip_of(&[("lindera-ipadic/metadata.json".into(), vec![b' '; 4 << 20])]);
        let server = TestServer::file(big.clone().into());
        let source = source_for(&server, &big, 10);
        let err = install(&source, &work, &AtomicBool::new(false), |_, _| {}).unwrap_err();
        assert!(err.to_string().contains("larger than expected"), "{err}");

        // HTTP errors and cancellation.
        let server = TestServer::start(|_, _, _| Reply::Status(404));
        let source = source_for(&server, &good, 2);
        let err = install(&source, &work, &AtomicBool::new(false), |_, _| {}).unwrap_err();
        assert!(err.to_string().contains("404"), "{err}");
        let server = TestServer::file(good.clone().into());
        let source = source_for(&server, &good, 2);
        let err = install(&source, &work, &AtomicBool::new(true), |_, _| {}).unwrap_err();
        assert!(err.to_string().contains("cancelled"), "{err}");

        // Nothing was left behind.
        let left: Vec<_> = fs::read_dir(&work).unwrap().flatten().collect();
        assert!(left.is_empty(), "{left:?}");
        // A broken dictionary directory fails to load (and changes nothing).
        assert!(load(&work).is_err());
        let _ = fs::remove_dir_all(&work);
    }
}
