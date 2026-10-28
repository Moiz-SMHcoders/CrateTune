//! Locale-aware formatting: digit grouping, dates, relative times, listening time and
//! file sizes. Playback times stay `m:ss` everywhere (they are not formatted here).
//!
//! Separators and grouping follow CLDR's `latn` number symbols per language (Hindi
//! uses the Indian 3-2-2 grouping; Spanish and Polish group only from five digits).
//! Words — month names, "min", "ago" — come from the catalogs (`common.lang`).

use super::{locale, text};

struct NumberStyle {
    group: &'static str,
    decimal: char,
    /// CLDR `minimumGroupingDigits`: 2 = no separator in 4-digit numbers.
    min_grouping: usize,
    indian: bool,
}

fn style(language: &str) -> NumberStyle {
    let (group, decimal, min_grouping, indian) = match language {
        "de" | "es" | "it" | "nl" | "pt" | "tr" | "id" | "vi" | "da" | "el" | "ro" | "hr" => {
            (".", ',', if language == "es" { 2 } else { 1 }, false)
        }
        // Narrow no-break space (U+202F) in French, no-break space elsewhere.
        "fr" => ("\u{202f}", ',', 1, false),
        "ru" | "uk" | "pl" | "cs" | "sk" | "sv" | "fi" | "nb" | "bg" | "hu" | "et" => {
            ("\u{a0}", ',', if language == "pl" { 2 } else { 1 }, false)
        }
        "hi" => (",", '.', 1, true),
        _ => (",", '.', 1, false),
    };
    NumberStyle {
        group,
        decimal,
        min_grouping,
        indian,
    }
}

fn group_digits(digits: &str, style: &NumberStyle) -> String {
    if digits.len() < 4 || digits.len() < 3 + style.min_grouping {
        return digits.to_owned();
    }
    // Group sizes from the right: 3, then 3 (or 2 for Indian grouping).
    let mut cuts = Vec::new();
    let mut end = digits.len();
    let mut size = 3;
    while end > size {
        end -= size;
        cuts.push(end);
        if style.indian {
            size = 2;
        }
    }
    let mut out = String::with_capacity(digits.len() + cuts.len() * 3);
    let mut start = 0;
    for &cut in cuts.iter().rev() {
        out.push_str(&digits[start..cut]);
        out.push_str(style.group);
        start = cut;
    }
    out.push_str(&digits[start..]);
    out
}

/// `12345` → "12,345" / "12.345" / "12 345" (current locale).
pub fn number(n: u64) -> String {
    group_digits(&n.to_string(), &style(locale().language()))
}

/// `value` with `decimals` fraction digits and the locale's decimal separator.
pub fn decimal(value: f64, decimals: usize) -> String {
    let style = style(locale().language());
    let formatted = format!("{value:.decimals$}");
    let (int, frac) = match formatted.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (formatted.as_str(), None),
    };
    let (sign, digits) = match int.strip_prefix('-') {
        Some(d) => ("-", d),
        None => ("", int),
    };
    let mut out = format!("{sign}{}", group_digits(digits, &style));
    if let Some(frac) = frac {
        out.push(style.decimal);
        out.push_str(frac);
    }
    out
}

const MONTHS: [super::Key; 12] = [
    k!("date.month.1"),
    k!("date.month.2"),
    k!("date.month.3"),
    k!("date.month.4"),
    k!("date.month.5"),
    k!("date.month.6"),
    k!("date.month.7"),
    k!("date.month.8"),
    k!("date.month.9"),
    k!("date.month.10"),
    k!("date.month.11"),
    k!("date.month.12"),
];
const MONTHS_IN_DATE: [super::Key; 12] = [
    k!("date.month_in_date.1"),
    k!("date.month_in_date.2"),
    k!("date.month_in_date.3"),
    k!("date.month_in_date.4"),
    k!("date.month_in_date.5"),
    k!("date.month_in_date.6"),
    k!("date.month_in_date.7"),
    k!("date.month_in_date.8"),
    k!("date.month_in_date.9"),
    k!("date.month_in_date.10"),
    k!("date.month_in_date.11"),
    k!("date.month_in_date.12"),
];
const MONTHS_SHORT: [super::Key; 12] = [
    k!("date.month_short.1"),
    k!("date.month_short.2"),
    k!("date.month_short.3"),
    k!("date.month_short.4"),
    k!("date.month_short.5"),
    k!("date.month_short.6"),
    k!("date.month_short.7"),
    k!("date.month_short.8"),
    k!("date.month_short.9"),
    k!("date.month_short.10"),
    k!("date.month_short.11"),
    k!("date.month_short.12"),
];
/// Monday first (ISO), as the library's weekday statistics.
const WEEKDAYS_SHORT: [super::Key; 7] = [
    k!("date.weekday_short.1"),
    k!("date.weekday_short.2"),
    k!("date.weekday_short.3"),
    k!("date.weekday_short.4"),
    k!("date.weekday_short.5"),
    k!("date.weekday_short.6"),
    k!("date.weekday_short.7"),
];

/// Standalone month name, `month` 1–12 ("October", "Oktober", "октябрь").
pub fn month_name(month: u32) -> &'static str {
    text(MONTHS[(month.clamp(1, 12) - 1) as usize])
}

/// Abbreviated month name for chart labels ("Oct").
pub fn month_short(month: u32) -> &'static str {
    text(MONTHS_SHORT[(month.clamp(1, 12) - 1) as usize])
}

/// Abbreviated weekday, `weekday` 1 = Monday … 7 = Sunday.
pub fn weekday_short(weekday: u32) -> &'static str {
    text(WEEKDAYS_SHORT[(weekday.clamp(1, 7) - 1) as usize])
}

/// A calendar date in the locale's long form ("28 October 2026", "28. Oktober 2026",
/// "2026年10月28日").
pub fn date(year: i64, month: u32, day: u32) -> String {
    let month = month.clamp(1, 12);
    t!(
        "date.full",
        day = day,
        month = text(MONTHS_IN_DATE[(month - 1) as usize]),
        month_num = month,
        year = year
    )
}

/// `2026-10-28` → [`date`]; anything else is returned as it is.
pub fn iso_date(day: &str) -> String {
    let mut parts = day.splitn(3, '-');
    let (Some(y), Some(m), Some(d)) = (parts.next(), parts.next(), parts.next()) else {
        return day.to_owned();
    };
    match (y.parse::<i64>(), m.parse::<u32>(), d.parse::<u32>()) {
        (Ok(y), Ok(m @ 1..=12), Ok(d @ 1..=31)) => date(y, m, d),
        _ => day.to_owned(),
    }
}

/// "just now", "5 min ago", "3 h ago", "2 days ago".
pub fn ago(secs: u64) -> String {
    match secs {
        0..=9 => t!("time.just_now").to_owned(),
        10..=59 => tp!("time.seconds_ago", secs),
        60..=3599 => tp!("time.minutes_ago", secs / 60),
        3600..=86_399 => tp!("time.hours_ago", secs / 3600),
        _ => tp!("time.days_ago", secs / 86_400),
    }
}

/// Listening time: "3 h 12 min", "45 min", "20 s".
pub fn listening(ms: u64) -> String {
    let secs = ms / 1000;
    let (h, m) = (secs / 3600, (secs / 60) % 60);
    if h > 0 {
        t!("time.hours_minutes", hours = number(h), minutes = m)
    } else if m > 0 {
        t!("time.minutes", minutes = m)
    } else {
        t!("time.seconds", seconds = secs)
    }
}

/// `1,5 GB`, `640 KB` (binary multiples, as before; unit symbols are not translated).
pub fn bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024. && unit + 1 < UNITS.len() {
        value /= 1024.;
        unit += 1;
    }
    let digits = if unit == 0 || value >= 100. { 0 } else { 1 };
    let unit = text(UNITS_KEYS[unit]);
    t!("units.size", value = decimal(value, digits), unit = unit)
}

const UNITS_KEYS: [super::Key; 5] = [
    k!("units.b"),
    k!("units.kb"),
    k!("units.mb"),
    k!("units.gb"),
    k!("units.tb"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouping() {
        let en = style("en");
        assert_eq!(group_digits("1234567", &en), "1,234,567");
        assert_eq!(group_digits("123", &en), "123");
        assert_eq!(group_digits("1234", &style("es")), "1234");
        assert_eq!(group_digits("12345", &style("es")), "12.345");
        assert_eq!(group_digits("1234567", &style("hi")), "12,34,567");
        assert_eq!(group_digits("1234", &style("de")), "1.234");
        assert_eq!(group_digits("1234", &style("pl")), "1234");
        assert_eq!(group_digits("12345", &style("ru")), "12\u{a0}345");
    }
}
