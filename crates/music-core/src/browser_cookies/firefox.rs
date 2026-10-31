//! Firefox's cookie store: plain SQLite (`cookies.sqlite`, table `moz_cookies`).

use std::path::Path;

use super::{Cookie, ImportError, TempCopy};

/// `moz_cookies.expiry` → Unix seconds (newer Firefox versions store milliseconds).
fn expires(expiry: i64) -> Option<i64> {
    match expiry {
        e if e <= 0 => None,
        e if e > 100_000_000_000 => Some(e / 1000),
        e => Some(e),
    }
}

/// The youtube.com cookies of the Firefox database at `db` (copied first).
pub fn read(db: &Path) -> Result<Vec<Cookie>, ImportError> {
    let copy = TempCopy::of(db)?;
    let conn = rusqlite::Connection::open(&copy.path)?;
    let mut stmt = conn.prepare(
        "SELECT host, name, value, expiry FROM moz_cookies \
         WHERE host = 'youtube.com' OR host LIKE '%.youtube.com'",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Cookie {
            host: r.get(0)?,
            name: r.get(1)?,
            value: r.get(2)?,
            expires: expires(r.get::<_, Option<i64>>(3)?.unwrap_or(0)),
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser_cookies::tests::temp_home;
    use crate::browser_cookies::{cookie_header, unix_now};

    /// A `cookies.sqlite` like Firefox's with `rows` (host, name, value, expiry).
    fn fixture_db(path: &Path, rows: &[(&str, &str, &str, i64)]) {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE moz_cookies (id INTEGER PRIMARY KEY, originAttributes TEXT NOT NULL DEFAULT '',
               name TEXT, value TEXT, host TEXT, path TEXT, expiry INTEGER, lastAccessed INTEGER,
               creationTime INTEGER, isSecure INTEGER, isHttpOnly INTEGER);",
        )
        .unwrap();
        for (host, name, value, expiry) in rows {
            conn.execute(
                "INSERT INTO moz_cookies(name, value, host, path, expiry) VALUES (?1, ?2, ?3, '/', ?4)",
                rusqlite::params![name, value, host, expiry],
            )
            .unwrap();
        }
    }

    #[test]
    fn reads_youtube_cookies_only() {
        let home = temp_home("firefox");
        let db = home.join("cookies.sqlite");
        let later = unix_now() + 86_400;
        fixture_db(
            &db,
            &[
                (".youtube.com", "SAPISID", "ff-sap", later),
                (".youtube.com", "__Secure-3PSID", "ff-3psid", later * 1000),
                (".youtube.com", "OLD", "gone", unix_now() - 10),
                (".google.com", "SID", "not-read", later),
                ("accounts.youtube.com.evil.example", "X", "no", later),
            ],
        );
        let cookies = read(&db).unwrap();
        assert_eq!(cookies.len(), 3);
        assert_eq!(cookies[1].expires, Some(later), "milliseconds understood");
        assert_eq!(
            cookie_header(&cookies).unwrap(),
            "SAPISID=ff-sap; __Secure-3PSID=ff-3psid"
        );
        assert_eq!(
            read(&home.join("missing.sqlite")),
            Err(ImportError::NotFound)
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}
