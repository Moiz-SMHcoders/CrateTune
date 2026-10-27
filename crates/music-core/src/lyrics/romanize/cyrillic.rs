//! Cyrillic → Latin (BGN/PCGN-style, covering Russian, Ukrainian, Belarusian,
//! Serbian, Macedonian, Bulgarian and Kazakh basics).

fn lower(c: char) -> Option<&'static str> {
    Some(match c {
        'а' => "a",
        'б' => "b",
        'в' => "v",
        'г' => "g",
        'д' => "d",
        'е' => "e",
        'ё' => "yo",
        'ж' => "zh",
        'з' => "z",
        'и' => "i",
        'й' => "y",
        'к' => "k",
        'л' => "l",
        'м' => "m",
        'н' => "n",
        'о' => "o",
        'п' => "p",
        'р' => "r",
        'с' => "s",
        'т' => "t",
        'у' => "u",
        'ф' => "f",
        'х' => "kh",
        'ц' => "ts",
        'ч' => "ch",
        'ш' => "sh",
        'щ' => "shch",
        'ъ' | 'ь' => "",
        'ы' => "y",
        'э' => "e",
        'ю' => "yu",
        'я' => "ya",
        'і' => "i",
        'ї' => "yi",
        'є' => "ye",
        'ґ' => "g",
        'ў' => "w",
        'ђ' => "dj",
        'ј' => "j",
        'љ' => "lj",
        'њ' => "nj",
        'ћ' => "c",
        'џ' => "dz",
        'ѓ' => "gj",
        'ќ' => "kj",
        'ѕ' => "dz",
        'ә' => "a",
        'ғ' => "gh",
        'қ' => "q",
        'ң' => "ng",
        'ө' => "o",
        'ұ' | 'ү' => "u",
        'һ' => "h",
        _ => return None,
    })
}

pub(crate) fn is_cyrillic(c: char) -> bool {
    matches!(c, '\u{0400}'..='\u{052f}')
}

/// Ukrainian spelling where it differs from Russian (и → y, г → h, ї → i inside words).
fn ukrainian(c: char, word_start: bool) -> Option<&'static str> {
    Some(match c {
        'и' => "y",
        'г' => "h",
        'ї' if !word_start => "i",
        _ => return None,
    })
}

pub(crate) fn romanize_cyrillic(text: &str) -> String {
    let is_ukrainian = text.contains(['і', 'ї', 'є', 'ґ', 'І', 'Ї', 'Є', 'Ґ']);
    let mut out = String::with_capacity(text.len());
    let mut word_start = true;
    for c in text.chars() {
        let lc = c.to_lowercase().next().unwrap_or(c);
        let mapped = if is_ukrainian {
            ukrainian(lc, word_start).or_else(|| lower(lc))
        } else {
            lower(lc)
        };
        word_start = !c.is_alphabetic();
        match mapped {
            Some(r) if lc != c => {
                // Capitalize the first letter of the transliteration.
                let mut it = r.chars();
                if let Some(first) = it.next() {
                    out.extend(first.to_uppercase());
                    out.push_str(it.as_str());
                }
            }
            Some(r) => out.push_str(r),
            None => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn russian_and_ukrainian() {
        assert_eq!(romanize_cyrillic("Привет, мир"), "Privet, mir");
        assert_eq!(romanize_cyrillic("Щастя"), "Shchastya");
        assert_eq!(romanize_cyrillic("Київ"), "Kyiv");
        assert_eq!(romanize_cyrillic("объём"), "obyom");
    }
}
