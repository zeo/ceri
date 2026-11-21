// text segmentation after Unicode UAX 29: grapheme clusters, from the line breaking classes and
// Extended_Pictographic the line breaker already carries, and the word and sentence segments
// built over them; dictionary segmentation of scripts written without spaces is not done, so
// such text splits into one segment per character

use crate::linebreak::{LineBreakClass, line_break_class};
use crate::text_tables::EXTENDED_PICTOGRAPHIC;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Grapheme {
    Cr,
    Lf,
    Control,
    Extend,
    Zwj,
    RegionalIndicator,
    L,
    V,
    T,
    Lv,
    Lvt,
    // a consonant of a script whose virama joins it to the next one
    Consonant,
    Linker,
    Other,
}

// Indic_Conjunct_Break from DerivedCoreProperties 18.0: the viramas that join consonants
// into one cluster, and the consonants they join
const CONJUNCT_LINKERS: &[(u32, u32)] = &[
    (0x094d, 0x094d),
    (0x09cd, 0x09cd),
    (0x0acd, 0x0acd),
    (0x0b4d, 0x0b4d),
    (0x0c4d, 0x0c4d),
    (0x0d4d, 0x0d4d),
    (0x1039, 0x1039),
    (0x17d2, 0x17d2),
    (0x1a60, 0x1a60),
    (0x1b44, 0x1b44),
    (0x1bab, 0x1bab),
    (0x1cf5, 0x1cf6),
    (0xa9c0, 0xa9c0),
    (0xaaf6, 0xaaf6),
    (0x10a3f, 0x10a3f),
    (0x11133, 0x11133),
    (0x113d0, 0x113d0),
    (0x1193e, 0x1193e),
    (0x11a3a, 0x11a3a),
    (0x11a47, 0x11a47),
    (0x11a99, 0x11a99),
    (0x11f42, 0x11f42),
];
const CONJUNCT_CONSONANTS: &[(u32, u32)] = &[
    (0x0915, 0x0939),
    (0x0958, 0x095f),
    (0x0978, 0x097f),
    (0x0995, 0x09a8),
    (0x09aa, 0x09b0),
    (0x09b2, 0x09b2),
    (0x09b6, 0x09b9),
    (0x09dc, 0x09dd),
    (0x09df, 0x09df),
    (0x09f0, 0x09f1),
    (0x0a95, 0x0aa8),
    (0x0aaa, 0x0ab0),
    (0x0ab2, 0x0ab3),
    (0x0ab5, 0x0ab9),
    (0x0af9, 0x0af9),
    (0x0b15, 0x0b28),
    (0x0b2a, 0x0b30),
    (0x0b32, 0x0b33),
    (0x0b35, 0x0b39),
    (0x0b5c, 0x0b5d),
    (0x0b5f, 0x0b5f),
    (0x0b71, 0x0b71),
    (0x0c15, 0x0c28),
    (0x0c2a, 0x0c39),
    (0x0c58, 0x0c5a),
    (0x0d15, 0x0d3a),
    (0x1000, 0x102a),
    (0x103f, 0x103f),
    (0x1050, 0x1055),
    (0x105a, 0x105d),
    (0x1061, 0x1061),
    (0x1065, 0x1066),
    (0x106e, 0x1070),
    (0x1075, 0x1081),
    (0x108e, 0x108e),
    (0x1780, 0x17b3),
    (0x1a20, 0x1a54),
    (0x1b0b, 0x1b0c),
    (0x1b13, 0x1b33),
    (0x1b45, 0x1b4c),
    (0x1b83, 0x1ba0),
    (0x1bae, 0x1baf),
    (0x1bbb, 0x1bbd),
    (0xa989, 0xa98b),
    (0xa98f, 0xa9b2),
    (0xa9e0, 0xa9e4),
    (0xa9e7, 0xa9ef),
    (0xa9fa, 0xa9fe),
    (0xaa60, 0xaa6f),
    (0xaa71, 0xaa73),
    (0xaa7a, 0xaa7a),
    (0xaa7e, 0xaa7f),
    (0xaae0, 0xaaea),
    (0xabc0, 0xabda),
    (0x10a00, 0x10a00),
    (0x10a10, 0x10a13),
    (0x10a15, 0x10a17),
    (0x10a19, 0x10a35),
    (0x11103, 0x11126),
    (0x11144, 0x11144),
    (0x11147, 0x11147),
    (0x11380, 0x11389),
    (0x1138b, 0x1138b),
    (0x1138e, 0x1138e),
    (0x11390, 0x113b5),
    (0x11900, 0x11906),
    (0x11909, 0x11909),
    (0x1190c, 0x11913),
    (0x11915, 0x11916),
    (0x11918, 0x1192f),
    (0x11a00, 0x11a00),
    (0x11a0b, 0x11a32),
    (0x11a50, 0x11a50),
    (0x11a5c, 0x11a83),
    (0x11b0a, 0x11b0a),
    (0x11df1, 0x11df1),
    (0x11f04, 0x11f10),
    (0x11f12, 0x11f33),
];

fn in_ranges(ranges: &[(u32, u32)], point: u32) -> bool {
    ranges
        .binary_search_by(|&(first, last)| {
            if point < first {
                std::cmp::Ordering::Greater
            } else if point > last {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

fn pictographic(character: char) -> bool {
    in_ranges(EXTENDED_PICTOGRAPHIC, u32::from(character))
}

fn grapheme_class(character: char) -> Grapheme {
    let point = u32::from(character);
    match character {
        '\r' => return Grapheme::Cr,
        '\n' => return Grapheme::Lf,
        '\u{200d}' => return Grapheme::Zwj,
        _ => {}
    }
    if point < 0x20 || (0x7f..=0x9f).contains(&point) || matches!(point, 0x2028 | 0x2029) {
        return Grapheme::Control;
    }
    if in_ranges(CONJUNCT_LINKERS, point) {
        return Grapheme::Linker;
    }
    if in_ranges(CONJUNCT_CONSONANTS, point) {
        return Grapheme::Consonant;
    }
    match line_break_class(character) {
        LineBreakClass::CM | LineBreakClass::EM | LineBreakClass::VF | LineBreakClass::VI => {
            Grapheme::Extend
        }
        LineBreakClass::Zwj => Grapheme::Zwj,
        LineBreakClass::RI => Grapheme::RegionalIndicator,
        LineBreakClass::JL => Grapheme::L,
        LineBreakClass::JV => Grapheme::V,
        LineBreakClass::JT => Grapheme::T,
        LineBreakClass::H2 => Grapheme::Lv,
        LineBreakClass::H3 => Grapheme::Lvt,
        _ => Grapheme::Other,
    }
}

// the code unit offsets where grapheme clusters start, with the length of the text last
pub(crate) fn grapheme_boundaries(units: &[u16]) -> Vec<usize> {
    let characters: Vec<(usize, char)> = {
        let mut offset = 0;
        char::decode_utf16(units.iter().copied())
            .map(|decoded| {
                let character = decoded.unwrap_or(char::REPLACEMENT_CHARACTER);
                let start = offset;
                offset += character.len_utf16();
                (start, character)
            })
            .collect()
    };
    let mut boundaries = Vec::new();
    let mut previous: Option<Grapheme> = None;
    // an emoji with its extends so far, which a joiner may tie to the next emoji
    let mut emoji_run = false;
    let mut joined_emoji = false;
    let mut regional_count = 0;
    // a conjunct so far: a consonant, then extends with at least one linker
    let mut conjunct = false;
    let mut linked = false;
    for &(start, character) in &characters {
        let class = grapheme_class(character);
        let keep = match (previous, class) {
            (None, _) => false,
            (Some(Grapheme::Cr), Grapheme::Lf) => true,
            (Some(Grapheme::Cr | Grapheme::Lf | Grapheme::Control), _) => false,
            (_, Grapheme::Cr | Grapheme::Lf | Grapheme::Control) => false,
            (Some(Grapheme::L), Grapheme::L | Grapheme::V | Grapheme::Lv | Grapheme::Lvt) => true,
            (Some(Grapheme::Lv | Grapheme::V), Grapheme::V | Grapheme::T) => true,
            (Some(Grapheme::Lvt | Grapheme::T), Grapheme::T) => true,
            (_, Grapheme::Extend | Grapheme::Zwj | Grapheme::Linker) => true,
            (Some(Grapheme::Zwj), _) if joined_emoji && pictographic(character) => true,
            (Some(Grapheme::RegionalIndicator), Grapheme::RegionalIndicator) => {
                regional_count % 2 == 1
            }
            (_, Grapheme::Consonant) if conjunct && linked => true,
            _ => false,
        };
        if !keep {
            boundaries.push(start);
        }
        // follow the emoji, regional indicator and conjunct sequences the rules look back on
        if pictographic(character) {
            emoji_run = true;
            joined_emoji = false;
        } else if class == Grapheme::Extend && emoji_run {
        } else if class == Grapheme::Zwj && emoji_run {
            joined_emoji = true;
            emoji_run = false;
        } else {
            emoji_run = false;
            joined_emoji = false;
        }
        regional_count = if class == Grapheme::RegionalIndicator {
            if keep { regional_count + 1 } else { 1 }
        } else {
            0
        };
        match class {
            Grapheme::Consonant => {
                conjunct = true;
                linked = false;
            }
            Grapheme::Linker if conjunct => linked = true,
            Grapheme::Extend | Grapheme::Zwj if conjunct => {}
            _ => {
                conjunct = false;
                linked = false;
            }
        }
        previous = Some(class);
    }
    boundaries.push(units.len());
    boundaries
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WordClass {
    Letter,
    Number,
    Ideograph,
    Space,
    Newline,
    // a mark that may sit inside a word or number: an apostrophe, a period, a comma
    Mid,
    Other,
}

fn word_class(character: char) -> WordClass {
    let point = u32::from(character);
    if matches!(
        character,
        '\r' | '\n' | '\u{2028}' | '\u{2029}' | '\u{0b}' | '\u{0c}' | '\u{85}'
    ) {
        return WordClass::Newline;
    }
    if character.is_whitespace() {
        return WordClass::Space;
    }
    if matches!(point, 0x3040..=0x30ff | 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x3134f)
    {
        return WordClass::Ideograph;
    }
    if character.is_numeric() {
        return WordClass::Number;
    }
    if character.is_alphabetic() || character == '_' {
        return WordClass::Letter;
    }
    if matches!(character, '\'' | '\u{2019}' | '.' | ',' | ':' | '\u{b7}') {
        return WordClass::Mid;
    }
    WordClass::Other
}

// word segments as start and end offsets, and whether each is a word rather than space or
// punctuation; letters and digits run together, as do the marks between them
pub(crate) fn word_segments(units: &[u16]) -> Vec<(usize, usize, bool)> {
    let boundaries = grapheme_boundaries(units);
    let clusters: Vec<(usize, usize, WordClass)> = boundaries
        .windows(2)
        .map(|pair| {
            let first = char::decode_utf16(units[pair[0]..pair[1]].iter().copied())
                .next()
                .and_then(Result::ok)
                .unwrap_or(char::REPLACEMENT_CHARACTER);
            (pair[0], pair[1], word_class(first))
        })
        .collect();
    let wordy = |class: WordClass| matches!(class, WordClass::Letter | WordClass::Number);
    let mut segments = Vec::new();
    let mut index = 0;
    while index < clusters.len() {
        let (start, mut end, class) = clusters[index];
        index += 1;
        match class {
            WordClass::Letter | WordClass::Number => {
                while index < clusters.len() {
                    let next = clusters[index].2;
                    if wordy(next) {
                        end = clusters[index].1;
                        index += 1;
                        continue;
                    }
                    // a mark between two word characters stays inside the word
                    if next == WordClass::Mid
                        && clusters.get(index + 1).is_some_and(|after| wordy(after.2))
                    {
                        end = clusters[index + 1].1;
                        index += 2;
                        continue;
                    }
                    break;
                }
                segments.push((start, end, true));
            }
            WordClass::Space => {
                while index < clusters.len() && clusters[index].2 == WordClass::Space {
                    end = clusters[index].1;
                    index += 1;
                }
                segments.push((start, end, false));
            }
            WordClass::Ideograph => segments.push((start, end, true)),
            _ => segments.push((start, end, false)),
        }
    }
    segments
}

// sentence segments: each runs through its terminator, any closing marks after it and the
// space that follows; a period followed by a lowercase word does not end the sentence
pub(crate) fn sentence_segments(units: &[u16]) -> Vec<(usize, usize)> {
    let characters: Vec<(usize, char)> = {
        let mut offset = 0;
        char::decode_utf16(units.iter().copied())
            .map(|decoded| {
                let character = decoded.unwrap_or(char::REPLACEMENT_CHARACTER);
                let start = offset;
                offset += character.len_utf16();
                (start, character)
            })
            .collect()
    };
    let terminal = |character: char| {
        matches!(
            character,
            '!' | '?'
                | '\u{3002}'
                | '\u{ff01}'
                | '\u{ff1f}'
                | '\u{2026}'
                | '\u{203c}'
                | '\u{2047}'
                | '\u{2048}'
                | '\u{2049}'
        )
    };
    let closing = |character: char| {
        matches!(
            character,
            '"' | '\''
                | ')'
                | ']'
                | '}'
                | '\u{2019}'
                | '\u{201d}'
                | '\u{bb}'
                | '\u{300d}'
                | '\u{300f}'
        )
    };
    let mut segments = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < characters.len() {
        let (_, character) = characters[index];
        if matches!(character, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
            index += 1;
            if character == '\r' && characters.get(index).is_some_and(|(_, next)| *next == '\n') {
                index += 1;
            }
            let end = characters
                .get(index)
                .map_or(units.len(), |(offset, _)| *offset);
            segments.push((start, end));
            start = end;
            continue;
        }
        if terminal(character) || character == '.' {
            let mut after = index + 1;
            while after < characters.len()
                && (terminal(characters[after].1) || characters[after].1 == '.')
            {
                after += 1;
            }
            while after < characters.len() && closing(characters[after].1) {
                after += 1;
            }
            let spaced = after < characters.len() && characters[after].1.is_whitespace();
            while after < characters.len()
                && characters[after].1.is_whitespace()
                && !matches!(characters[after].1, '\n' | '\r' | '\u{2028}' | '\u{2029}')
            {
                after += 1;
            }
            // a line or paragraph break after the terminator ends the sentence with it
            let mut paragraph = false;
            if let Some(&(_, next)) = characters.get(after)
                && matches!(next, '\n' | '\r' | '\u{2028}' | '\u{2029}')
            {
                after += 1;
                if next == '\r' && characters.get(after).is_some_and(|(_, then)| *then == '\n') {
                    after += 1;
                }
                paragraph = true;
            }
            let ends = after >= characters.len()
                || paragraph
                || (spaced && (terminal(character) || !characters[after].1.is_lowercase()));
            if ends {
                let end = characters
                    .get(after)
                    .map_or(units.len(), |(offset, _)| *offset);
                segments.push((start, end));
                start = end;
            }
            index = after;
            continue;
        }
        index += 1;
    }
    if start < units.len() {
        segments.push((start, units.len()));
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pieces(text: &str, cut: impl Fn(&[u16]) -> Vec<(usize, usize)>) -> Vec<String> {
        let units: Vec<u16> = text.encode_utf16().collect();
        cut(&units)
            .into_iter()
            .map(|(start, end)| String::from_utf16_lossy(&units[start..end]))
            .collect()
    }

    fn graphemes(text: &str) -> Vec<String> {
        pieces(text, |units| {
            grapheme_boundaries(units)
                .windows(2)
                .map(|pair| (pair[0], pair[1]))
                .collect()
        })
    }

    #[test]
    fn graphemes_hold_marks_emoji_sequences_flags_and_syllables() {
        assert_eq!(graphemes("e\u{301}a"), ["e\u{301}", "a"]);
        assert_eq!(graphemes("\r\nx"), ["\r\n", "x"]);
        assert_eq!(
            graphemes("\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}!"),
            ["\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}", "!"]
        );
        assert_eq!(graphemes("\u{1f44d}\u{1f3fd}"), ["\u{1f44d}\u{1f3fd}"]);
        assert_eq!(
            graphemes("\u{1f1fa}\u{1f1f8}\u{1f1eb}\u{1f1f7}\u{1f1e9}"),
            ["\u{1f1fa}\u{1f1f8}", "\u{1f1eb}\u{1f1f7}", "\u{1f1e9}"]
        );
        assert_eq!(
            graphemes("\u{1100}\u{1161}\u{11a8}"),
            ["\u{1100}\u{1161}\u{11a8}"]
        );
        assert_eq!(
            graphemes("\u{915}\u{94d}\u{937}"),
            ["\u{915}\u{94d}\u{937}"]
        );
    }

    #[test]
    fn words_and_sentences_split_where_readers_expect() {
        let words = pieces("Can't stop, 3.14 now!", |units| {
            word_segments(units)
                .into_iter()
                .map(|(start, end, _)| (start, end))
                .collect()
        });
        assert_eq!(
            words,
            ["Can't", " ", "stop", ",", " ", "3.14", " ", "now", "!"]
        );
        let flags: Vec<bool> = word_segments(&"a b".encode_utf16().collect::<Vec<_>>())
            .into_iter()
            .map(|(_, _, word)| word)
            .collect();
        assert_eq!(flags, [true, false, true]);
        assert_eq!(
            pieces(
                "Hello there. How are you? e.g. this stays.\nNext",
                sentence_segments
            ),
            [
                "Hello there. ",
                "How are you? ",
                "e.g. this stays.\n",
                "Next"
            ]
        );
    }
}
