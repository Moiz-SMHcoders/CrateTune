//! Devanagari (Hindi) → casual Latin romanization as used for Bollywood lyrics
//! (aa/ee/oo for long vowels), with word-final schwa deletion (दिल → dil).

#[rustfmt::skip]
fn consonant(c: char) -> Option<&'static str> {
    Some(match c {
        'क' => "k", 'ख' => "kh", 'ग' => "g", 'घ' => "gh", 'ङ' => "ng",
        'च' => "ch", 'छ' => "chh", 'ज' => "j", 'झ' => "jh", 'ञ' => "ny",
        'ट' => "t", 'ठ' => "th", 'ड' => "d", 'ढ' => "dh", 'ण' => "n",
        'त' => "t", 'थ' => "th", 'द' => "d", 'ध' => "dh", 'न' | 'ऩ' => "n",
        'प' => "p", 'फ' => "ph", 'ब' => "b", 'भ' => "bh", 'म' => "m",
        'य' => "y", 'र' | 'ऱ' => "r", 'ल' | 'ळ' => "l", 'ऴ' => "zh", 'व' => "v",
        'श' | 'ष' => "sh", 'स' => "s", 'ह' => "h",
        // Precomposed nukta letters.
        '\u{0958}' => "q", '\u{0959}' => "kh", '\u{095a}' => "gh", '\u{095b}' => "z",
        '\u{095c}' => "r", '\u{095d}' => "rh", '\u{095e}' => "f", '\u{095f}' => "y",
        _ => return None,
    })
}

fn with_nukta(c: char) -> Option<&'static str> {
    Some(match c {
        'क' => "q",
        'ख' => "kh",
        'ग' => "gh",
        'ज' => "z",
        'ड' => "r",
        'ढ' => "rh",
        'फ' => "f",
        _ => return None,
    })
}

#[rustfmt::skip]
fn vowel(c: char) -> Option<&'static str> {
    Some(match c {
        'अ' => "a", 'आ' => "aa", 'इ' => "i", 'ई' => "ee", 'उ' => "u", 'ऊ' => "oo",
        'ऋ' => "ri", 'ॠ' => "ree", 'ऌ' => "lri", 'ऍ' | 'ए' => "e", 'ऐ' => "ai",
        'ऑ' | 'ओ' => "o", 'औ' => "au",
        _ => return None,
    })
}

#[rustfmt::skip]
fn matra(c: char) -> Option<&'static str> {
    Some(match c {
        'ा' => "aa", 'ि' => "i", 'ी' => "ee", 'ु' => "u", 'ू' => "oo",
        'ृ' => "ri", 'ॄ' => "ree", 'ॅ' | 'े' => "e", 'ै' => "ai",
        'ॉ' | 'ो' => "o", 'ौ' => "au",
        _ => return None,
    })
}

const VIRAMA: char = '्';
const NUKTA: char = '़';
const ANUSVARA: char = 'ं';
const CHANDRABINDU: char = 'ँ';
const VISARGA: char = 'ः';

pub(crate) fn is_devanagari(c: char) -> bool {
    matches!(c, '\u{0900}'..='\u{097f}' | '\u{a8e0}'..='\u{a8ff}')
}

fn is_letter_or_sign(c: char) -> bool {
    consonant(c).is_some() || vowel(c).is_some() || matches!(c, ANUSVARA | CHANDRABINDU | VISARGA)
}

fn is_labial(c: Option<&char>) -> bool {
    matches!(c, Some('प' | 'फ' | 'ब' | 'भ' | 'म'))
}

pub(crate) fn romanize_devanagari(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut aksharas = 0usize; // in the current word
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(base) = consonant(c) {
            let mut j = i + 1;
            let mut base = base;
            if chars.get(j) == Some(&NUKTA) {
                base = with_nukta(c).unwrap_or(base);
                j += 1;
            }
            out.push_str(base);
            aksharas += 1;
            match chars.get(j) {
                Some(&VIRAMA) => j += 1,
                Some(&m) if matra(m).is_some() => {
                    out.push_str(matra(m).unwrap_or_default());
                    j += 1;
                }
                next => {
                    // Inherent "a", dropped at the end of a multi-syllable word.
                    let continues = next.is_some_and(|&n| is_letter_or_sign(n));
                    if continues || aksharas == 1 {
                        out.push('a');
                    }
                }
            }
            i = j;
            continue;
        }
        if let Some(v) = vowel(c) {
            out.push_str(v);
            aksharas += 1;
        } else if let Some(m) = matra(c) {
            out.push_str(m); // stray matra
        } else {
            match c {
                ANUSVARA => out.push(if is_labial(chars.get(i + 1)) {
                    'm'
                } else {
                    'n'
                }),
                CHANDRABINDU => out.push('n'),
                VISARGA => out.push('h'),
                VIRAMA | NUKTA => {}
                'ॐ' => out.push_str("om"),
                '।' | '॥' => out.push('.'),
                'ऽ' => out.push('\''),
                '०'..='९' => out.push(char::from(b'0' + (c as u32 - '०' as u32) as u8)),
                _ => {
                    out.push(c);
                    aksharas = 0;
                }
            }
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hindi() {
        assert_eq!(romanize_devanagari("दिल"), "dil");
        assert_eq!(romanize_devanagari("मेरा दिल"), "meraa dil");
        assert_eq!(romanize_devanagari("प्यार"), "pyaar");
        assert_eq!(romanize_devanagari("कमल"), "kamal");
        assert_eq!(romanize_devanagari("हैं"), "hain");
        assert_eq!(romanize_devanagari("तुम्हें"), "tumhen");
        assert_eq!(romanize_devanagari("ज़िंदगी"), "zindagee");
        assert_eq!(romanize_devanagari("न"), "na");
        assert_eq!(romanize_devanagari("संबंध, १२"), "sambandh, 12");
    }
}
