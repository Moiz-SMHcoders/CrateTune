//! Small helpers shared across views.

use gpui::ImageFormat;

/// Runs blocking work on a dedicated thread. GPUI's background executor has about
/// one thread per core, so a burst of blocking network calls would otherwise
/// queue unrelated work (e.g. the lyrics request) behind them (seen: >15 s).
pub async fn off_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = futures_channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    rx.await.expect("worker thread panicked")
}

/// YouTube serves JPEG for `=w..-h..` URLs but may switch to WebP/PNG.
pub fn sniff_format(bytes: &[u8]) -> ImageFormat {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => ImageFormat::Png,
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => ImageFormat::Webp,
        [b'G', b'I', b'F', ..] => ImageFormat::Gif,
        _ => ImageFormat::Jpeg,
    }
}

/// Local calendar date `YYYY-MM-DD` of a Unix-ms instant (same days the library's
/// `TimeZone::Local` grouping uses).
pub fn local_date(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = (secs + local_offset_secs(secs)).div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Local year of a Unix-ms instant.
pub fn local_year(ms: i64) -> i32 {
    let secs = ms.div_euclid(1000);
    civil_from_days((secs + local_offset_secs(secs)).div_euclid(86_400)).0 as i32
}

/// Seconds east of UTC at `secs` (Unix time), from the C library's `localtime_r`.
#[cfg(unix)]
fn local_offset_secs(secs: i64) -> i64 {
    // `struct tm` as on macOS and glibc (both have `tm_gmtoff` / `tm_zone`).
    #[repr(C)]
    struct Tm {
        tm_sec: i32,
        tm_min: i32,
        tm_hour: i32,
        tm_mday: i32,
        tm_mon: i32,
        tm_year: i32,
        tm_wday: i32,
        tm_yday: i32,
        tm_isdst: i32,
        tm_gmtoff: std::ffi::c_long,
        tm_zone: *const std::ffi::c_char,
    }
    unsafe extern "C" {
        fn localtime_r(time: *const i64, result: *mut Tm) -> *mut Tm;
    }
    let mut tm = std::mem::MaybeUninit::<Tm>::zeroed();
    // SAFETY: `localtime_r` is thread-safe, reads `secs` and fills the `struct tm`
    // behind the pointer; it returns null on failure.
    let filled = unsafe { !localtime_r(&secs, tm.as_mut_ptr()).is_null() };
    if filled {
        // SAFETY: zero-initialised and filled by the successful call.
        // `c_long` is `i64` on 64-bit targets, narrower elsewhere.
        #[allow(clippy::useless_conversion)]
        i64::from(unsafe { tm.assume_init() }.tm_gmtoff)
    } else {
        0
    }
}

/// Seconds east of UTC at `secs` (Unix time), from the C runtime's `_localtime64_s`
/// (the same time-zone rules SQLite's `localtime` uses there).
#[cfg(windows)]
fn local_offset_secs(secs: i64) -> i64 {
    // `struct tm` of the Microsoft C runtime (no `tm_gmtoff`).
    #[repr(C)]
    struct Tm {
        tm_sec: i32,
        tm_min: i32,
        tm_hour: i32,
        tm_mday: i32,
        tm_mon: i32,
        tm_year: i32,
        tm_wday: i32,
        tm_yday: i32,
        tm_isdst: i32,
    }
    unsafe extern "C" {
        fn _localtime64_s(result: *mut Tm, time: *const i64) -> i32;
    }
    let mut tm = std::mem::MaybeUninit::<Tm>::zeroed();
    // SAFETY: `_localtime64_s` reads `secs` and fills the `struct tm` behind the
    // pointer; it returns 0 on success (EINVAL e.g. before 1970).
    if unsafe { _localtime64_s(tm.as_mut_ptr(), &secs) } != 0 {
        return 0;
    }
    // SAFETY: zero-initialised and filled by the successful call.
    let tm = unsafe { tm.assume_init() };
    let days = days_from_civil(
        i64::from(tm.tm_year) + 1900,
        (tm.tm_mon + 1) as u32,
        tm.tm_mday as u32,
    );
    let local = days * 86_400
        + i64::from(tm.tm_hour) * 3600
        + i64::from(tm.tm_min) * 60
        + i64::from(tm.tm_sec);
    local - secs
}

#[cfg(not(any(unix, windows)))]
fn local_offset_secs(_: i64) -> i64 {
    0
}

/// (year, month, day) → days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Days since 1970-01-01 → (year, month, day) (Howard Hinnant's `civil_from_days`).
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `2026-10-28` → `28 October 2026` (in the UI language).
pub fn pretty_date(day: &str) -> String {
    crate::i18n::format::iso_date(day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_image_formats() {
        assert_eq!(sniff_format(b"\x89PNG\r\n"), ImageFormat::Png);
        assert_eq!(sniff_format(b"RIFF1234WEBPVP8 "), ImageFormat::Webp);
        assert_eq!(sniff_format(b"GIF89a"), ImageFormat::Gif);
        assert_eq!(sniff_format(b"\xff\xd8\xff"), ImageFormat::Jpeg);
    }

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_389), (2025, 10, 28));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        for days in [-800_000, -1, 0, 59, 60, 20_389, 2_932_896] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(pretty_date("2026-10-28"), "28 October 2026");
        assert_eq!(pretty_date("bogus"), "bogus");
        assert_eq!(local_date(0).len(), 10);
        assert!((1969..=1970).contains(&local_year(0)));
    }
}
