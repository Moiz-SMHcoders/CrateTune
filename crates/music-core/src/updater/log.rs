//! `update.log`: what the updater did, one timestamped line per step (also sent
//! to the `log` crate).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Kept below this size (the older half is dropped).
const MAX_LOG_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone)]
pub struct UpdateLog {
    path: PathBuf,
}

impl UpdateLog {
    pub fn new(path: PathBuf) -> Self {
        UpdateLog { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn line(&self, message: &str) {
        log::info!("update: {message}");
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() > MAX_LOG_BYTES)
            && let Ok(text) = std::fs::read_to_string(&self.path)
        {
            // From the first line break after the middle (a char boundary).
            let half = text.len() / 2;
            let cut = text.as_bytes()[half..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(text.len(), |i| half + i + 1);
            let _ = std::fs::write(&self.path, &text[cut..]);
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(
                file,
                "{} [{}] {message}",
                format_utc(secs),
                std::process::id()
            );
        }
    }
}

/// `2026-10-29T12:34:56Z`.
fn format_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_timestamps() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(1_793_276_096), "2026-10-29T12:14:56Z");
    }

    #[test]
    fn appends_lines() {
        let path = std::env::temp_dir().join(format!("updater-log-{}.log", std::process::id()));
        let log = UpdateLog::new(path.clone());
        log.line("one");
        log.line("two");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("] one\n") && text.contains("] two\n"),
            "{text}"
        );
        std::fs::remove_file(path).unwrap();
    }
}
