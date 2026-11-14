//! Latin-letter readings of lyrics in other scripts, for singing along
//! without reading the script.
//!
//! Only scripts whose sounds follow from the letters are converted here:
//! Hangul (Revised Romanization), kana (Hepburn), Devanagari, Cyrillic and
//! Greek. Chinese characters (and Japanese kanji) need a dictionary for
//! their readings; lines with them are left to romanizations the lyrics
//! services provide.

/// The reading of `text` in Latin letters, or `None` when it has nothing
/// to convert or holds characters this cannot read (kanji, hanzi).
pub fn romanize(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len() * 2);
    let mut converted = false;
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let code = c as u32;
        if is_han(code) {
            return None;
        }
        if let Some(syllable) = hangul(c) {
            // Space between words is kept; syllables of a word run together.
            out.push_str(&syllable);
            converted = true;
        } else if is_kana(code) {
            let (reading, used) = kana(&chars[i..]);
            out.push_str(&reading);
            converted = true;
            i += used;
            continue;
        } else if (0x0900..=0x097F).contains(&code) {
            let word_start = i == 0 || !is_devanagari(chars[i - 1]);
            let (reading, used) = devanagari(&chars[i..], word_start);
            out.push_str(&reading);
            converted = true;
            i += used;
            continue;
        } else if let Some(latin) = cyrillic(c).or_else(|| greek(c)) {
            out.push_str(latin);
            converted = true;
        } else {
            out.push(match c {
                '、' => ',',
                '。' => '.',
                '！' => '!',
                '？' => '?',
                '　' => ' ',
                '「' | '」' | '『' | '』' => '"',
                '（' => '(',
                '）' => ')',
                _ => c,
            });
        }
        i += 1;
    }
    converted.then(|| tidy(&out))
}

/// Romanize every line; `None` unless every line with foreign script could
/// be read (a half-converted song is worse than none).
pub fn romanize_all<'a>(lines: impl Iterator<Item = &'a str>) -> Option<Vec<String>> {
    let mut any = false;
    let mut out = Vec::new();
    for line in lines {
        match romanize(line) {
            Some(latin) => {
                any = true;
                out.push(latin);
            }
            None if line.chars().any(|c| is_han(c as u32)) => return None,
            None => out.push(line.to_string()),
        }
    }
    any.then_some(out)
}

fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_space = false;
    for c in text.chars() {
        if c == ' ' {
            if !last_space {
                out.push(c);
            }
            last_space = true;
        } else {
            out.push(c);
            last_space = false;
        }
    }
    out.trim().to_string()
}

fn is_han(code: u32) -> bool {
    matches!(code, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F)
}

fn is_kana(code: u32) -> bool {
    matches!(code, 0x3041..=0x309F | 0x30A0..=0x30FF | 0x31F0..=0x31FF)
}

// ------------------------------------------------------------------ Hangul

fn hangul(c: char) -> Option<String> {
    const INITIAL: [&str; 19] = [
        "g", "kk", "n", "d", "tt", "r", "m", "b", "pp", "s", "ss", "", "j", "jj", "ch", "k", "t",
        "p", "h",
    ];
    const MEDIAL: [&str; 21] = [
        "a", "ae", "ya", "yae", "eo", "e", "yeo", "ye", "o", "wa", "wae", "oe", "yo", "u", "wo",
        "we", "wi", "yu", "eu", "ui", "i",
    ];
    const FINAL: [&str; 28] = [
        "", "k", "k", "k", "n", "n", "n", "t", "l", "k", "m", "l", "l", "l", "p", "l", "m", "p",
        "p", "t", "t", "ng", "t", "t", "k", "t", "p", "t",
    ];
    let code = c as u32;
    if !(0xAC00..=0xD7A3).contains(&code) {
        return None;
    }
    let index = code - 0xAC00;
    let (initial, medial, last) = (index / 588, (index % 588) / 28, index % 28);
    Some(format!(
        "{}{}{}",
        INITIAL[initial as usize], MEDIAL[medial as usize], FINAL[last as usize]
    ))
}

// -------------------------------------------------------------------- kana

/// Hepburn reading of the kana at the start of `chars`, and how many
/// characters it used.
fn kana(chars: &[char]) -> (String, usize) {
    let base = |c: char| -> Option<&'static str> {
        // Katakana map onto hiragana.
        let c = match c as u32 {
            0x30A1..=0x30F6 => char::from_u32(c as u32 - 0x60).unwrap_or(c),
            _ => c,
        };
        Some(match c {
            'あ' => "a",
            'い' => "i",
            'う' => "u",
            'え' => "e",
            'お' => "o",
            'ぁ' => "a",
            'ぃ' => "i",
            'ぅ' => "u",
            'ぇ' => "e",
            'ぉ' => "o",
            'か' => "ka",
            'き' => "ki",
            'く' => "ku",
            'け' => "ke",
            'こ' => "ko",
            'が' => "ga",
            'ぎ' => "gi",
            'ぐ' => "gu",
            'げ' => "ge",
            'ご' => "go",
            'さ' => "sa",
            'し' => "shi",
            'す' => "su",
            'せ' => "se",
            'そ' => "so",
            'ざ' => "za",
            'じ' => "ji",
            'ず' => "zu",
            'ぜ' => "ze",
            'ぞ' => "zo",
            'た' => "ta",
            'ち' => "chi",
            'つ' => "tsu",
            'て' => "te",
            'と' => "to",
            'だ' => "da",
            'ぢ' => "ji",
            'づ' => "zu",
            'で' => "de",
            'ど' => "do",
            'な' => "na",
            'に' => "ni",
            'ぬ' => "nu",
            'ね' => "ne",
            'の' => "no",
            'は' => "ha",
            'ひ' => "hi",
            'ふ' => "fu",
            'へ' => "he",
            'ほ' => "ho",
            'ば' => "ba",
            'び' => "bi",
            'ぶ' => "bu",
            'べ' => "be",
            'ぼ' => "bo",
            'ぱ' => "pa",
            'ぴ' => "pi",
            'ぷ' => "pu",
            'ぺ' => "pe",
            'ぽ' => "po",
            'ま' => "ma",
            'み' => "mi",
            'む' => "mu",
            'め' => "me",
            'も' => "mo",
            'や' => "ya",
            'ゆ' => "yu",
            'よ' => "yo",
            'ら' => "ra",
            'り' => "ri",
            'る' => "ru",
            'れ' => "re",
            'ろ' => "ro",
            'わ' => "wa",
            'ゐ' => "i",
            'ゑ' => "e",
            'を' => "o",
            'ん' => "n",
            'ゔ' => "vu",
            _ => return None,
        })
    };
    let c = chars[0];
    let next = chars.get(1).copied();
    // Small tsu doubles the next consonant.
    if matches!(c, 'っ' | 'ッ') {
        if next.is_some() {
            let (reading, used) = kana(&chars[1..]);
            if let Some(first) = reading.chars().next().filter(|f| !"aeiou".contains(*f)) {
                let doubled = if reading.starts_with("ch") {
                    't'
                } else {
                    first
                };
                return (format!("{doubled}{reading}"), used + 1);
            }
            return (reading, used + 1);
        }
        return (String::new(), 1);
    }
    // Long vowel mark repeats the vowel before it; handled by the caller's
    // output, so emit a macron-free repeat.
    if c == 'ー' {
        return ("-".to_string(), 1);
    }
    if c == '・' {
        return (" ".to_string(), 1);
    }
    let Some(reading) = base(c) else {
        return (c.to_string(), 1);
    };
    // Small ya/yu/yo combine with the i-row before them: き+ゃ = kya.
    if let Some(small) = next.and_then(|n| match n {
        'ゃ' | 'ャ' => Some('a'),
        'ゅ' | 'ュ' => Some('u'),
        'ょ' | 'ョ' => Some('o'),
        _ => None,
    }) && reading.ends_with('i')
        && reading.len() > 1
    {
        let stem = &reading[..reading.len() - 1];
        let joined = match stem {
            "sh" | "ch" | "j" => format!("{stem}{small}"),
            _ => format!("{stem}y{small}"),
        };
        return (joined, 2);
    }
    (reading.to_string(), 1)
}

// --------------------------------------------------------------- Devanagari

/// ISO 15919-style reading, simplified to plain letters; the inherent
/// vowel is dropped at the end of a word, as Nepali and Hindi are sung.
fn is_devanagari(c: char) -> bool {
    ('\u{0900}'..='\u{097F}').contains(&c)
}

/// `word_start`: the letter begins a word; a one-letter word keeps its
/// vowel (म is "ma").
fn devanagari(chars: &[char], word_start: bool) -> (String, usize) {
    let consonant = |c: char| -> Option<&'static str> {
        Some(match c {
            'क' => "k",
            'ख' => "kh",
            'ग' => "g",
            'घ' => "gh",
            'ङ' => "n",
            'च' => "ch",
            'छ' => "chh",
            'ज' => "j",
            'झ' => "jh",
            'ञ' => "ny",
            'ट' => "t",
            'ठ' => "th",
            'ड' => "d",
            'ढ' => "dh",
            'ण' => "n",
            'त' => "t",
            'थ' => "th",
            'द' => "d",
            'ध' => "dh",
            'न' => "n",
            'प' => "p",
            'फ' => "ph",
            'ब' => "b",
            'भ' => "bh",
            'म' => "m",
            'य' => "y",
            'र' => "r",
            'ल' => "l",
            'व' => "v",
            'श' => "sh",
            'ष' => "sh",
            'स' => "s",
            'ह' => "h",
            'ळ' => "l",
            _ => return None,
        })
    };
    let vowel = |c: char| -> Option<&'static str> {
        Some(match c {
            'अ' => "a",
            'आ' => "aa",
            'इ' => "i",
            'ई' => "i",
            'उ' => "u",
            'ऊ' => "u",
            'ऋ' => "ri",
            'ए' => "e",
            'ऐ' => "ai",
            'ओ' => "o",
            'औ' => "au",
            _ => return None,
        })
    };
    let sign = |c: char| -> Option<&'static str> {
        Some(match c {
            'ा' => "aa",
            'ि' => "i",
            'ी' => "i",
            'ु' => "u",
            'ू' => "u",
            'ृ' => "ri",
            'े' => "e",
            'ै' => "ai",
            'ो' => "o",
            'ौ' => "au",
            _ => return None,
        })
    };
    let c = chars[0];
    if let Some(v) = vowel(c) {
        return (v.to_string(), 1);
    }
    match c {
        'ं' | 'ँ' => return ("n".to_string(), 1),
        'ः' => return ("h".to_string(), 1),
        '।' | '॥' => return (".".to_string(), 1),
        '़' | '्' => return (String::new(), 1),
        _ => {}
    }
    if let Some(digit) = c.to_digit(10).filter(|_| ('०'..='९').contains(&c)) {
        return (digit.to_string(), 1);
    }
    let Some(base) = consonant(c) else {
        return (String::new(), 1);
    };
    let mut used = 1;
    // A nukta modifies the letter before it.
    if chars.get(used) == Some(&'़') {
        used += 1;
    }
    match chars.get(used).copied() {
        Some('्') => (base.to_string(), used + 1),
        Some(next) if sign(next).is_some() => (
            format!("{base}{}", sign(next).unwrap_or_default()),
            used + 1,
        ),
        next => {
            // Inherent "a", silent at the end of a word.
            let word_ends = next.is_none_or(|n| !is_devanagari(n));
            let vowel = if word_ends && !word_start { "" } else { "a" };
            (format!("{base}{vowel}"), used)
        }
    }
}

// ----------------------------------------------------- Cyrillic and Greek

fn cyrillic(c: char) -> Option<&'static str> {
    let lower = c.to_lowercase().next().unwrap_or(c);
    let latin = match lower {
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
        'ъ' => "",
        'ы' => "y",
        'ь' => "",
        'э' => "e",
        'ю' => "yu",
        'я' => "ya",
        'є' => "ye",
        'і' => "i",
        'ї' => "yi",
        'ґ' => "g",
        'ў' => "w",
        _ => return None,
    };
    Some(latin)
}

fn greek(c: char) -> Option<&'static str> {
    let lower = c.to_lowercase().next().unwrap_or(c);
    let latin = match lower {
        'α' | 'ά' => "a",
        'β' => "v",
        'γ' => "g",
        'δ' => "d",
        'ε' | 'έ' => "e",
        'ζ' => "z",
        'η' | 'ή' => "i",
        'θ' => "th",
        'ι' | 'ί' | 'ϊ' => "i",
        'κ' => "k",
        'λ' => "l",
        'μ' => "m",
        'ν' => "n",
        'ξ' => "x",
        'ο' | 'ό' => "o",
        'π' => "p",
        'ρ' => "r",
        'σ' | 'ς' => "s",
        'τ' => "t",
        'υ' | 'ύ' | 'ϋ' => "y",
        'φ' => "f",
        'χ' => "ch",
        'ψ' => "ps",
        'ω' | 'ώ' => "o",
        _ => return None,
    };
    Some(latin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hangul_reads() {
        assert_eq!(romanize("사랑해").as_deref(), Some("saranghae"));
        assert_eq!(romanize("안녕 하세요").as_deref(), Some("annyeong haseyo"));
    }

    #[test]
    fn kana_reads() {
        assert_eq!(romanize("ありがとう").as_deref(), Some("arigatou"));
        assert_eq!(romanize("きょう").as_deref(), Some("kyou"));
        assert_eq!(romanize("ちょっと").as_deref(), Some("chotto"));
        assert_eq!(romanize("カタカナ").as_deref(), Some("katakana"));
        // Kanji cannot be read without a dictionary.
        assert_eq!(romanize("心全部満たしたい"), None);
    }

    #[test]
    fn devanagari_reads() {
        assert_eq!(romanize("म हाँसिदिउँला").as_deref(), Some("ma haansidiunlaa"));
        assert_eq!(romanize("नमस्ते").as_deref(), Some("namaste"));
    }

    #[test]
    fn cyrillic_and_greek_read() {
        assert_eq!(romanize("Привет").as_deref(), Some("privet"));
        assert_eq!(romanize("καλημέρα").as_deref(), Some("kalimera"));
    }

    #[test]
    fn latin_only_is_nothing_to_do() {
        assert_eq!(romanize("Hello there"), None);
    }

    #[test]
    fn songs_with_kanji_are_not_half_converted() {
        assert!(romanize_all(["ありがとう", "心"].into_iter()).is_none());
        assert_eq!(
            romanize_all(["ありがとう", "yeah"].into_iter()),
            Some(vec!["arigatou".to_string(), "yeah".to_string()])
        );
    }
}
