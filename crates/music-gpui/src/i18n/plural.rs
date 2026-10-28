//! CLDR plural rules (cardinal, integer counts) for the languages the catalogs use,
//! from the CLDR 46 `plurals.xml`. Counts in the UI are whole numbers, so only the
//! integer branches (`v = 0`) are implemented; fractional forms fall back to `other`.

/// A CLDR plural category; the discriminant is the slot in a catalog's plural table
/// (same order as `CATEGORIES` in `i18n_parse.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Zero = 0,
    One = 1,
    Two = 2,
    Few = 3,
    Many = 4,
    Other = 5,
}

/// The category of the whole number `n` in `language` (a plain subtag: `ru`, `pt`).
pub fn category(language: &str, n: u64) -> Category {
    use Category::*;
    let (i10, i100) = (n % 10, n % 100);
    let million = n != 0 && n % 1_000_000 == 0;
    match language {
        // one: i = 1 and v = 0 (+ "many" for exact millions in es / it / pt / fr).
        "en" | "de" | "nl" | "sv" | "da" | "nb" | "no" | "fi" | "et" | "el" | "bg" | "hu"
        | "tr" => {
            if n == 1 {
                One
            } else {
                Other
            }
        }
        "es" | "it" => match n {
            1 => One,
            _ if million => Many,
            _ => Other,
        },
        // one: i = 0,1 (French, Portuguese as spoken in Brazil — CLDR "pt").
        "fr" | "pt" => match n {
            0 | 1 => One,
            _ if million => Many,
            _ => Other,
        },
        // one: i = 0 or n = 1.
        "hi" | "fa" | "bn" | "am" => {
            if n <= 1 {
                One
            } else {
                Other
            }
        }
        "ru" | "uk" | "be" => {
            if i10 == 1 && i100 != 11 {
                One
            } else if (2..=4).contains(&i10) && !(12..=14).contains(&i100) {
                Few
            } else {
                Many
            }
        }
        "pl" => {
            if n == 1 {
                One
            } else if (2..=4).contains(&i10) && !(12..=14).contains(&i100) {
                Few
            } else {
                Many
            }
        }
        "cs" | "sk" => match n {
            1 => One,
            2..=4 => Few,
            _ => Other,
        },
        "ar" => match (n, i100) {
            (0, _) => Zero,
            (1, _) => One,
            (2, _) => Two,
            (_, 3..=10) => Few,
            (_, 11..=99) => Many,
            _ => Other,
        },
        "he" => match n {
            1 => One,
            2 => Two,
            _ => Other,
        },
        // No plural distinction: ja, ko, zh, id, vi, th, ms, …
        _ => Other,
    }
}

#[cfg(test)]
mod tests {
    use super::Category::*;
    use super::category;

    #[test]
    fn cldr_examples() {
        assert_eq!(category("en", 1), One);
        assert_eq!(category("en", 0), Other);
        assert_eq!(category("fr", 0), One);
        assert_eq!(category("fr", 2), Other);
        assert_eq!(category("pt", 1_000_000), Many);
        for (n, c) in [
            (1, One),
            (2, Few),
            (5, Many),
            (11, Many),
            (21, One),
            (22, Few),
            (25, Many),
            (111, Many),
            (112, Many),
            (101, One),
        ] {
            assert_eq!(category("ru", n), c, "ru {n}");
            assert_eq!(category("uk", n), c, "uk {n}");
        }
        for (n, c) in [
            (1, One),
            (2, Few),
            (5, Many),
            (12, Many),
            (21, Many),
            (22, Few),
            (0, Many),
        ] {
            assert_eq!(category("pl", n), c, "pl {n}");
        }
        for (n, c) in [
            (0, Zero),
            (1, One),
            (2, Two),
            (3, Few),
            (10, Few),
            (11, Many),
            (99, Many),
            (100, Other),
            (103, Few),
        ] {
            assert_eq!(category("ar", n), c, "ar {n}");
        }
        assert_eq!(category("hi", 0), One);
        assert_eq!(category("ja", 1), Other);
        assert_eq!(category("zh", 1), Other);
    }
}
