// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/lyrics/LyricsUtils.kt (romanizeKorean, HANGUL_ROMAJA_MAP) — © Rukamori and ArchiveTune contributors, GPL-3.0
//! Hangul → Revised Romanization, algorithmic: syllables are decomposed into
//! jamo and a final consonant is romanized with the next initial in mind
//! (liaison and nasal/liquid assimilation, e.g. 한국어 → hangugeo, 신라 → silla).

#[rustfmt::skip]
const CHO: [&str; 19] = [
    "g", "kk", "n", "d", "tt", "r", "m", "b", "pp", "s", "ss", "", "j", "jj", "ch", "k", "t", "p",
    "h",
];
#[rustfmt::skip]
const JUNG: [&str; 21] = [
    "a", "ae", "ya", "yae", "eo", "e", "yeo", "ye", "o", "wa", "wae", "oe", "yo", "u", "wo", "we",
    "wi", "yu", "eu", "ui", "i",
];

/// Final consonant (index 1..=27) as `[alone, before ㅇ, before ㄴ, before ㄹ, before ㅁ, before ㅎ]`.
/// A context form spells the final *and* the following initial (신라 → si + ll + a);
/// an empty one means "final alone, then the initial as usual".
#[rustfmt::skip]
const JONG: [[&str; 6]; 28] = [
    ["", "", "", "", "", ""],
    ["k", "g", "ngn", "ngn", "ngm", "kh"],      // ㄱ
    ["kk", "kg", "ngn", "ngn", "ngm", "kh"],    // ㄲ
    ["k", "ks", "ngn", "ngn", "ngm", "kch"],    // ㄳ
    ["n", "", "", "ll", "", ""],                // ㄴ
    ["n", "nj", "nn", "nn", "nm", ""],          // ㄵ
    ["n", "nh", "", "nn", "", ""],              // ㄶ
    ["t", "d", "nn", "nn", "nm", "th"],         // ㄷ
    ["l", "r", "ll", "ll", "", ""],             // ㄹ
    ["k", "lg", "ngn", "ngn", "ngm", "lkh"],    // ㄺ
    ["m", "lm", "mn", "mn", "mm", "lmh"],       // ㄻ
    ["p", "lb", "mn", "mn", "mm", "lph"],       // ㄼ
    ["t", "ls", "nn", "nn", "nm", "lsh"],       // ㄽ
    ["t", "lt", "nn", "nn", "nm", "lth"],       // ㄾ
    ["p", "lp", "mn", "mn", "mm", "lph"],       // ㄿ
    ["l", "lh", "ll", "ll", "lm", "lh"],        // ㅀ
    ["m", "", "", "mn", "", ""],                // ㅁ
    ["p", "b", "mn", "mn", "mm", "ph"],         // ㅂ
    ["p", "ps", "mn", "mn", "mm", "psh"],       // ㅄ
    ["t", "s", "nn", "nn", "nm", "sh"],         // ㅅ
    ["t", "ss", "tn", "tn", "nm", "th"],        // ㅆ
    ["ng", "", "", "", "", ""],                 // ㅇ
    ["t", "j", "nn", "nn", "nm", "ch"],         // ㅈ
    ["t", "ch", "nn", "nn", "nm", "ch"],        // ㅊ
    ["k", "k", "ngn", "ngn", "ngm", "kh"],      // ㅋ
    ["t", "t", "nn", "nn", "nm", "th"],         // ㅌ
    ["p", "p", "mn", "mn", "mm", "ph"],         // ㅍ
    ["t", "h", "nn", "nn", "mm", "t"],          // ㅎ
];
const JONG_HIEUT: usize = 27;
const CHO_G: usize = 0;

/// The final's spelling and whether it already includes the next initial.
fn final_before(jong: usize, next_cho: Option<usize>) -> (&'static str, bool) {
    let row = &JONG[jong];
    // ㅎ is silent before a vowel: 좋아 joa, 않아 ana, 싫어 sireo.
    if next_cho == Some(11) {
        match jong {
            JONG_HIEUT => return ("", true),
            6 => return ("n", true),
            15 => return ("r", true),
            _ => {}
        }
    }
    let ctx = match next_cho {
        Some(11) => row[1], // ㅇ
        Some(2) => row[2],  // ㄴ
        Some(5) => row[3],  // ㄹ
        Some(6) => row[4],  // ㅁ
        Some(18) => row[5], // ㅎ
        Some(CHO_G) if jong == JONG_HIEUT => "k",
        _ => "",
    };
    if ctx.is_empty() {
        (row[0], false)
    } else {
        (ctx, true)
    }
}

pub(crate) fn is_hangul(c: char) -> bool {
    matches!(c, '\u{ac00}'..='\u{d7a3}')
}

pub(crate) fn romanize_korean(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    let mut prev_final: Option<usize> = None;
    for c in text.chars() {
        if is_hangul(c) {
            let idx = c as usize - 0xac00;
            let (cho, jung, jong) = (idx / (21 * 28), (idx % (21 * 28)) / 28, idx % 28);
            let mut initial_done = false;
            if let Some(f) = prev_final.take() {
                let (spelled, merged) = final_before(f, Some(cho));
                out.push_str(spelled);
                initial_done = merged;
            }
            if !initial_done {
                out.push_str(CHO[cho]);
            }
            out.push_str(JUNG[jung]);
            prev_final = (jong != 0).then_some(jong);
        } else {
            if let Some(f) = prev_final.take() {
                out.push_str(final_before(f, None).0);
            }
            out.push(c);
        }
    }
    if let Some(f) = prev_final {
        out.push_str(final_before(f, None).0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revised_romanization() {
        assert_eq!(romanize_korean("한국어"), "hangugeo");
        assert_eq!(romanize_korean("안녕하세요"), "annyeonghaseyo");
        assert_eq!(romanize_korean("신라"), "silla");
        assert_eq!(romanize_korean("사랑해"), "saranghae");
        assert_eq!(romanize_korean("좋아"), "joa");
        assert_eq!(romanize_korean("싫어"), "sireo");
        assert_eq!(romanize_korean("좋고"), "joko");
        assert_eq!(romanize_korean("국민"), "gungmin");
        assert_eq!(romanize_korean("설날"), "seollal");
        assert_eq!(romanize_korean("있어"), "isseo");
        assert_eq!(romanize_korean("의사"), "uisa");
        assert_eq!(romanize_korean("나는 너를"), "naneun neoreul");
        assert_eq!(romanize_korean("Love 사랑!"), "Love sarang!");
    }
}
