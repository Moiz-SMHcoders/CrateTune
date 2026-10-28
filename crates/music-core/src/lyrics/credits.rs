//! Credit and metadata lines that providers put around the lyrics: a
//! "Title - Artist" header, "Written by: …", "作词：…", "작곡 : …", slash-separated
//! songwriter lists ("Julian Casablancas/Thomas Bangalter/…"), producer and studio
//! names, copyright notices. Shared by every format (TTML, QRC, LRC, plain).
//!
//! Only a leading block (and a short trailing block) is examined, never lines in
//! the middle of a song. A line is dropped when it carries a strong signal (a known
//! credit label, a name list containing one of the track's artists, a title line,
//! a notice) or a weak one (an ambiguous label such as "Music:", a list of names,
//! a lone Chinese name) backed by timing or context: it spans 0 s, its words are
//! evenly timed (synthesized by the provider), it starts at 0, or the line next to
//! it is a credit.

use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;

use super::types::{LyricLine, LyricsQuery};

/// Lines examined at the start / end of a song.
const HEAD_LIMIT: usize = 16;
const TAIL_LIMIT: usize = 8;

/// What is known about the track: its title(s) and artist names, folded with [`fold`].
#[derive(Debug, Clone, Default)]
pub(crate) struct CreditContext {
    titles: Vec<String>,
    names: Vec<String>,
}

impl CreditContext {
    /// From a provider's own metadata (`[ti:]` / `[ar:]` tags).
    pub(crate) fn from_metadata(title: Option<&str>, artist: Option<&str>) -> Self {
        let mut ctx = Self::default();
        if let Some(t) = title {
            ctx.add_title(t);
        }
        if let Some(a) = artist {
            ctx.add_artists(a);
        }
        ctx
    }

    /// From the track: title (with and without `(feat. …)`), artists, featured artists.
    pub(crate) fn from_query(query: &LyricsQuery) -> Self {
        let mut ctx = Self::default();
        ctx.add_title(&query.title);
        ctx.add_artists(&query.artist);
        ctx
    }

    pub(crate) fn merge(mut self, other: &Self) -> Self {
        for t in &other.titles {
            push_unique(&mut self.titles, t.clone());
        }
        for n in &other.names {
            push_unique(&mut self.names, n.clone());
        }
        self
    }

    fn add_title(&mut self, title: &str) {
        push_unique(&mut self.titles, fold(title));
        push_unique(&mut self.titles, fold(&strip_brackets(title)));
        // "(feat. X)", "[with X]": featured artists are credited too.
        static FEAT: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"(?i)[(\[（]\s*(?:feat\.?|ft\.?|featuring|with)\s+([^)\]）]+)[)\]）]")
                .unwrap()
        });
        for c in FEAT.captures_iter(title) {
            self.add_artists(&c[1]);
        }
    }

    fn add_artists(&mut self, artists: &str) {
        push_unique(&mut self.names, fold(artists));
        static SPLIT: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
                r"(?i)\s*(?:[,，、/／&＆;；]|\s(?:x|feat\.?|ft\.?|featuring|with|and|vs\.?)\s)\s*",
            )
            .unwrap()
        });
        for part in SPLIT.split(artists) {
            push_unique(&mut self.names, fold(part));
        }
    }

    fn is_name(&self, text: &str) -> bool {
        let f = fold(text);
        f.chars().count() >= 2
            && self.names.iter().any(|n| {
                let (nl, fl) = (n.chars().count(), f.chars().count());
                *n == f
                    // "Daft Punk (Producer)" contains the artist; a fragment of a
                    // name ("Julian") is not enough.
                    || (nl >= 4 && f.contains(n.as_str()))
                    || (fl >= 6 && fl * 2 >= nl && n.contains(f.as_str()))
            })
    }

    fn is_title(&self, text: &str) -> bool {
        let f = fold(text);
        !f.is_empty() && self.titles.contains(&f)
    }

    /// `text` contains the title (as a whole folded substring of at least 2 chars).
    fn contains_title(&self, text: &str) -> bool {
        let f = fold(text);
        self.titles
            .iter()
            .any(|t| t.chars().count() >= 2 && f.contains(t.as_str()))
    }
}

fn push_unique(v: &mut Vec<String>, s: String) {
    if !s.is_empty() && !v.contains(&s) {
        v.push(s);
    }
}

/// Lowercase alphanumerics after transliteration, so "ROSALÍA" = "Rosalia" and
/// "周杰倫" = "周杰伦" (both "zhoujielun").
pub(crate) fn fold(s: &str) -> String {
    deunicode::deunicode(s)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn strip_brackets(s: &str) -> String {
    static BRACKETS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\s*(?:\([^)]*\)|\[[^\]]*\]|（[^）]*）|【[^】]*】)").unwrap());
    BRACKETS.replace_all(s, "").trim().to_owned()
}

/// One line as the classifier sees it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CreditLine<'a> {
    pub text: &'a str,
    /// `(start, end)` for synced lyrics.
    pub timing: Option<(Duration, Duration)>,
    /// Real word timings that are all (nearly) the same length: synthesized by the provider.
    pub uniform_words: bool,
}

impl<'a> CreditLine<'a> {
    pub(crate) fn plain(text: &'a str) -> Self {
        Self {
            text,
            timing: None,
            uniform_words: false,
        }
    }

    /// `word_synced`: whether the words' timings came from the source.
    pub(crate) fn synced(line: &'a LyricLine, word_synced: bool) -> Self {
        Self {
            text: &line.text,
            timing: Some((line.start, line.end)),
            uniform_words: word_synced && uniform_words(line),
        }
    }

    fn zero_span(&self) -> bool {
        self.timing
            .is_some_and(|(s, e)| e <= s + Duration::from_millis(20))
    }
}

fn uniform_words(line: &LyricLine) -> bool {
    let durations: Vec<u128> = line
        .words
        .iter()
        .filter(|w| !w.text.trim().is_empty())
        .map(|w| w.end.saturating_sub(w.start).as_millis())
        .collect();
    if durations.len() < 3 || durations.contains(&0) {
        return false;
    }
    let (min, max) = (
        durations.iter().min().copied().unwrap_or(0),
        durations.iter().max().copied().unwrap_or(0),
    );
    max - min <= 15
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Signal {
    None,
    Weak,
    Strong,
}

/// How many lines at the start and at the end are credits.
pub(crate) fn credit_block(lines: &[CreditLine], ctx: &CreditContext) -> (usize, usize) {
    let mut head = 0;
    for (i, line) in lines.iter().enumerate().take(HEAD_LIMIT) {
        let signal = classify(line, ctx, Some(i));
        let supported = line.zero_span()
            || line.uniform_words
            || (i > 0 && head == i)
            || lines
                .get(i + 1)
                .is_some_and(|next| classify(next, ctx, Some(i + 1)) == Signal::Strong);
        if signal == Signal::Strong || (signal == Signal::Weak && supported) {
            head = i + 1;
        } else {
            break;
        }
    }
    let mut tail = 0;
    let n = lines.len();
    for k in 0..TAIL_LIMIT.min(n - head) {
        let line = &lines[n - 1 - k];
        let signal = classify(line, ctx, None);
        let supported = k > 0 || line.zero_span() || line.uniform_words;
        if signal == Signal::Strong || (signal == Signal::Weak && supported) {
            tail = k + 1;
        } else {
            break;
        }
    }
    (head, tail)
}

/// `index`: position in the head block (title lines only count near the top).
fn classify(line: &CreditLine, ctx: &CreditContext, index: Option<usize>) -> Signal {
    let text = line.text.trim();
    if text.is_empty() {
        return Signal::None;
    }
    if is_notice(text) {
        return Signal::Strong;
    }
    if let Some(s) = labelled(text) {
        return s;
    }
    if let Some(s) = by_line(text) {
        return s;
    }
    if index.is_some_and(|i| i < 3) {
        if is_title_line(text, ctx) {
            return Signal::Strong;
        }
        if (index == Some(0) && ctx.is_title(text)) || looks_like_title_line(text) {
            return Signal::Weak;
        }
    }
    name_list(text, ctx)
}

// ---- labels ---------------------------------------------------------------------------

static LABEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[(\[（【]?\s*([^:：()\[\]（）【】]{1,40}?)\s*[)\]）】]?\s*[:：]\s*(.*)$").unwrap()
});

/// Labels that only ever introduce credits.
#[rustfmt::skip]
const STRONG_LABELS: &[&str] = &[
    // English
    "written by", "writer", "writers", "songwriter", "songwriters", "songwriting", "lyricist",
    "lyricists", "lyrics by", "lyric by", "words by", "words and music", "words & music",
    "music by", "music and lyrics", "music & lyrics", "lyrics and music", "lyrics & music",
    "composer", "composers", "composed by", "composition", "producer", "producers", "produced by",
    "production", "executive producer", "co-producer", "additional production", "arranger",
    "arrangers", "arranged by", "arrangement", "arrangements", "mixed by", "mixing",
    "mixing engineer", "mix engineer", "mastered by", "mastering", "mastering engineer",
    "recorded by", "recording", "recording engineer", "recording engineers", "engineer",
    "engineers", "engineered by", "assistant engineer", "programming", "programmed by",
    "vocal producer", "vocal production", "vocal arrangement", "vocal director",
    "vocal directing", "vocal editing", "vocal engineer", "background vocals", "backing vocals",
    "publisher", "publishers", "published by", "publishing", "label", "copyright",
    "original artist", "originally performed by", "performed by", "cover by", "translated by",
    "lrc by", "synced by", "lyrics provided by", "transcribed by", "a&r", "artist",
    // Spanish / Portuguese / French / German / Italian
    "letra y música", "letra y musica", "compositor", "compositores", "autor", "autores",
    "productor", "productores", "producción", "produccion", "arreglos", "arreglista",
    "composição", "composicao", "paroles et musique", "compositeur", "compositeurs", "auteur",
    "auteurs", "réalisation", "komponist", "texter", "text und musik", "autore", "autori",
    "compositore", "testo e musica",
    // Chinese
    "作词", "作詞", "作曲", "编曲", "編曲", "词", "詞", "曲", "词曲", "詞曲", "作词作曲", "作詞作曲",
    "制作人", "製作人", "制作", "製作", "监制", "監製", "出品", "出品人", "发行", "發行", "原唱", "演唱",
    "翻唱", "歌手", "和声", "和聲", "和音", "混音", "混音师", "混音師", "母带", "母帶", "录音", "錄音",
    "录音师", "錄音師", "吉他", "贝斯", "貝斯", "鼓", "键盘", "鍵盤", "弦乐", "弦樂", "人声", "人聲",
    "统筹", "統籌", "企划", "企劃", "策划", "策劃", "监修", "監修", "版权", "版權", "op", "sp",
    // Japanese
    "作詞者", "作曲者", "編曲者", "作詞・作曲", "唄", "ボーカル",
    // Korean
    "작사", "작곡", "편곡", "프로듀서", "프로듀싱", "가사", "믹싱", "마스터링", "녹음", "보컬", "제작",
];

/// Labels that are credits only when what follows looks like names.
#[rustfmt::skip]
const AMBIGUOUS_LABELS: &[&str] = &[
    "music", "lyrics", "lyric", "words", "vocals", "vocal", "guitar", "guitars", "bass", "drums",
    "piano", "keys", "keyboards", "strings", "text", "letra", "música", "musica", "musique",
    "paroles", "musik", "testo", "chorus", "feat", "featuring", "歌", "노래",
];

/// CJK fragments that make a short label a credit label ("和声编写", "吉他手", "작사·작곡").
#[rustfmt::skip]
const CJK_LABEL_KEYWORDS: &[&str] = &[
    "词", "詞", "曲", "唱", "制作", "製作", "监制", "監製", "混音", "录音", "錄音", "编", "編", "版权",
    "版權", "吉他", "贝斯", "貝斯", "和声", "和聲", "策划", "企划", "统筹", "母带", "工程", "작사",
    "작곡", "편곡", "프로듀", "믹싱", "마스터링", "녹음", "제작",
];

fn labelled(text: &str) -> Option<Signal> {
    let caps = LABEL.captures(text)?;
    let label = caps[1].trim().to_lowercase();
    let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
    let value = caps[2].trim();
    if value.is_empty() {
        return None;
    }
    let has_cjk = label.chars().any(is_cjk);
    // "Keyboard by：…", "Background Vocals by：…"
    let role_by = !has_cjk
        && label.ends_with(" by")
        && label.split_whitespace().count() <= 4
        && label
            .chars()
            .all(|c| c.is_alphabetic() || c == ' ' || c == '-');
    if STRONG_LABELS.contains(&label.as_str())
        || role_by
        || (has_cjk
            && label.chars().count() <= 10
            && CJK_LABEL_KEYWORDS.iter().any(|k| label.contains(k)))
    {
        return Some(Signal::Strong);
    }
    if AMBIGUOUS_LABELS.contains(&label.as_str()) {
        return Some(if is_name_list(value) {
            Signal::Strong
        } else {
            Signal::Weak
        });
    }
    None
}

/// "Written by Max Martin", "Produced by Pharrell & Chad Hugo" (names only).
fn by_line(text: &str) -> Option<Signal> {
    static BY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)^(?:written|lyrics|lyric|words|music|composed|produced|arranged|mixed|mastered|recorded|programmed)(?:\s+(?:and|&)\s+\w+)?\s+by\s+(.+)$",
        )
        .unwrap()
    });
    let caps = BY.captures(text)?;
    is_name_list(&caps[1]).then_some(Signal::Strong)
}

/// Copyright and "provided by" notices.
fn is_notice(text: &str) -> bool {
    #[rustfmt::skip]
    const NOTICES: &[&str] = &[
        "未经著作权人", "未经许可", "未經許可", "不得翻唱", "不得翻录", "版权所有", "版權所有",
        "all rights reserved", "lyrics provided by", "lyrics licensed", "lyrics powered by",
        "provided by musixmatch", "歌词贡献者", "歌詞貢獻者", "unauthorized reproduction",
    ];
    let lower = text.to_lowercase();
    NOTICES.iter().any(|n| lower.contains(n))
}

// ---- title lines and name lists -------------------------------------------------------

/// "Title - Artist", "Artist - Title", "Title - uploader" near the top.
pub(crate) fn is_title_line(text: &str, ctx: &CreditContext) -> bool {
    static SEP: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\s+[-–—~]\s+|\s*[|｜]\s*").unwrap());
    let parts: Vec<&str> = SEP
        .split(text)
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() < 2 {
        return false;
    }
    let titled: Vec<bool> = parts
        .iter()
        .map(|p| ctx.is_title(p) || ctx.contains_title(p))
        .collect();
    let Some(t) = titled.iter().position(|t| *t) else {
        return false;
    };
    // The rest: the artist, or a name (uploader) that isn't the title again.
    parts
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != t)
        .all(|(_, p)| {
            ctx.is_name(p) || (is_name_like(p) && !ctx.is_title(p) && !ctx.contains_title(p))
        })
}

/// "周杰伦 - 晴天", "Daft Punk - Instant Crush" without knowing the track: two short
/// parts around a dash, both without sentence punctuation.
fn looks_like_title_line(text: &str) -> bool {
    let Some((a, b)) = text.split_once(" - ") else {
        return false;
    };
    let short = |p: &str| {
        let p = p.trim();
        !p.is_empty()
            && p.chars().count() <= 40
            && !p.contains(" - ")
            && !p
                .chars()
                .any(|c| matches!(c, ',' | '，' | '.' | '!' | '?' | '！' | '？' | '。'))
    };
    short(a) && short(b)
}

fn name_list(text: &str, ctx: &CreditContext) -> Signal {
    let parts = split_names(text);
    if parts.is_empty() || !parts.iter().all(|p| is_name_like(p)) {
        return Signal::None;
    }
    if parts.iter().any(|p| ctx.is_name(p)) {
        return Signal::Strong;
    }
    if parts.len() >= 2 || parts.iter().all(|p| is_chinese_name(p)) {
        return Signal::Weak;
    }
    Signal::None
}

fn is_name_list(text: &str) -> bool {
    let parts = split_names(text);
    !parts.is_empty() && parts.iter().all(|p| is_name_like(p))
}

/// Splits a credit value into names, dropping parenthetical notes ("(Alfa Studio)").
fn split_names(text: &str) -> Vec<String> {
    static SPLIT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\s*(?:[/／、,，&＆;；]|\s(?:x|and)\s)\s*").unwrap());
    let stripped = strip_brackets(text);
    SPLIT
        .split(&stripped)
        .map(|p| collapse_spaced_cjk(p.trim()))
        .filter(|p| !p.is_empty())
        .collect()
}

/// "周 杰 伦" → "周杰伦" (one CJK char per token).
fn collapse_spaced_cjk(s: &str) -> String {
    let tokens: Vec<&str> = s.split_whitespace().collect();
    if tokens.len() > 1
        && tokens
            .iter()
            .all(|t| t.chars().count() == 1 && t.chars().all(is_cjk))
    {
        tokens.concat()
    } else {
        s.to_owned()
    }
}

/// Lowercase words allowed inside a Latin name ("Guy-Manuel de Homem-Christo").
const NAME_PARTICLES: &[&str] = &[
    "de", "da", "del", "della", "der", "den", "di", "do", "dos", "du", "la", "le", "van", "von",
    "y", "e", "bin", "al", "el", "st.", "mc",
];

fn is_name_like(part: &str) -> bool {
    let part = part.trim();
    if part.is_empty() || part.chars().count() > 48 {
        return false;
    }
    if part.chars().any(is_cjk) {
        let n = part.chars().count();
        return (1..=8).contains(&n)
            && part
                .chars()
                .all(|c| is_cjk(c) || matches!(c, '・' | '·' | '•'));
    }
    let words: Vec<&str> = part.split_whitespace().collect();
    if words.is_empty() || words.len() > 5 || !part.chars().any(char::is_alphabetic) {
        return false;
    }
    words.iter().all(|w| {
        let first = w.chars().next().unwrap_or(' ');
        let charset = w
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '\'' | '’' | '.' | '$'));
        charset
            && (first.is_uppercase()
                || first.is_ascii_digit()
                || NAME_PARTICLES.contains(&w.to_lowercase().as_str()))
    })
}

/// 2–4 Han characters starting with a common surname ("陈任佑", "杨大纬").
fn is_chinese_name(part: &str) -> bool {
    #[rustfmt::skip]
    const SURNAMES: &str = "王李张張刘劉陈陳杨楊黄黃赵趙吴吳周徐孙孫马馬朱胡郭何高林罗羅郑鄭梁谢謝宋唐许許韩韓冯馮邓鄧曹彭曾肖萧蕭田董袁潘于蒋蔣蔡余杜叶葉程苏蘇魏吕呂丁任沈姚卢盧姜崔钟鍾谭譚陆陸汪范金石廖贾賈夏韦韋付方白邹鄒孟熊秦邱江尹薛闫閆段雷侯龙龍史陶黎贺賀顾顧毛郝龚龔邵万萬钱錢严嚴覃武戴莫孔向汤湯常温溫康施文牛樊葛邢安齐齊易乔喬伍庞龐颜顏倪庄莊聂聶章鲁魯岳翟殷詹申欧歐耿关關兰蘭焦俞左柳甘祝包宁寧尚符舒阮柯纪紀梅童凌毕畢单單季裴霍涂成苗谷盛曲翁冉骆駱蓝藍路游辛靳管柴蒙鲍鮑华華喻祁蒲房滕屈饶饒解牟艾尤阳陽时時穆农農司卓古吉缪繆简簡车車项項连連芦蘆麦麥褚娄婁窦竇戚岑景党宫宮费費卜冷晏席卫衛米柏宗瞿桂全佟应應臧闵閔苟邬鄔边邊卞姬师師和仇栾欒隋商刁沙荣榮巫寇桑郎甄丛叢仲虞敖巩鞏明佘池查麻苑迟遲邝鄺";
    let n = part.chars().count();
    (2..=4).contains(&n)
        && part.chars().all(|c| matches!(c, '\u{4e00}'..='\u{9fff}'))
        && part.chars().next().is_some_and(|c| SURNAMES.contains(c))
}

fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4e00}'..='\u{9fff}' | '\u{3400}'..='\u{4dbf}' | '\u{f900}'..='\u{faff}'
        | '\u{3040}'..='\u{30ff}' | '\u{ac00}'..='\u{d7af}' | '\u{1100}'..='\u{11ff}'
        | '\u{3130}'..='\u{318f}')
}

// ---- instrumental markers -------------------------------------------------------------

/// "纯音乐，请欣赏", "Instrumental", "♪": what providers return for songs without lyrics.
pub(crate) fn is_instrumental_marker(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return true;
    }
    #[rustfmt::skip]
    const CJK: &[&str] = &[
        "纯音乐", "純音樂", "没有填词", "沒有填詞", "无歌词", "無歌詞", "インスト", "연주곡",
        "此歌曲为没有填词",
    ];
    if CJK.iter().any(|m| t.contains(m)) {
        return true;
    }
    #[rustfmt::skip]
    const LATIN: &[&str] = &[
        "instrumental", "music", "instrumentalbreak", "instrumentalversion", "instrumentalonly",
        "nolyrics", "thisisaninstrumental", "thisisaninstrumentaltrack",
        "thissongisaninstrumental", "thissongisinstrumental",
    ];
    let letters: String = t
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    letters.is_empty() || LATIN.contains(&letters.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(title: &str, artist: &str) -> CreditContext {
        CreditContext::from_query(&LyricsQuery::new(title, artist))
    }

    fn head(texts: &[&str], ctx: &CreditContext) -> usize {
        let lines: Vec<CreditLine> = texts.iter().map(|t| CreditLine::plain(t)).collect();
        credit_block(&lines, ctx).0
    }

    #[test]
    fn folding() {
        assert_eq!(fold("ROSALÍA"), fold("Rosalia"));
        assert_eq!(fold("周杰倫"), fold("周杰伦"));
        assert_eq!(fold("Guy-Manuel"), "guymanuel");
    }

    #[test]
    fn labels() {
        let c = CreditContext::default();
        for credit in [
            "Written by: Max Martin",
            "Lyrics by：Julian Casablancas/Thomas Bangalter",
            "作词 : 周杰伦",
            "词：Gigi/Ylva Dimberg/HANNI",
            "曲：250/Ylva Dimberg",
            "和声编写：某人",
            "작사 : 민지",
            "作詞：Ayase",
            "Keyboard by：Pdogg",
            "Composer：Rosalía Vila/Pablo Diaz-Reixa",
            "Produced by Pharrell Williams & Chad Hugo",
            "Music: Daft Punk",
            "(Producer: Someone)",
            "OP：Sony Music Publishing",
            "未经著作权人书面许可，不得翻唱",
        ] {
            assert_eq!(
                classify(&CreditLine::plain(credit), &c, Some(5)),
                Signal::Strong,
                "{credit}"
            );
        }
        for lyric in [
            "Baby: don't go",
            "Music: it's all I need",
            "Produced by the night and the rain",
            "I said: hello",
            "1 2 3 4",
            "I didn't want to be the one to forget",
            "故事的小黄花",
            "Love/Hate",
            "24/7, 365",
            "AC/DC on the radio",
        ] {
            assert_ne!(
                classify(&CreditLine::plain(lyric), &c, Some(5)),
                Signal::Strong,
                "{lyric}"
            );
        }
    }

    #[test]
    fn name_lists_need_the_artist_or_support() {
        let daft = ctx("Instant Crush (feat. Julian Casablancas)", "Daft Punk");
        let credit = "Julian Casablancas/Thomas Bangalter/Guy-Manuel de Homem-Christo";
        assert_eq!(name_list(credit, &daft), Signal::Strong);
        assert_eq!(name_list(credit, &CreditContext::default()), Signal::Weak);
        // Lyrics with slashes are not names.
        assert_eq!(name_list("I'm on my way / to the top", &daft), Signal::None);
        assert_eq!(name_list("Love/Hate", &daft), Signal::Weak);
        assert_eq!(head(&["Love/Hate", "Is all I feel"], &daft), 0);
        assert_eq!(head(&["24/7", "I think of you"], &daft), 0);
    }

    #[test]
    fn title_lines() {
        let c = ctx("晴天", "周杰倫");
        assert!(is_title_line("周杰伦 - 晴天", &c));
        let c = ctx("Hype Boy", "NewJeans");
        assert!(is_title_line("Hype Boy - 柯雯", &c));
        assert!(!is_title_line("Hype Boy - Hype Boy", &c));
        let c = ctx("MALAMENTE (Cap.1: Augurio)", "ROSALÍA");
        assert!(is_title_line(
            "MALAMENTE (Cap.1: Augurio) (Cap.1: Augurio) - ROSALÍA",
            &c
        ));
        // A chorus line that starts with the title is not a title line.
        let c = ctx("Hello", "Adele");
        assert_eq!(head(&["Hello, it's me", "I was wondering"], &c), 0);
    }

    #[test]
    fn chinese_producer_block() {
        let c = ctx("晴天", "周杰倫");
        let lines = [
            "周 杰 伦",
            "陈 任 佑",
            "陈 柏 州",
            "刘 勇 志",
            "杨 瑞 代 （ Alfa Studio ）",
            "杨大纬（杨大纬录音工作室）",
            "故事的小黄花",
            "从出生那年就飘着",
        ];
        assert_eq!(head(&lines, &c), 6);
        // Without the artist the block is not anchored and stays.
        assert_eq!(head(&lines[1..], &CreditContext::default()), 0);
    }

    #[test]
    fn instrumental() {
        for m in [
            "此歌曲为没有填词的纯音乐，请您欣赏",
            "纯音乐，请欣赏",
            "Instrumental",
            "(Instrumental)",
            "♪",
            "This is an instrumental track",
        ] {
            assert!(is_instrumental_marker(m), "{m}");
        }
        for l in [
            "Instrumental love",
            "I play instrumental music all night long",
        ] {
            assert!(!is_instrumental_marker(l), "{l}");
        }
    }

    // ---- real provider payloads (trimmed) ----------------------------------------------

    use crate::lyrics::LyricsResult;
    use crate::lyrics::providers::fixture;

    fn first_texts(r: &LyricsResult, n: usize) -> Vec<String> {
        match &r.synced {
            Some(l) => l.lines.iter().take(n).map(|l| l.text.clone()).collect(),
            None => r
                .plain
                .as_deref()
                .unwrap_or_default()
                .lines()
                .take(n)
                .map(str::to_owned)
                .collect(),
        }
    }

    #[test]
    fn youlyplus_songwriter_line() {
        let raw = fixture("youlyplus_instant_crush_credits.ttml");
        // Evenly timed words: dropped even without knowing the artists.
        let r = LyricsResult::parse("YouLyPlus", raw.clone(), Some(338)).unwrap();
        assert_eq!(first_texts(&r, 1), ["I didn't wanna be the one to forget"]);
        assert_eq!(r.synced.as_ref().unwrap().lines.len(), 2);
        assert_eq!(r.extras.len(), 2);
        assert!(!r.plain.as_deref().unwrap().contains("Casablancas"));
        let q = LyricsQuery::new(
            "Instant Crush (feat. Julian Casablancas)",
            "Daft Punk, Julian Casablancas",
        );
        assert_eq!(
            r.without_credits(&q).unwrap().synced.unwrap().lines.len(),
            2
        );
    }

    #[test]
    fn portato_title_and_credit_lines() {
        let r = LyricsResult::parse(
            "BetterLyrics Portato",
            fixture("portato_hype_boy.qrc"),
            Some(180),
        )
        .unwrap();
        assert_eq!(
            first_texts(&r, 2),
            ["1 2 3 4", "Baby got me looking so crazy"]
        );

        let r = LyricsResult::parse(
            "BetterLyrics Portato",
            fixture("portato_malamente.qrc"),
            Some(151),
        )
        .unwrap();
        assert_eq!(first_texts(&r, 1), ["Ese cristalito roto"]);
        // The title line's "(Cap.1:" is not taken for a singer.
        assert!(r.agents.is_empty(), "{:?}", r.agents);
    }

    #[test]
    fn lrclib_header_needs_the_query() {
        let r = LyricsResult::parse("LrcLib", fixture("lrclib_qingtian.lrc"), Some(269)).unwrap();
        // 词：/曲： are labels, and the "Artist - Title" header right above them goes too.
        assert_eq!(first_texts(&r, 1), ["故事的小黄花"]);
        // Alone, an "Artist - Title" header needs to know the track.
        let alone = "[00:00.18]周杰伦 - 晴天\n[00:29.39]故事的小黄花";
        let r = LyricsResult::parse("LrcLib", alone, Some(269)).unwrap();
        assert_eq!(first_texts(&r, 1), ["周杰伦 - 晴天"]);
        let r = r
            .without_credits(&LyricsQuery::new("晴天", "周杰倫"))
            .unwrap();
        assert_eq!(first_texts(&r, 1), ["故事的小黄花"]);
        assert_eq!(
            r.synced.unwrap().lines[0].start,
            Duration::from_millis(29_390)
        );
    }

    #[test]
    fn simpmusic_producer_names() {
        let r =
            LyricsResult::parse("SimpMusic", fixture("simpmusic_qingtian.lrc"), Some(269)).unwrap();
        let r = r
            .without_credits(&LyricsQuery::new("晴天", "周杰倫"))
            .unwrap();
        assert_eq!(first_texts(&r, 1), ["故 事 的 小 黄 花"]);
    }

    #[test]
    fn instrumental_markers() {
        for f in [
            "portato_time_instrumental.lrc",
            "kugou_time_instrumental.lrc",
        ] {
            let r = LyricsResult::parse("x", fixture(f), Some(276)).unwrap();
            assert!(r.is_instrumental(), "{f}");
        }
        let r = LyricsResult::parse("x", fixture("lrclib_qingtian.lrc"), Some(269)).unwrap();
        assert!(!r.is_instrumental());
    }

    #[test]
    fn plain_text_credits() {
        let plain = "Written by: Max Martin\nProduced by Shellback\n\nI got this feeling\nInside my bones\n\nLyrics provided by Example";
        let r = LyricsResult::parse("YouTube Music", plain, None).unwrap();
        assert_eq!(
            r.plain.as_deref(),
            Some("I got this feeling\nInside my bones")
        );
        let all_credits = LyricsResult::parse("x", "作词：A\n作曲：B", None);
        assert!(all_credits.is_none());
    }

    #[test]
    fn real_lyrics_survive() {
        // Lyrics at 0 s, lines with "/" and colons, a title-led chorus.
        let lrc = "[00:00.90]沈むように溶けてゆくように\n[00:07.73]二人だけの空が広がる夜に";
        let r = LyricsResult::parse("x", lrc, Some(261)).unwrap();
        assert_eq!(r.synced.unwrap().lines.len(), 2);
        let lrc = "[00:00.00]Love/Hate, it's all the same\n[00:03.00]Baby: don't you go\n[00:06.00]AC/DC on the radio";
        let q = LyricsQuery::new("Love/Hate", "Someone");
        let r = LyricsResult::parse("x", lrc, None)
            .unwrap()
            .without_credits(&q)
            .unwrap();
        assert_eq!(r.synced.unwrap().lines.len(), 3);
        let lrc = "[00:12.00]Hello, it's me\n[00:15.00]I was wondering";
        let r = LyricsResult::parse("x", lrc, None)
            .unwrap()
            .without_credits(&LyricsQuery::new("Hello", "Adele"))
            .unwrap();
        assert_eq!(r.synced.unwrap().lines.len(), 2);
        let lrc = "[00:00.00]Mary, Mary\n[00:03.00]Quite contrary";
        let r = LyricsResult::parse("x", lrc, None)
            .unwrap()
            .without_credits(&LyricsQuery::new("Mary", "X"))
            .unwrap();
        assert_eq!(
            r.synced.unwrap().lines.len(),
            2,
            "a weak name list needs more than t = 0"
        );
    }
}
