// line break opportunities by the Unicode line breaking algorithm (UAX 14), with the
// word-break tailorings CSS asks for

use crate::text_tables::{EAST_ASIAN_WIDE, EXTENDED_PICTOGRAPHIC, LINE_BREAK};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LineBreakClass {
    AI,
    AK,
    AL,
    AP,
    AS,
    B2,
    BA,
    BB,
    BK,
    CB,
    CJ,
    CL,
    CM,
    CP,
    CR,
    EB,
    EM,
    EX,
    GL,
    H2,
    H3,
    HH,
    HL,
    HY,
    ID,
    IN,
    IS,
    JL,
    JT,
    JV,
    LF,
    NL,
    NS,
    NU,
    OP,
    PO,
    PR,
    QU,
    RI,
    SA,
    SG,
    SP,
    SY,
    VF,
    VI,
    WJ,
    XX,
    ZW,
    Zwj,
}

use LineBreakClass::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum WordBreak {
    #[default]
    Normal,
    // any two letters may break, as for CJK
    BreakAll,
    // CJK text breaks only where other scripts would
    KeepAll,
}

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

pub(crate) fn line_break_class(character: char) -> LineBreakClass {
    let point = u32::from(character);
    LINE_BREAK
        .binary_search_by(|&(first, last, _)| {
            if point < first {
                std::cmp::Ordering::Greater
            } else if point > last {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .map_or(XX, |index| LINE_BREAK[index].2)
}

fn east_asian(character: char) -> bool {
    in_ranges(EAST_ASIAN_WIDE, u32::from(character))
}

fn pictographic(character: char) -> bool {
    in_ranges(EXTENDED_PICTOGRAPHIC, u32::from(character))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Break {
    // a line must end here, as after a newline
    Mandatory,
    Allowed,
}

// the byte offsets where a line may or must break, each before the character at that offset;
// the end of the text is not listed
pub(crate) fn break_opportunities(text: &str, word_break: WordBreak) -> Vec<(usize, Break)> {
    let characters = text.char_indices().collect::<Vec<_>>();
    // LB1: resolve the classes the algorithm leaves to the implementation
    let raw = characters
        .iter()
        .map(|&(_, character)| {
            let class = match line_break_class(character) {
                AI | SG | XX | SA => AL,
                CJ => NS,
                class => class,
            };
            match (word_break, class) {
                (WordBreak::BreakAll, AL | HL | NU | AK | AS) => ID,
                (WordBreak::KeepAll, ID | H2 | H3 | JL | JV | JT) => AL,
                _ => class,
            }
        })
        .collect::<Vec<_>>();
    // LB9 and LB10: combining marks take the class of what they attach to
    let mut classes = raw.clone();
    let mut attached = vec![false; raw.len()];
    for index in 1..raw.len() {
        if matches!(raw[index], CM | Zwj) {
            if matches!(classes[index - 1], BK | CR | LF | NL | SP | ZW) {
                classes[index] = AL;
            } else {
                classes[index] = classes[index - 1];
                attached[index] = true;
            }
        }
    }
    if matches!(classes.first(), Some(CM | Zwj)) {
        classes[0] = AL;
    }

    let mut breaks = Vec::new();
    // the regional indicators in the current run, for LB30a
    let mut regional_run = usize::from(classes.first() == Some(&RI));
    for index in 1..classes.len() {
        let before = classes[index - 1];
        let after = classes[index];
        // the last class before any spaces, for the rules that look across SP*
        let mut back = index - 1;
        while back > 0 && classes[back] == SP {
            back -= 1;
        }
        let across = classes[back];
        // the class before the pair's before, past the marks LB9 attached to it, for the
        // rules that look at a hyphen's context, or none when the text starts there
        let mut start = index - 1;
        while start > 0 && attached[start] {
            start -= 1;
        }
        let behind = start.checked_sub(1).map(|back| classes[back]);
        let spaced = before == SP;
        let decision = decide(Pair {
            before,
            after,
            across,
            behind,
            spaced,
            raw_before: raw[index - 1],
            attached: attached[index],
            before_char: characters[index - 1].1,
            after_char: characters[index].1,
            regional_run,
        });
        regional_run = if after == RI {
            if before == RI && !attached[index] {
                regional_run + 1
            } else {
                1
            }
        } else if attached[index] {
            regional_run
        } else {
            0
        };
        if let Some(kind) = decision {
            breaks.push((characters[index].0, kind));
        }
    }
    breaks
}

struct Pair {
    before: LineBreakClass,
    after: LineBreakClass,
    // the class before a run of spaces that ends at the boundary
    across: LineBreakClass,
    // the class before the pair's before, or none at the start of the text
    behind: Option<LineBreakClass>,
    spaced: bool,
    raw_before: LineBreakClass,
    attached: bool,
    before_char: char,
    after_char: char,
    regional_run: usize,
}

// the rules of UAX 14 in order; none means no break
fn decide(pair: Pair) -> Option<Break> {
    let Pair {
        before,
        after,
        across,
        behind,
        spaced,
        raw_before,
        attached,
        before_char,
        after_char,
        regional_run,
    } = pair;
    // LB4 and LB5: hard line breaks
    match before {
        BK | LF | NL => return Some(Break::Mandatory),
        CR if after == LF => return None,
        CR => return Some(Break::Mandatory),
        _ => {}
    }
    // LB6, LB7: no break before hard breaks, spaces and zero width spaces
    if matches!(after, BK | CR | LF | NL | SP | ZW) {
        return None;
    }
    // LB8: a break after a zero width space and any spaces after it
    if across == ZW {
        return Some(Break::Allowed);
    }
    // LB8a: nothing breaks after a zero width joiner
    if raw_before == Zwj {
        return None;
    }
    // LB9: combining marks stay with their base
    if attached {
        return None;
    }
    // LB11, LB12: word joiners and glue
    if after == WJ || before == WJ || before == GL {
        return None;
    }
    // LB12a
    if after == GL && !matches!(before, SP | BA | HY | HH) {
        return None;
    }
    // LB13: no break before closing punctuation, exclamations or slashes
    if matches!(after, CL | CP | EX | SY) {
        return None;
    }
    // LB14: nothing breaks after an opening bracket, even across spaces
    if across == OP {
        return None;
    }
    // LB15c, LB15d: a space may break before a decimal point that starts a number
    if after == IS {
        return if spaced { Some(Break::Allowed) } else { None };
    }
    // LB16, LB17
    if matches!(across, CL | CP) && after == NS {
        return None;
    }
    if across == B2 && after == B2 {
        return None;
    }
    // LB18: spaces allow a break after them
    if spaced {
        return Some(Break::Allowed);
    }
    // LB19: quotation marks stay with what they quote
    if after == QU || before == QU {
        return None;
    }
    // LB20: contingent breaks
    if after == CB || before == CB {
        return Some(Break::Allowed);
    }
    // LB20a: a hyphen at the start of a word stays with the letter after it
    let word_initial = matches!(before, HY | HH)
        && matches!(after, AL | HL)
        && behind.is_none_or(|class| matches!(class, BK | CR | LF | NL | SP | ZW | CB | GL));
    if word_initial {
        return None;
    }
    // LB21, LB21b
    if matches!(after, BA | HH | HY | NS) || before == BB {
        return None;
    }
    if before == SY && after == HL {
        return None;
    }
    // LB21a: a hyphen after a Hebrew letter stays with what follows it
    if matches!(before, HY | HH) && after != HL && behind == Some(HL) {
        return None;
    }
    // LB22: no break before ellipses
    if after == IN {
        return None;
    }
    // LB23 to LB27: letters, numbers and their prefixes and suffixes stay together
    let letter = |class| matches!(class, AL | HL);
    let korean = |class| matches!(class, JL | JV | JT | H2 | H3);
    if (letter(before) && after == NU) || (before == NU && letter(after)) {
        return None;
    }
    if (before == PR && matches!(after, ID | EB | EM))
        || (matches!(before, ID | EB | EM) && after == PO)
    {
        return None;
    }
    if (matches!(before, PR | PO) && letter(after)) || (letter(before) && matches!(after, PR | PO))
    {
        return None;
    }
    if (matches!(before, CL | CP | NU) && matches!(after, PO | PR))
        || (matches!(before, PO | PR) && matches!(after, OP | NU))
        || (matches!(before, HY | IS | NU | SY) && after == NU)
    {
        return None;
    }
    if (before == JL && matches!(after, JL | JV | H2 | H3))
        || (matches!(before, JV | H2) && matches!(after, JV | JT))
        || (matches!(before, JT | H3) && after == JT)
    {
        return None;
    }
    if (korean(before) && after == PO) || (before == PR && korean(after)) {
        return None;
    }
    // LB28: letters stay together
    if letter(before) && letter(after) {
        return None;
    }
    // LB28a: Brahmic orthographic syllables
    if (before == AP && matches!(after, AK | AS))
        || (matches!(before, AK | AS) && matches!(after, VF | VI))
        || (before == VI && matches!(after, AK | AS))
    {
        return None;
    }
    // LB29
    if before == IS && letter(after) {
        return None;
    }
    // LB30: brackets that are not East Asian stay with the letters and digits next to them
    if (matches!(before, AL | HL | NU) && after == OP && !east_asian(after_char))
        || (before == CP && !east_asian(before_char) && matches!(after, AL | HL | NU))
    {
        return None;
    }
    // LB30a: regional indicators pair into flags
    if before == RI && after == RI && regional_run % 2 == 1 {
        return None;
    }
    // LB30b: emoji modifiers stay with their base
    if after == EM && (before == EB || (pictographic(before_char) && before == ID)) {
        return None;
    }
    // LB31
    Some(Break::Allowed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn breaks(text: &str) -> Vec<usize> {
        break_opportunities(text, WordBreak::Normal)
            .into_iter()
            .map(|(offset, _)| offset)
            .collect()
    }

    #[test]
    fn breaks_after_spaces_hyphens_and_ideographs_and_not_inside_words() {
        assert_eq!(breaks("hello world"), vec![6]);
        assert_eq!(breaks("well-known"), vec![5]);
        assert_eq!(breaks("(a) b"), vec![4]);
        assert_eq!(breaks("x 100% y"), vec![2, 7]);
        // every ideograph may start a line
        assert_eq!(breaks("\u{4E00}\u{4E01}\u{4E02}"), vec![3, 6]);
        // no break before closing punctuation
        assert_eq!(breaks("\u{4E00}\u{3002}"), Vec::<usize>::new());
        assert_eq!(
            break_opportunities("a\nb", WordBreak::Normal),
            vec![(2, Break::Mandatory)]
        );
        // a flag of two regional indicators stays whole
        assert_eq!(breaks("\u{1F1EF}\u{1F1F5}\u{1F1FA}\u{1F1F8}"), vec![8]);
        assert_eq!(break_opportunities("ab", WordBreak::BreakAll).len(), 1);
        assert!(break_opportunities("\u{4E00}\u{4E01}", WordBreak::KeepAll).is_empty());
        // a url may break after its slashes, not inside its words
        assert_eq!(breaks("https://example.com/a/b"), vec![8, 20, 22]);
    }

    #[test]
    fn unicode_17_hyphen_rules_hold() {
        // a word-initial hyphen stays with the letter after it, as with the suffix -ing
        assert_eq!(breaks("free -ing"), vec![5]);
        assert_eq!(breaks("well-known"), vec![5]);
        // every HH hyphen takes the word-initial rule, figure dash included in unicode 17
        assert_eq!(breaks("free \u{2012}ing"), vec![5]);
        // marks attached to the hyphen do not hide what precedes it
        assert_eq!(breaks("free -\u{0301}ing"), vec![5]);
        assert_eq!(breaks("\u{05D0}\u{05BE}\u{0301}x y"), vec![8]);
        // a hyphen after a Hebrew letter stays with what follows it
        assert_eq!(breaks("\u{05D0}\u{05D1}\u{05BE}x y"), vec![8]);
        // while a maqaf between hebrew letters may still break after itself
        assert_eq!(breaks("\u{05D0}\u{05D1}\u{05BE}\u{05D2}\u{05D3}"), vec![6]);
    }
}
