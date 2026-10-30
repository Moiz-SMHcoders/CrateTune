//! `Set-Cookie` handling for the signed-in session, the way a browser keeps it fresh.
//!
//! Google rotates some session cookies (`__Secure-1PSIDTS`, `__Secure-3PSIDTS`,
//! `SIDCC`, `__Secure-1PSIDCC`, `__Secure-3PSIDCC`, …) through `Set-Cookie` on
//! responses. The app stores one `Cookie` header for `.youtube.com`; this module
//! parses `Set-Cookie` lines and merges them into that header: a cookie is replaced by
//! name, a deletion (`Max-Age<=0`, `Expires` in the past) removes it, and anything
//! that isn't a youtube.com cookie from a trusted host is ignored.
//!
//! **Trust:** lines are only taken from responses of `https://` hosts on
//! `youtube.com` / `google.com` (or a loopback stand-in for music.youtube.com set
//! with `InnerTube::with_api_base` / `MUSIC_INNERTUBE_API`, which already receives
//! the cookie). Only cookies whose domain is youtube.com (or one of its subdomains)
//! go into the header: a `.google.com` cookie isn't sent to music.youtube.com by a
//! browser either. Nothing here logs names or values.

/// Upper bound for the merged header; new cookies beyond it are dropped.
const MAX_HEADER_BYTES: usize = 16 * 1024;

/// One parsed `Set-Cookie` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SetCookie {
    pub name: String,
    pub value: String,
    /// `Domain` without a leading dot, lowercase (`None`: host-only).
    pub domain: Option<String>,
    /// `Path` (`None`: the default, which for our requests is `/`).
    pub path: Option<String>,
    /// `Max-Age<=0` or `Expires` in the past: the browser deletes the cookie.
    pub delete: bool,
}

/// Parses one `Set-Cookie` header value; `now` is the current unix time (for
/// `Expires`). `None` for malformed lines.
pub(crate) fn parse_set_cookie(line: &str, now: u64) -> Option<SetCookie> {
    let mut parts = line.split(';');
    let (name, value) = parts.next()?.split_once('=')?;
    let (name, value) = (name.trim(), value.trim());
    let value = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value);
    if !is_token(name) || !value.bytes().all(is_cookie_octet) {
        return None;
    }
    let mut cookie = SetCookie {
        name: name.to_owned(),
        value: value.to_owned(),
        domain: None,
        path: None,
        delete: false,
    };
    // Max-Age wins over Expires (RFC 6265 §5.3 step 3).
    let mut max_age: Option<i64> = None;
    let mut expires: Option<u64> = None;
    for attr in parts {
        let (key, val) = match attr.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => (attr.trim(), ""),
        };
        match key.to_ascii_lowercase().as_str() {
            "domain" if !val.is_empty() => {
                cookie.domain = Some(val.trim_start_matches('.').to_ascii_lowercase());
            }
            "path" if !val.is_empty() => cookie.path = Some(val.to_owned()),
            "max-age" => {
                if let Ok(n) = val.parse::<i64>() {
                    max_age = Some(n);
                }
            }
            "expires" => expires = parse_http_date(val),
            _ => {}
        }
    }
    cookie.delete = match (max_age, expires) {
        (Some(n), _) => n <= 0,
        (None, Some(at)) => at <= now,
        (None, None) => false,
    };
    Some(cookie)
}

/// RFC 6265 token (cookie name).
fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_graphic() && !b"()<>@,;:\\\"/[]?={}".contains(&b))
}

/// RFC 6265 `cookie-octet` (lenient: no space, `"`, `,`, `;`, `\`, controls).
fn is_cookie_octet(b: u8) -> bool {
    b.is_ascii_graphic() && !matches!(b, b'"' | b',' | b';' | b'\\')
}

/// The host of `origin` (`https://music.youtube.com[:443][/…]`), lowercase.
fn host_of(origin: &str) -> Option<(&'static str, String)> {
    let (scheme, rest) = match origin.strip_prefix("https://") {
        Some(r) => ("https", r),
        None => ("http", origin.strip_prefix("http://")?),
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next()?,
        None => authority.split(':').next()?,
    };
    (!host.is_empty()).then(|| (scheme, host.to_ascii_lowercase()))
}

fn is_within(host: &str, domain: &str) -> bool {
    host == domain || host.ends_with(&format!(".{domain}"))
}

/// The host whose `Set-Cookie` lines may update the session: an `https://` YouTube or
/// Google host, or a loopback stand-in for music.youtube.com (`loopback_ok`).
/// Returns the host to domain-match cookies against.
pub(crate) fn trusted_host(origin: &str, loopback_ok: bool) -> Option<String> {
    let (scheme, host) = host_of(origin)?;
    let google = ["youtube.com", "google.com"]
        .iter()
        .any(|d| is_within(&host, d));
    if scheme == "https" && google {
        return Some(host);
    }
    let loopback = matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1");
    // The stand-in plays music.youtube.com.
    (loopback_ok && scheme == "http" && loopback).then(|| "music.youtube.com".to_owned())
}

/// Whether `cookie`, received from `host` (see [`trusted_host`]), belongs in the
/// `.youtube.com` cookie header.
pub(crate) fn applies(cookie: &SetCookie, host: &str) -> bool {
    if cookie.path.as_deref().is_some_and(|p| p != "/") {
        return false;
    }
    let domain = cookie.domain.as_deref().unwrap_or(host);
    // The host must be allowed to set it (RFC 6265 domain match) and it must be a
    // youtube.com cookie (the header is sent to music.youtube.com).
    is_within(host, domain) && is_within(domain, "youtube.com")
}

/// `cookie` (a `Cookie` header) with `updates` applied: same name → new value in
/// place, new names appended, deletions removed. `None` when nothing changed.
pub(crate) fn merge(cookie: &str, updates: &[SetCookie]) -> Option<String> {
    let mut pairs: Vec<(String, String)> = cookie
        .split(';')
        .filter_map(|part| {
            let (k, v) = part.split_once('=')?;
            let k = k.trim();
            (!k.is_empty()).then(|| (k.to_owned(), v.trim().to_owned()))
        })
        .collect();
    let mut changed = false;
    for update in updates {
        let existing = pairs.iter().position(|(k, _)| *k == update.name);
        match (existing, update.delete) {
            (Some(_), true) => {
                // Also drops duplicates of the name.
                pairs.retain(|(k, _)| *k != update.name);
                changed = true;
            }
            (None, true) => {}
            (Some(i), false) => {
                if pairs[i].1 != update.value {
                    pairs[i].1 = update.value.clone();
                    changed = true;
                }
            }
            (None, false) => {
                let size: usize = pairs.iter().map(|(k, v)| k.len() + v.len() + 3).sum();
                if size + update.name.len() + update.value.len() + 3 <= MAX_HEADER_BYTES {
                    pairs.push((update.name.clone(), update.value.clone()));
                    changed = true;
                }
            }
        }
    }
    changed.then(|| {
        pairs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    })
}

/// Parses `lines` from a response of `origin` and returns the cookies to apply.
pub(crate) fn applicable<'a>(
    origin: &str,
    loopback_ok: bool,
    lines: impl IntoIterator<Item = &'a str>,
    now: u64,
) -> Vec<SetCookie> {
    let Some(host) = trusted_host(origin, loopback_ok) else {
        return Vec::new();
    };
    lines
        .into_iter()
        .filter_map(|l| parse_set_cookie(l, now))
        .filter(|c| applies(c, &host))
        .collect()
}

/// Unix time of an HTTP date: IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`), the
/// cookie variant with dashes (`Sun, 06-Nov-1994 08:49:37 GMT`) and two-digit years.
pub(crate) fn parse_http_date(s: &str) -> Option<u64> {
    let s = s.trim();
    let s = s.split_once(',').map_or(s, |(_, rest)| rest).trim();
    let mut fields = s.split([' ', '-']).filter(|f| !f.is_empty());
    let day: u32 = fields.next()?.parse().ok()?;
    let month = match fields.next()?.get(..3)?.to_ascii_lowercase().as_str() {
        "jan" => 1,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    };
    let mut year: i64 = fields.next()?.parse().ok()?;
    if year < 70 {
        year += 2000;
    } else if year < 100 {
        year += 1900;
    }
    let mut hms = fields.next()?.split(':').map(|n| n.parse::<u64>().ok());
    let (h, m, sec) = (hms.next()??, hms.next()??, hms.next()??);
    if !(1..=31).contains(&day) || h > 23 || m > 59 || sec > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    (days >= 0).then(|| days as u64 * 86_400 + h * 3600 + m * 60 + sec)
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn sc(line: &str) -> SetCookie {
        parse_set_cookie(line, NOW).unwrap()
    }

    #[test]
    fn parses_attributes() {
        let c = sc(
            "__Secure-1PSIDTS=sidts-NEW; Domain=.youtube.com; Path=/; Expires=Fri, 30-Oct-2027 10:00:00 GMT; Secure; HttpOnly; Priority=HIGH; SameParty",
        );
        assert_eq!(c.name, "__Secure-1PSIDTS");
        assert_eq!(c.value, "sidts-NEW");
        assert_eq!(c.domain.as_deref(), Some("youtube.com"));
        assert_eq!(c.path.as_deref(), Some("/"));
        assert!(!c.delete);
        // Quoted values lose their quotes; attribute names are case-insensitive.
        assert!(
            parse_set_cookie("A=\"b c\"", NOW).is_none(),
            "space in value"
        );
        let q = sc("A=\"bc\"; PATH=/x; DOMAIN=Music.YouTube.com");
        assert_eq!(q.value, "bc");
        assert_eq!(q.path.as_deref(), Some("/x"));
        assert_eq!(q.domain.as_deref(), Some("music.youtube.com"));
        // Malformed.
        assert!(parse_set_cookie("novalue", NOW).is_none());
        assert!(parse_set_cookie("=v", NOW).is_none());
        assert!(parse_set_cookie("a b=v", NOW).is_none());
        assert!(parse_set_cookie("a=v\u{7}", NOW).is_none());
    }

    #[test]
    fn deletions() {
        assert!(sc("SIDCC=; Max-Age=0").delete);
        assert!(sc("SIDCC=x; max-age=-1").delete);
        assert!(sc("SIDCC=x; Expires=Thu, 01 Jan 1970 00:00:01 GMT").delete);
        assert!(!sc("SIDCC=x; Expires=Thu, 01 Jan 2099 00:00:01 GMT").delete);
        // Max-Age beats Expires.
        assert!(!sc("SIDCC=x; Expires=Thu, 01 Jan 1970 00:00:01 GMT; Max-Age=60").delete);
        assert!(sc("SIDCC=x; Max-Age=0; Expires=Thu, 01 Jan 2099 00:00:01 GMT").delete);
        // An unparsable date keeps the cookie (a session cookie).
        assert!(!sc("SIDCC=x; Expires=someday").delete);
    }

    #[test]
    fn http_dates() {
        assert_eq!(parse_http_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(
            parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(784_111_777)
        );
        assert_eq!(
            parse_http_date("Sunday, 06-Nov-94 08:49:37 GMT"),
            Some(784_111_777)
        );
        assert_eq!(
            parse_http_date("Wed, 15 Jan 2025 12:00:00 GMT"),
            Some(1_736_942_400)
        );
        assert_eq!(
            parse_http_date("Thu, 29 Feb 2024 00:00:00 GMT"),
            Some(1_709_164_800)
        );
        assert_eq!(parse_http_date("garbage"), None);
        assert_eq!(parse_http_date("Thu, 01 Foo 2024 00:00:00 GMT"), None);
    }

    #[test]
    fn merge_replaces_deletes_and_appends() {
        let cookie = "SID=s; __Secure-1PSIDTS=old1; SAPISID=a; SIDCC=cc";
        let merged = merge(
            cookie,
            &[
                sc("__Secure-1PSIDTS=new1; Domain=.youtube.com; Path=/"),
                sc("SIDCC=; Max-Age=0"),
                sc("__Secure-3PSIDCC=n3; Domain=.youtube.com"),
            ],
        )
        .unwrap();
        assert_eq!(
            merged,
            "SID=s; __Secure-1PSIDTS=new1; SAPISID=a; __Secure-3PSIDCC=n3"
        );
        // The same values again: nothing changes.
        assert_eq!(merge(&merged, &[sc("SID=s")]), None);
        // Deleting something absent is no change either.
        assert_eq!(merge(&merged, &[sc("NOPE=; Max-Age=0")]), None);
        // Values containing '=' survive.
        let m = merge("LOGIN_INFO=a=b=c; X=1", &[sc("X=2")]).unwrap();
        assert_eq!(m, "LOGIN_INFO=a=b=c; X=2");
    }

    #[test]
    fn merge_has_a_size_cap() {
        let big = "x".repeat(MAX_HEADER_BYTES);
        assert_eq!(merge("A=1", &[sc(&format!("B={big}"))]), None);
        // Replacing in place is still allowed.
        assert_eq!(merge("A=1", &[sc("A=2")]).as_deref(), Some("A=2"));
    }

    #[test]
    fn only_trusted_hosts_and_youtube_cookies() {
        assert_eq!(
            trusted_host("https://music.youtube.com", false).as_deref(),
            Some("music.youtube.com")
        );
        assert!(trusted_host("https://www.youtube.com/youtubei/v1/player", false).is_some());
        assert!(trusted_host("https://accounts.google.com", false).is_some());
        assert!(
            trusted_host("http://music.youtube.com", false).is_none(),
            "https only"
        );
        assert!(trusted_host("https://youtube.com.evil.example", false).is_none());
        assert!(trusted_host("https://evilyoutube.com", false).is_none());
        assert!(trusted_host("https://user@evil.example", false).is_none());
        assert!(trusted_host("http://127.0.0.1:9", false).is_none());
        assert_eq!(
            trusted_host("http://127.0.0.1:9", true).as_deref(),
            Some("music.youtube.com")
        );
        assert!(trusted_host("http://10.0.0.1:9", true).is_none());

        let host = "music.youtube.com";
        assert!(applies(&sc("A=1; Domain=.youtube.com"), host));
        assert!(applies(&sc("A=1"), host), "host-only");
        assert!(applies(&sc("A=1; Domain=music.youtube.com; Path=/"), host));
        assert!(!applies(&sc("A=1; Domain=.google.com"), host), "foreign");
        assert!(
            !applies(&sc("A=1; Domain=www.youtube.com"), host),
            "not ours to set"
        );
        assert!(!applies(&sc("A=1; Domain=com"), host));
        assert!(!applies(&sc("A=1; Path=/youtubei"), host), "path-scoped");
        // A Google host's own cookies are not youtube.com cookies.
        assert!(!applies(&sc("A=1"), "accounts.google.com"));
        assert!(!applies(
            &sc("A=1; Domain=.google.com"),
            "accounts.google.com"
        ));

        let lines = [
            "__Secure-3PSIDTS=n; Domain=.youtube.com; Path=/; Secure",
            "NID=x; Domain=.google.com",
            "broken",
        ];
        let got = applicable("https://music.youtube.com", false, lines, NOW);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "__Secure-3PSIDTS");
        assert!(applicable("https://evil.example", false, lines, NOW).is_empty());
    }
}
