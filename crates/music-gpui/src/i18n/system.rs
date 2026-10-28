//! The system's preferred UI languages → the best embedded locale.
//!
//! `sys-locale` (already in GPUI's dependency graph) reads macOS' preferred languages
//! (`NSLocale.preferredLanguages`), Windows' user UI languages
//! (`GetUserPreferredUILanguages`) and on Linux `LANGUAGE` / `LC_ALL` /
//! `LC_MESSAGES` / `LANG`.

use super::Locale;

/// The first preferred system language we have a catalog for (English otherwise).
pub fn system_locale() -> Locale {
    if let Some(forced) = std::env::var("MUSIC_SYSTEM_LANG")
        .ok()
        .filter(|v| !v.is_empty())
    {
        // Tests / screenshots: pretend the system prefers these (comma-separated).
        return best_match(forced.split(',').map(str::to_owned));
    }
    best_match(sys_locale::get_locales())
}

/// The best catalog for a list of BCP 47 / POSIX tags in preference order.
pub fn best_match(tags: impl IntoIterator<Item = String>) -> Locale {
    tags.into_iter()
        .find_map(|tag| match_tag(&tag))
        .unwrap_or(Locale::ENGLISH)
}

/// `de-AT` → de, `zh-Hant-HK` → zh-TW, `pt-PT` → pt-BR, `en_US.UTF-8` → en.
pub fn match_tag(tag: &str) -> Option<Locale> {
    let tag = tag
        .split(['.', '@'])
        .next()
        .unwrap_or(tag)
        .replace('_', "-");
    if tag.is_empty() || tag == "C" || tag == "POSIX" {
        return None;
    }
    if let Some(exact) = Locale::from_code(&tag) {
        return Some(exact);
    }
    let lower = tag.to_ascii_lowercase();
    let mut parts = lower.split('-');
    let language = parts.next()?;
    let rest: Vec<&str> = parts.collect();
    let code = match language {
        "zh" => {
            let traditional = rest
                .iter()
                .any(|p| matches!(*p, "hant" | "tw" | "hk" | "mo"));
            if traditional { "zh-TW" } else { "zh-CN" }
        }
        "pt" => "pt-BR",
        // Old ISO codes some systems still report.
        "iw" => "he",
        "in" => "id",
        "nb" | "nn" => "no",
        other => other,
    };
    Locale::from_code(code).or_else(|| {
        // A regional catalog for the bare language (`es-419` for `es`), if that's all
        // there is.
        Locale::all().find(|l| l.language().eq_ignore_ascii_case(code))
    })
}

/// The YouTube Music content language (`hl`) that fits a UI locale.
pub fn content_language(locale: Locale) -> &'static str {
    match locale.code() {
        "zh-CN" | "zh-TW" | "pt-BR" => locale.code(),
        _ => locale.language(),
    }
}

/// A country (`gl`) from the first system tag with a region, if any.
pub fn system_region() -> Option<String> {
    let tags: Vec<String> = match std::env::var("MUSIC_SYSTEM_LANG") {
        Ok(v) if !v.is_empty() => v.split(',').map(str::to_owned).collect(),
        _ => sys_locale::get_locales().collect(),
    };
    tags.iter().find_map(|tag| {
        let tag = tag.split(['.', '@']).next()?.replace('_', "-");
        tag.split('-')
            .skip(1)
            .find(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_alphabetic()))
            .map(|p| p.to_ascii_uppercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_map_to_catalogs() {
        let code = |tag: &str| match_tag(tag).map(Locale::code);
        assert_eq!(code("en-US"), Some("en"));
        assert_eq!(code("en_GB.UTF-8"), Some("en"));
        assert_eq!(code("C"), None);
        assert_eq!(code("xx-YY"), None);
        assert_eq!(
            best_match(["xx".to_owned(), "en-AU".to_owned()]),
            Locale::ENGLISH
        );
        for locale in Locale::all() {
            assert_eq!(match_tag(locale.code()), Some(locale));
        }
        if Locale::from_code("zh-TW").is_some() {
            assert_eq!(code("zh-Hant-HK"), Some("zh-TW"));
            assert_eq!(code("zh-Hans-CN"), Some("zh-CN"));
            assert_eq!(code("zh-HK"), Some("zh-TW"));
        }
        if Locale::from_code("pt-BR").is_some() {
            assert_eq!(code("pt-PT"), Some("pt-BR"));
        }
        if Locale::from_code("de").is_some() {
            assert_eq!(code("de-AT"), Some("de"));
            assert_eq!(code("de_CH.UTF-8"), Some("de"));
        }
    }
}
