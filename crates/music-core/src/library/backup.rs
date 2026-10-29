// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/backup/BackupArchiveRepository.kt
// — © Rukamori and ArchiveTune contributors, GPL-3.0

//! Backup and restore: the database (copied with SQLite's online backup API, so it is
//! consistent while the app keeps running) plus the app's settings JSON, in one zip.
//!
//! Archive layout: `manifest.json` ([`BackupManifest`]), `library.sqlite`, and optionally
//! `settings.json` (exactly what the caller passed in).

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use super::{Library, SCHEMA_VERSION, migrate, now_ms};
use crate::{Error, Result};

pub const BACKUP_MANIFEST_ENTRY: &str = "manifest.json";
pub const BACKUP_DB_ENTRY: &str = "library.sqlite";
pub const BACKUP_SETTINGS_ENTRY: &str = "settings.json";

const FORMAT: &str = "music-macos-backup";
const FORMAT_VERSION: u32 = 1;
/// Refuse absurd entries (zip bombs) when restoring.
const MAX_ENTRY_BYTES: u64 = 4 << 30;

/// Describes a backup archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupManifest {
    pub format: String,
    pub format_version: u32,
    /// Library schema version of the contained database.
    pub schema_version: u32,
    pub created_at: i64,
    pub has_settings: bool,
}

/// What [`Library::restore_backup`] brought back besides the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoredBackup {
    pub manifest: BackupManifest,
    /// The settings JSON stored in the backup, for the caller to apply.
    pub settings_json: Option<String>,
}

fn storage(e: impl std::fmt::Display) -> Error {
    Error::Storage(e.to_string())
}

/// A sibling path that doesn't exist yet, removed on drop.
struct TempPath(PathBuf);

impl TempPath {
    fn next_to(path: &Path, suffix: &str) -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let name = format!(
            ".{}.{}-{}{suffix}",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        );
        let dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        TempPath(dir.join(name))
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        for ext in ["-wal", "-shm", "-journal"] {
            let mut p = self.0.clone().into_os_string();
            p.push(ext);
            let _ = std::fs::remove_file(p);
        }
    }
}

fn read_entry(archive: &mut ZipArchive<File>, name: &str) -> Result<Option<Vec<u8>>> {
    let mut entry = match archive.by_name(name) {
        Ok(e) => e,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(storage(e)),
    };
    if entry.size() > MAX_ENTRY_BYTES {
        return Err(storage(format!("backup entry {name} is too large")));
    }
    let mut buf = Vec::with_capacity(entry.size() as usize);
    entry.by_ref().take(MAX_ENTRY_BYTES).read_to_end(&mut buf)?;
    Ok(Some(buf))
}

fn read_manifest(archive: &mut ZipArchive<File>) -> Result<BackupManifest> {
    let bytes = read_entry(archive, BACKUP_MANIFEST_ENTRY)?
        .ok_or_else(|| storage("not a backup: manifest.json missing"))?;
    let manifest: BackupManifest =
        serde_json::from_slice(&bytes).map_err(|e| storage(format!("bad manifest: {e}")))?;
    if manifest.format != FORMAT {
        return Err(storage(format!(
            "not a backup: format {:?}",
            manifest.format
        )));
    }
    if manifest.format_version > FORMAT_VERSION {
        return Err(storage(format!(
            "backup format v{} is newer than this build",
            manifest.format_version
        )));
    }
    if manifest.schema_version > SCHEMA_VERSION {
        return Err(storage(format!(
            "backup library schema v{} is newer than this build (v{SCHEMA_VERSION})",
            manifest.schema_version
        )));
    }
    Ok(manifest)
}

/// Opens a restored database read-only and checks that it is an intact library.
fn validate_database(path: &Path) -> Result<()> {
    let c = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| storage(format!("backup database: {e}")))?;
    let check: String = c
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(|e| storage(format!("backup database: {e}")))?;
    if check != "ok" {
        return Err(storage(format!("backup database is corrupt: {check}")));
    }
    let version: u32 = c.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version == 0 || version > SCHEMA_VERSION {
        return Err(storage(format!(
            "backup database has unsupported schema v{version}"
        )));
    }
    let has_songs: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'song')",
        [],
        |r| r.get(0),
    )?;
    if !has_songs {
        return Err(storage("backup database is not a music library"));
    }
    Ok(())
}

impl Library {
    /// Writes a backup zip to `path` (atomically replacing it). `settings_json` must be valid
    /// JSON if given.
    pub fn export_backup(
        &self,
        path: impl AsRef<Path>,
        settings_json: Option<&str>,
    ) -> Result<BackupManifest> {
        let path = path.as_ref();
        if let Some(json) = settings_json {
            serde_json::from_str::<serde_json::Value>(json)
                .map_err(|e| storage(format!("settings are not valid JSON: {e}")))?;
        }
        let db_copy = TempPath::next_to(path, ".sqlite");
        {
            let conn = self.lock();
            conn.backup(
                rusqlite::MAIN_DB,
                &db_copy.0,
                None::<fn(rusqlite::backup::Progress)>,
            )?;
        }
        // The copy inherits WAL mode; fold it into a single self-contained file.
        {
            let c = Connection::open(&db_copy.0)?;
            c.pragma_update(None, "journal_mode", "DELETE")?;
        }
        let manifest = BackupManifest {
            format: FORMAT.into(),
            format_version: FORMAT_VERSION,
            schema_version: self.schema_version()?,
            created_at: now_ms(),
            has_settings: settings_json.is_some(),
        };

        let zip_tmp = TempPath::next_to(path, ".zip");
        {
            let mut zip = ZipWriter::new(File::create(&zip_tmp.0)?);
            let options =
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
            zip.start_file(BACKUP_MANIFEST_ENTRY, options)
                .map_err(storage)?;
            zip.write_all(&serde_json::to_vec_pretty(&manifest).map_err(storage)?)?;
            zip.start_file(BACKUP_DB_ENTRY, options.large_file(true))
                .map_err(storage)?;
            std::io::copy(&mut File::open(&db_copy.0)?, &mut zip)?;
            if let Some(json) = settings_json {
                zip.start_file(BACKUP_SETTINGS_ENTRY, options)
                    .map_err(storage)?;
                zip.write_all(json.as_bytes())?;
            }
            zip.finish().map_err(storage)?.sync_all()?;
        }
        std::fs::rename(&zip_tmp.0, path)?;
        Ok(manifest)
    }

    /// Reads and validates a backup's manifest without restoring anything.
    pub fn read_backup_manifest(path: impl AsRef<Path>) -> Result<BackupManifest> {
        let mut archive = ZipArchive::new(File::open(path)?).map_err(storage)?;
        read_manifest(&mut archive)
    }

    /// Replaces the whole library with the backup at `path`, after validating it (manifest,
    /// SQLite integrity, schema). On any validation error the library is left untouched.
    /// Older schemas are migrated. Returns the stored settings for the caller to apply.
    pub fn restore_backup(&self, path: impl AsRef<Path>) -> Result<RestoredBackup> {
        let path = path.as_ref();
        let mut archive = ZipArchive::new(File::open(path)?).map_err(storage)?;
        let manifest = read_manifest(&mut archive)?;
        let settings_json = read_entry(&mut archive, BACKUP_SETTINGS_ENTRY)?
            .map(|b| String::from_utf8(b).map_err(|_| storage("settings.json is not UTF-8")))
            .transpose()?;
        if let Some(json) = &settings_json {
            serde_json::from_str::<serde_json::Value>(json)
                .map_err(|e| storage(format!("backup settings are not valid JSON: {e}")))?;
        }

        let db_tmp = TempPath::next_to(path, ".sqlite");
        {
            let mut entry = archive
                .by_name(BACKUP_DB_ENTRY)
                .map_err(|_| storage("backup has no library.sqlite"))?;
            let mut out = File::create(&db_tmp.0)?;
            let copied = std::io::copy(&mut entry.by_ref().take(MAX_ENTRY_BYTES), &mut out)?;
            if copied >= MAX_ENTRY_BYTES {
                return Err(storage("backup database is too large"));
            }
            out.sync_all()?;
        }
        validate_database(&db_tmp.0)?;

        let mut conn = self.lock();
        conn.restore(
            rusqlite::MAIN_DB,
            &db_tmp.0,
            None::<fn(rusqlite::backup::Progress)>,
        )?;
        conn.pragma_update(None, "foreign_keys", true)?;
        migrate(&mut conn)?;
        Ok(RestoredBackup {
            manifest,
            settings_json,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::test_util::{tempdir, track};
    use crate::library::{SongScope, TimeZone};

    #[test]
    fn export_and_restore_round_trip() {
        let dir = tempdir();
        let lib = Library::open(dir.join("lib.sqlite")).unwrap();
        let t = track("v1", "Song", "Artist", Some(("MPREb_1", "Album")));
        lib.like(&t).unwrap();
        lib.record_play(&t, 5_000, 1_000).unwrap();
        let p = lib.create_playlist("Keep").unwrap();
        lib.add_to_playlist(&p.id, std::slice::from_ref(&t))
            .unwrap();

        let zip_path = dir.join("backup.zip");
        let manifest = lib
            .export_backup(&zip_path, Some(r#"{"volume":0.5}"#))
            .unwrap();
        assert!(manifest.has_settings);
        assert_eq!(Library::read_backup_manifest(&zip_path).unwrap(), manifest);

        // Change things after the backup, then restore into the live handle.
        lib.unlike("v1").unwrap();
        lib.delete_playlist(&p.id).unwrap();
        lib.add_to_library(&track("v2", "Other", "B", None))
            .unwrap();
        let other_handle = lib.clone();

        let restored = lib.restore_backup(&zip_path).unwrap();
        assert_eq!(restored.settings_json.as_deref(), Some(r#"{"volume":0.5}"#));
        assert_eq!(restored.manifest, manifest);
        assert!(other_handle.song("v1").unwrap().unwrap().liked());
        assert!(other_handle.song("v2").unwrap().is_none());
        assert_eq!(other_handle.playlist_entries(&p.id).unwrap().len(), 1);
        assert_eq!(lib.history(TimeZone::Offset(0), 10, 0).unwrap().len(), 1);
        assert_eq!(lib.song_count(SongScope::Liked).unwrap(), 1);
        // Foreign keys are still enforced after the restore.
        lib.delete_song("v1").unwrap();
        assert!(lib.playlist_entries(&p.id).unwrap().is_empty());

        // Restoring into a fresh, separate library works too.
        let fresh = Library::open_in_memory().unwrap();
        assert!(
            fresh
                .restore_backup(&zip_path)
                .unwrap()
                .settings_json
                .is_some()
        );
        assert!(fresh.song("v1").unwrap().is_some());

        // No temp files left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        // Windows can't delete a database file that is still open.
        drop((lib, other_handle));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn export_without_settings_and_rejects_bad_settings() {
        let dir = tempdir();
        let lib = Library::open_in_memory().unwrap();
        let zip_path = dir.join("b.zip");
        assert!(lib.export_backup(&zip_path, Some("{not json")).is_err());
        assert!(!zip_path.exists());
        let m = lib.export_backup(&zip_path, None).unwrap();
        assert!(!m.has_settings);
        assert_eq!(lib.restore_backup(&zip_path).unwrap().settings_json, None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let mut zip = ZipWriter::new(File::create(path).unwrap());
        for (name, data) in entries {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn restore_rejects_invalid_archives_and_keeps_library() {
        let dir = tempdir();
        let lib = Library::open_in_memory().unwrap();
        lib.add_to_library(&track("keep", "K", "A", None)).unwrap();
        let manifest = |schema: u32, format: &str| {
            serde_json::to_vec(&BackupManifest {
                format: format.into(),
                format_version: 1,
                schema_version: schema,
                created_at: 0,
                has_settings: false,
            })
            .unwrap()
        };
        let good = manifest(SCHEMA_VERSION, FORMAT);
        type Entries = Vec<(&'static str, Vec<u8>)>;
        let cases: Vec<(&str, Entries)> = vec![
            ("no manifest", vec![(BACKUP_DB_ENTRY, b"x".to_vec())]),
            (
                "wrong format",
                vec![(BACKUP_MANIFEST_ENTRY, manifest(1, "other"))],
            ),
            (
                "newer schema",
                vec![(BACKUP_MANIFEST_ENTRY, manifest(SCHEMA_VERSION + 1, FORMAT))],
            ),
            ("no database", vec![(BACKUP_MANIFEST_ENTRY, good.clone())]),
            (
                "garbage database",
                vec![
                    (BACKUP_MANIFEST_ENTRY, good.clone()),
                    (BACKUP_DB_ENTRY, vec![7u8; 5000]),
                ],
            ),
            (
                "bad settings",
                vec![
                    (BACKUP_MANIFEST_ENTRY, good.clone()),
                    (BACKUP_SETTINGS_ENTRY, b"{".to_vec()),
                ],
            ),
        ];
        for (name, entries) in cases {
            let path = dir.join(format!("{}.zip", name.replace(' ', "_")));
            let entries: Vec<(&str, &[u8])> =
                entries.iter().map(|(n, d)| (*n, d.as_slice())).collect();
            write_zip(&path, &entries);
            assert!(lib.restore_backup(&path).is_err(), "{name} accepted");
        }
        // A valid SQLite file that isn't a library.
        let foreign = dir.join("foreign.sqlite");
        {
            let c = Connection::open(&foreign).unwrap();
            c.execute_batch("CREATE TABLE t(x); PRAGMA user_version = 1;")
                .unwrap();
        }
        let db = std::fs::read(&foreign).unwrap();
        let path = dir.join("foreign.zip");
        write_zip(
            &path,
            &[(BACKUP_MANIFEST_ENTRY, &good), (BACKUP_DB_ENTRY, &db)],
        );
        assert!(lib.restore_backup(&path).is_err());
        assert!(lib.restore_backup(dir.join("missing.zip")).is_err());
        std::fs::write(dir.join("notzip.zip"), b"hello").unwrap();
        assert!(lib.restore_backup(dir.join("notzip.zip")).is_err());

        assert!(lib.song("keep").unwrap().is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
