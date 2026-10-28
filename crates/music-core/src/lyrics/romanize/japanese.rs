// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/lyrics/LyricsUtils.kt (KANA_ROMAJI_MAP, katakanaToRomaji, romanizeJapanese) — © Rukamori and ArchiveTune contributors, GPL-3.0
//! Japanese → Hepburn-style romaji. Kana is converted with a table (sokuon,
//! yōon, long vowels). Kanji need a dictionary: text is tokenized with lindera +
//! IPADIC (embedded with the `japanese-dict` feature, or downloaded at runtime, see
//! [`super::dictionary`]) and the readings are romanized; without one kanji are
//! left as they are.

/// Two-kana combinations first, then single kana (katakana; hiragana is mapped up).
#[rustfmt::skip]
const DIGRAPHS: &[(&str, &str)] = &[
    ("キャ", "kya"), ("キュ", "kyu"), ("キョ", "kyo"),
    ("シャ", "sha"), ("シュ", "shu"), ("ショ", "sho"), ("シェ", "she"),
    ("チャ", "cha"), ("チュ", "chu"), ("チョ", "cho"), ("チェ", "che"),
    ("ニャ", "nya"), ("ニュ", "nyu"), ("ニョ", "nyo"),
    ("ヒャ", "hya"), ("ヒュ", "hyu"), ("ヒョ", "hyo"),
    ("ミャ", "mya"), ("ミュ", "myu"), ("ミョ", "myo"),
    ("リャ", "rya"), ("リュ", "ryu"), ("リョ", "ryo"),
    ("ギャ", "gya"), ("ギュ", "gyu"), ("ギョ", "gyo"),
    ("ジャ", "ja"), ("ジュ", "ju"), ("ジョ", "jo"), ("ジェ", "je"),
    ("ヂャ", "ja"), ("ヂュ", "ju"), ("ヂョ", "jo"),
    ("ビャ", "bya"), ("ビュ", "byu"), ("ビョ", "byo"),
    ("ピャ", "pya"), ("ピュ", "pyu"), ("ピョ", "pyo"),
    ("ファ", "fa"), ("フィ", "fi"), ("フェ", "fe"), ("フォ", "fo"),
    ("ティ", "ti"), ("ディ", "di"), ("トゥ", "tu"), ("ドゥ", "du"),
    ("ウィ", "wi"), ("ウェ", "we"), ("ウォ", "wo"),
    ("ヴァ", "va"), ("ヴィ", "vi"), ("ヴェ", "ve"), ("ヴォ", "vo"),
    ("ツァ", "tsa"), ("ツォ", "tso"), ("デュ", "dyu"),
];

#[rustfmt::skip]
const SINGLE: &[(char, &str)] = &[
    ('ア', "a"), ('イ', "i"), ('ウ', "u"), ('エ', "e"), ('オ', "o"),
    ('カ', "ka"), ('キ', "ki"), ('ク', "ku"), ('ケ', "ke"), ('コ', "ko"),
    ('サ', "sa"), ('シ', "shi"), ('ス', "su"), ('セ', "se"), ('ソ', "so"),
    ('タ', "ta"), ('チ', "chi"), ('ツ', "tsu"), ('テ', "te"), ('ト', "to"),
    ('ナ', "na"), ('ニ', "ni"), ('ヌ', "nu"), ('ネ', "ne"), ('ノ', "no"),
    ('ハ', "ha"), ('ヒ', "hi"), ('フ', "fu"), ('ヘ', "he"), ('ホ', "ho"),
    ('マ', "ma"), ('ミ', "mi"), ('ム', "mu"), ('メ', "me"), ('モ', "mo"),
    ('ヤ', "ya"), ('ユ', "yu"), ('ヨ', "yo"),
    ('ラ', "ra"), ('リ', "ri"), ('ル', "ru"), ('レ', "re"), ('ロ', "ro"),
    ('ワ', "wa"), ('ヰ', "i"), ('ヱ', "e"), ('ヲ', "o"), ('ン', "n"),
    ('ガ', "ga"), ('ギ', "gi"), ('グ', "gu"), ('ゲ', "ge"), ('ゴ', "go"),
    ('ザ', "za"), ('ジ', "ji"), ('ズ', "zu"), ('ゼ', "ze"), ('ゾ', "zo"),
    ('ダ', "da"), ('ヂ', "ji"), ('ヅ', "zu"), ('デ', "de"), ('ド', "do"),
    ('バ', "ba"), ('ビ', "bi"), ('ブ', "bu"), ('ベ', "be"), ('ボ', "bo"),
    ('パ', "pa"), ('ピ', "pi"), ('プ', "pu"), ('ペ', "pe"), ('ポ', "po"),
    ('ヴ', "vu"),
    ('ァ', "a"), ('ィ', "i"), ('ゥ', "u"), ('ェ', "e"), ('ォ', "o"),
    ('ャ', "ya"), ('ュ', "yu"), ('ョ', "yo"), ('ヮ', "wa"), ('ヵ', "ka"), ('ヶ', "ke"),
];

pub(crate) fn is_kana(c: char) -> bool {
    matches!(c, '\u{3041}'..='\u{309f}' | '\u{30a0}'..='\u{30ff}' | '\u{31f0}'..='\u{31ff}')
}

pub(crate) fn is_han(c: char) -> bool {
    matches!(c,
        '\u{4e00}'..='\u{9fff}' | '\u{3400}'..='\u{4dbf}' | '\u{f900}'..='\u{faff}'
        | '\u{20000}'..='\u{2ebef}' | '々' | '〆' | '〇')
}

/// Hiragana → katakana (other characters unchanged).
fn to_katakana(c: char) -> char {
    match c {
        '\u{3041}'..='\u{3096}' | '\u{309d}' | '\u{309e}' => {
            char::from_u32(c as u32 + 0x60).unwrap_or(c)
        }
        _ => c,
    }
}

fn single(c: char) -> Option<&'static str> {
    SINGLE.iter().find(|(k, _)| *k == c).map(|(_, v)| *v)
}

fn digraph(a: char, b: char) -> Option<&'static str> {
    DIGRAPHS.iter().find_map(|(k, v)| {
        let mut it = k.chars();
        (it.next() == Some(a) && it.next() == Some(b)).then_some(*v)
    })
}

/// Romaji of the kana at the start of `chars` and how many chars it used.
fn kana_unit(chars: &[char]) -> Option<(&'static str, usize)> {
    let a = *chars.first()?;
    if let Some(&b) = chars.get(1)
        && let Some(r) = digraph(a, b)
    {
        return Some((r, 2));
    }
    single(a).map(|r| (r, 1))
}

/// Kana (hiragana or katakana) → romaji; other characters pass through.
/// `next` is the following token's reading, for a trailing っ/ッ.
pub(crate) fn kana_to_romaji(text: &str, next: Option<&str>) -> String {
    let chars: Vec<char> = text.chars().map(to_katakana).collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == 'ッ' {
            // Sokuon doubles the next consonant (っち → tchi).
            let following: Option<&str> = match kana_unit(&chars[i + 1..]) {
                Some((r, _)) => Some(r),
                None => next.and_then(|n| {
                    let n: Vec<char> = n.chars().map(to_katakana).collect();
                    kana_unit(&n).map(|(r, _)| r)
                }),
            };
            if let Some(r) = following {
                if r.starts_with("ch") {
                    out.push('t');
                } else if let Some(first) = r.chars().next().filter(|c| !"aiueo".contains(*c)) {
                    out.push(first);
                }
            }
            i += 1;
            continue;
        }
        if c == 'ー' {
            // Long vowel mark: repeat the previous vowel.
            if let Some(v) = out.chars().last().filter(|c| "aiueo".contains(*c)) {
                out.push(v);
            }
            i += 1;
            continue;
        }
        match kana_unit(&chars[i..]) {
            Some((r, used)) => {
                out.push_str(r);
                i += used;
            }
            None => {
                out.push(text.chars().nth(i).unwrap_or(c));
                i += 1;
            }
        }
    }
    out
}

/// Japanese line → romaji. Kanji are kept unless a dictionary is embedded
/// (`japanese-dict`) or was loaded at runtime ([`super::dictionary`]).
pub(crate) fn romanize_japanese(text: &str) -> String {
    if let Some(r) = super::dictionary::romanize(text) {
        return r;
    }
    romanize_by_script_runs(text)
}

/// Without a dictionary: romanize kana runs, keep kanji runs, separate them with spaces.
fn romanize_by_script_runs(text: &str) -> String {
    #[derive(PartialEq, Clone, Copy)]
    enum Run {
        Kana,
        Han,
        Other,
    }
    let class = |c: char| {
        if is_kana(c) {
            Run::Kana
        } else if is_han(c) {
            Run::Han
        } else {
            Run::Other
        }
    };
    let mut runs: Vec<(Run, String)> = Vec::new();
    for c in text.chars() {
        let k = class(c);
        match runs.last_mut() {
            Some((last, s)) if *last == k => s.push(c),
            _ => runs.push((k, c.to_string())),
        }
    }
    let mut out = String::with_capacity(text.len() * 2);
    for (i, (kind, s)) in runs.iter().enumerate() {
        let piece = match kind {
            Run::Kana => kana_to_romaji(s, None),
            _ => s.clone(),
        };
        let prev_kind = i.checked_sub(1).map(|p| runs[p].0);
        if matches!(prev_kind, Some(Run::Kana | Run::Han))
            && *kind != Run::Other
            && !out.ends_with(' ')
        {
            out.push(' ');
        }
        out.push_str(&piece);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kana() {
        assert_eq!(kana_to_romaji("ありがとう", None), "arigatou");
        assert_eq!(kana_to_romaji("ミステリアス", None), "misuteriasu");
        assert_eq!(kana_to_romaji("きって", None), "kitte");
        assert_eq!(kana_to_romaji("まっちゃ", None), "matcha");
        assert_eq!(kana_to_romaji("ラーメン", None), "raamen");
        assert_eq!(kana_to_romaji("しょうじょ", None), "shoujo");
        assert_eq!(kana_to_romaji("パーティー", None), "paatii");
        assert_eq!(kana_to_romaji("あっ", Some("た")), "at");
        assert_eq!(kana_to_romaji("ok!", None), "ok!");
    }

    #[test]
    fn line_without_dictionary() {
        assert_eq!(
            romanize_by_script_runs("知りたいその秘密ミステリアス"),
            "知 ritaisono 秘密 misuteriasu"
        );
        assert_eq!(romanize_by_script_runs("君の名は。"), "君 no 名 ha。");
    }

    #[cfg(feature = "japanese-dict")]
    #[test]
    fn line_with_dictionary() {
        let r = romanize_japanese("東京に住んでいます");
        assert!(r.starts_with("toukyou ni"), "{r}");
        assert_eq!(romanize_japanese("君の名は"), "kimi no na wa");
    }
}
