//! Safari's cookie store: `Cookies.binarycookies` (macOS; in Safari's container,
//! readable with Full Disk Access only).
//!
//! Layout (big-endian header, little-endian pages): `cook`, page count, page sizes,
//! then the pages. A page: `00 00 01 00`, cookie count, cookie offsets, then the
//! cookies. A cookie: size, flags, offsets of domain / name / path / value (from the
//! cookie's start), the expiry as Mac absolute time (seconds since 2001), and the
//! NUL-terminated strings.

use std::path::Path;

use super::{Cookie, ImportError, is_youtube_host};

/// Seconds between 1970 and 2001 (Mac absolute time).
const MAC_EPOCH: f64 = 978_307_200.0;

fn u32_be(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn u32_le(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn f64_le(data: &[u8], at: usize) -> Option<f64> {
    Some(f64::from_le_bytes(data.get(at..at + 8)?.try_into().ok()?))
}

fn cstring(data: &[u8], at: usize) -> Option<String> {
    let rest = data.get(at..)?;
    let end = rest.iter().position(|b| *b == 0)?;
    String::from_utf8(rest[..end].to_vec()).ok()
}

fn parse_cookie(record: &[u8]) -> Option<Cookie> {
    let field = |n: usize| u32_le(record, 16 + 4 * n).map(|v| v as usize);
    let (domain, name, value) = (field(0)?, field(1)?, field(3)?);
    let expiry = f64_le(record, 40)?;
    Some(Cookie {
        host: cstring(record, domain)?,
        name: cstring(record, name)?,
        value: cstring(record, value)?,
        expires: (expiry > 0.0).then_some((expiry + MAC_EPOCH) as i64),
    })
}

/// The youtube.com cookies in a `Cookies.binarycookies` file's bytes.
pub fn parse(data: &[u8]) -> Result<Vec<Cookie>, ImportError> {
    let bad = |what: &str| ImportError::Format(format!("binarycookies: {what}"));
    if data.get(..4) != Some(b"cook") {
        return Err(bad("no signature"));
    }
    let pages = u32_be(data, 4).ok_or_else(|| bad("no page count"))? as usize;
    let mut sizes = Vec::with_capacity(pages.min(4096));
    for i in 0..pages {
        sizes.push(u32_be(data, 8 + 4 * i).ok_or_else(|| bad("page sizes"))? as usize);
    }
    let mut at = 8 + 4 * pages;
    let mut out = Vec::new();
    for size in sizes {
        let page = data
            .get(at..at + size)
            .ok_or_else(|| bad("page past the end"))?;
        at += size;
        if page.get(..4) != Some(&[0, 0, 1, 0]) {
            return Err(bad("page signature"));
        }
        let count = u32_le(page, 4).ok_or_else(|| bad("cookie count"))? as usize;
        for i in 0..count {
            let offset = u32_le(page, 8 + 4 * i).ok_or_else(|| bad("cookie offsets"))? as usize;
            let len = u32_le(page, offset).ok_or_else(|| bad("cookie size"))? as usize;
            let record = page
                .get(offset..offset + len)
                .ok_or_else(|| bad("cookie past the page"))?;
            // A record that doesn't parse is skipped, not fatal.
            if let Some(cookie) = parse_cookie(record)
                && is_youtube_host(&cookie.host)
            {
                out.push(cookie);
            }
        }
    }
    Ok(out)
}

/// Reads Safari's cookie file at `path` (youtube.com cookies only are kept).
pub fn read(path: &Path) -> Result<Vec<Cookie>, ImportError> {
    let data = std::fs::read(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => ImportError::NotFound,
        _ => ImportError::PermissionDenied,
    })?;
    parse(&data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser_cookies::cookie_header;

    /// One cookie record as Safari writes it.
    fn record(domain: &str, name: &str, value: &str, expiry: f64) -> Vec<u8> {
        let mut strings = Vec::new();
        let header_len = 56;
        let mut offsets = Vec::new();
        for s in [domain, name, "/", value] {
            offsets.push((header_len + strings.len()) as u32);
            strings.extend_from_slice(s.as_bytes());
            strings.push(0);
        }
        let size = (header_len + strings.len()) as u32;
        let mut r = Vec::new();
        r.extend_from_slice(&size.to_le_bytes());
        r.extend_from_slice(&1u32.to_le_bytes());
        r.extend_from_slice(&5u32.to_le_bytes()); // flags: secure | http-only
        r.extend_from_slice(&0u32.to_le_bytes());
        for o in offsets {
            r.extend_from_slice(&o.to_le_bytes());
        }
        r.extend_from_slice(&[0u8; 8]);
        r.extend_from_slice(&expiry.to_le_bytes());
        r.extend_from_slice(&0f64.to_le_bytes());
        assert_eq!(r.len(), header_len);
        r.extend_from_slice(&strings);
        r
    }

    fn page(records: &[Vec<u8>]) -> Vec<u8> {
        let mut p = vec![0, 0, 1, 0];
        p.extend_from_slice(&(records.len() as u32).to_le_bytes());
        let mut offset = 8 + 4 * records.len() + 4;
        for r in records {
            p.extend_from_slice(&(offset as u32).to_le_bytes());
            offset += r.len();
        }
        p.extend_from_slice(&[0, 0, 0, 0]);
        for r in records {
            p.extend_from_slice(r);
        }
        p
    }

    pub(crate) fn file(pages: &[Vec<u8>]) -> Vec<u8> {
        let mut f = b"cook".to_vec();
        f.extend_from_slice(&(pages.len() as u32).to_be_bytes());
        for p in pages {
            f.extend_from_slice(&(p.len() as u32).to_be_bytes());
        }
        for p in pages {
            f.extend_from_slice(p);
        }
        f.extend_from_slice(&[0u8; 8]);
        f
    }

    #[test]
    fn parses_binarycookies() {
        let later = (crate::browser_cookies::unix_now() as f64) - MAC_EPOCH + 86_400.0;
        let data = file(&[
            page(&[
                record(".youtube.com", "SAPISID", "safari-sap", later),
                record(".google.com", "SID", "not-kept", later),
            ]),
            page(&[record("music.youtube.com", "PREF", "hl=en", 0.0)]),
        ]);
        let cookies = parse(&data).unwrap();
        assert_eq!(cookies.len(), 2);
        assert_eq!(cookies[1].expires, None, "session cookie");
        assert_eq!(
            cookie_header(&cookies).unwrap(),
            "PREF=hl=en; SAPISID=safari-sap"
        );
        assert!(matches!(parse(b"nope"), Err(ImportError::Format(_))));
        assert!(matches!(parse(&data[..20]), Err(ImportError::Format(_))));
    }
}
