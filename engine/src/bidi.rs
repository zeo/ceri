// embedding levels and visual order by the Unicode bidirectional algorithm (UAX 9)

use crate::text_tables::{BIDI_BRACKETS, BIDI_CLASS};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BidiClass {
    L,
    R,
    AL,
    EN,
    ES,
    ET,
    AN,
    CS,
    Nsm,
    BN,
    B,
    S,
    WS,
    ON,
    Lre,
    Lro,
    Rle,
    Rlo,
    Pdf,
    Lri,
    Rli,
    Fsi,
    Pdi,
}

use BidiClass::*;

pub(crate) fn bidi_class(character: char) -> BidiClass {
    let point = u32::from(character);
    BIDI_CLASS
        .binary_search_by(|&(first, last, _)| {
            if point < first {
                std::cmp::Ordering::Greater
            } else if point > last {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .map_or(L, |index| BIDI_CLASS[index].2)
}

// the bracket a bracket pairs with, and whether it opens
fn bracket(character: char) -> Option<(char, bool)> {
    let point = u32::from(character);
    BIDI_BRACKETS
        .binary_search_by_key(&point, |&(bracket, _, _)| bracket)
        .ok()
        .and_then(|index| {
            let (_, pair, opens) = BIDI_BRACKETS[index];
            char::from_u32(pair).map(|pair| (pair, opens))
        })
}

// the glyph shown for a bracket on a right-to-left level
pub(crate) fn mirrored(character: char) -> char {
    bracket(character).map_or(character, |(pair, _)| pair)
}

const MAX_DEPTH: u8 = 125;

fn is_isolate_initiator(class: BidiClass) -> bool {
    matches!(class, Lri | Rli | Fsi)
}

fn removed_by_x9(class: BidiClass) -> bool {
    matches!(class, Rle | Lre | Rlo | Lro | Pdf | BN)
}

// the first strong direction in a range, skipping isolates (P2), as a level
fn first_strong(classes: &[BidiClass]) -> Option<u8> {
    let mut depth = 0_usize;
    for &class in classes {
        match class {
            Lri | Rli | Fsi => depth += 1,
            Pdi if depth > 0 => depth -= 1,
            Pdi => {}
            L if depth == 0 => return Some(0),
            R | AL if depth == 0 => return Some(1),
            _ => {}
        }
    }
    None
}

// the paragraph's embedding level and each character's resolved level; base is the paragraph
// level, or none to take it from the first strong character
pub(crate) fn resolve_levels(text: &str, base: Option<u8>) -> (u8, Vec<u8>) {
    let characters = text.chars().collect::<Vec<_>>();
    let original = characters
        .iter()
        .map(|&character| bidi_class(character))
        .collect::<Vec<_>>();
    let paragraph = base.unwrap_or_else(|| first_strong(&original).unwrap_or(0));
    if !original
        .iter()
        .any(|class| matches!(class, R | AL | AN | Rle | Rlo | Rli | Lre | Lro | Lri | Fsi))
        && paragraph == 0
    {
        return (0, vec![0; characters.len()]);
    }
    let count = characters.len();
    let mut classes = original.clone();
    let mut levels = vec![paragraph; count];

    // X1 to X8: explicit embeddings, overrides and isolates
    #[derive(Clone, Copy)]
    struct Status {
        level: u8,
        override_class: Option<BidiClass>,
        isolate: bool,
    }
    let mut stack = vec![Status {
        level: paragraph,
        override_class: None,
        isolate: false,
    }];
    let mut overflow_isolates = 0_usize;
    let mut overflow_embeddings = 0_usize;
    let mut valid_isolates = 0_usize;
    let next_level = |level: u8, rtl: bool| {
        if rtl {
            (level + 1) | 1
        } else {
            (level + 2) & !1
        }
    };
    for index in 0..count {
        let class = original[index];
        let top = *stack.last().expect("the stack keeps its base entry");
        match class {
            Rle | Lre | Rlo | Lro | Rli | Lri | Fsi => {
                let isolate = is_isolate_initiator(class);
                let rtl = match class {
                    Rle | Rlo | Rli => true,
                    Fsi => {
                        let end = matching_pdi(&original, index);
                        first_strong(&original[index + 1..end]) == Some(1)
                    }
                    _ => false,
                };
                levels[index] = top.level;
                if isolate && let Some(forced) = top.override_class {
                    classes[index] = forced;
                }
                let level = next_level(top.level, rtl);
                if level <= MAX_DEPTH && overflow_isolates == 0 && overflow_embeddings == 0 {
                    if isolate {
                        valid_isolates += 1;
                    }
                    stack.push(Status {
                        level,
                        override_class: match class {
                            Rlo => Some(R),
                            Lro => Some(L),
                            _ => None,
                        },
                        isolate,
                    });
                } else if isolate {
                    overflow_isolates += 1;
                } else if overflow_isolates == 0 {
                    overflow_embeddings += 1;
                }
            }
            Pdi => {
                if overflow_isolates > 0 {
                    overflow_isolates -= 1;
                } else if valid_isolates > 0 {
                    overflow_embeddings = 0;
                    while stack.last().is_some_and(|status| !status.isolate) {
                        stack.pop();
                    }
                    stack.pop();
                    valid_isolates -= 1;
                }
                let top = *stack.last().expect("the stack keeps its base entry");
                levels[index] = top.level;
                if let Some(forced) = top.override_class {
                    classes[index] = forced;
                }
            }
            Pdf => {
                if overflow_isolates == 0 {
                    if overflow_embeddings > 0 {
                        overflow_embeddings -= 1;
                    } else if !top.isolate && stack.len() > 1 {
                        stack.pop();
                    }
                }
                levels[index] = top.level;
            }
            B => levels[index] = paragraph,
            BN => levels[index] = top.level,
            _ => {
                levels[index] = top.level;
                if let Some(forced) = top.override_class {
                    classes[index] = forced;
                }
            }
        }
    }

    // X10: isolating run sequences, from level runs joined across matched isolates
    let kept = (0..count)
        .filter(|&index| !removed_by_x9(original[index]))
        .collect::<Vec<_>>();
    let mut runs: Vec<Vec<usize>> = Vec::new();
    for &index in &kept {
        match runs.last_mut() {
            Some(run) if levels[*run.last().expect("runs are not empty")] == levels[index] => {
                run.push(index)
            }
            _ => runs.push(vec![index]),
        }
    }
    let mut sequences: Vec<Vec<usize>> = Vec::new();
    let mut consumed = vec![false; runs.len()];
    for start in 0..runs.len() {
        if consumed[start] {
            continue;
        }
        let first = runs[start][0];
        // a run starting at a Pdi that closes an isolate continues its opener's sequence
        if original[first] == Pdi && matching_initiator(&original, first).is_some() {
            continue;
        }
        let mut sequence = Vec::new();
        let mut current = start;
        loop {
            consumed[current] = true;
            sequence.extend_from_slice(&runs[current]);
            let last = *runs[current].last().expect("runs are not empty");
            if !is_isolate_initiator(original[last]) {
                break;
            }
            let end = matching_pdi(&original, last);
            let Some(next) = (end < count)
                .then(|| runs.iter().position(|run| run[0] == end))
                .flatten()
            else {
                break;
            };
            current = next;
        }
        sequences.push(sequence);
    }

    for sequence in &sequences {
        resolve_sequence(
            sequence,
            &characters,
            &original,
            &mut classes,
            &mut levels,
            paragraph,
        );
    }
    // removed characters take the level of what precedes them, for L2
    for index in 0..count {
        if removed_by_x9(original[index]) {
            levels[index] = if index > 0 {
                levels[index - 1]
            } else {
                paragraph
            };
        }
    }
    // L1: separators and trailing whitespace go back to the paragraph level
    let mut trailing = true;
    for index in (0..count).rev() {
        match original[index] {
            B | S => {
                levels[index] = paragraph;
                trailing = true;
            }
            WS | Fsi | Lri | Rli | Pdi | BN | Lre | Rle | Lro | Rlo | Pdf if trailing => {
                levels[index] = paragraph;
            }
            _ => trailing = false,
        }
    }
    (paragraph, levels)
}

fn matching_pdi(classes: &[BidiClass], initiator: usize) -> usize {
    let mut depth = 1_usize;
    for (index, &class) in classes.iter().enumerate().skip(initiator + 1) {
        match class {
            Lri | Rli | Fsi => depth += 1,
            Pdi => {
                depth -= 1;
                if depth == 0 {
                    return index;
                }
            }
            B => break,
            _ => {}
        }
    }
    classes.len()
}

fn matching_initiator(classes: &[BidiClass], pdi: usize) -> Option<usize> {
    let mut depth = 1_usize;
    for index in (0..pdi).rev() {
        match classes[index] {
            Pdi => depth += 1,
            Lri | Rli | Fsi => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

// W1 to W7, N0 to N2 and I1 to I2 over one isolating run sequence
fn resolve_sequence(
    sequence: &[usize],
    characters: &[char],
    original: &[BidiClass],
    classes: &mut [BidiClass],
    levels: &mut [u8],
    paragraph: u8,
) {
    let level = levels[sequence[0]];
    let direction_of = |level: u8| if level.is_multiple_of(2) { L } else { R };
    // sos and eos come from the higher of the neighbouring levels
    let first = sequence[0];
    let last = *sequence.last().expect("sequences are not empty");
    let before = (0..first)
        .rev()
        .find(|&index| !removed_by_x9(original[index]))
        .map_or(paragraph, |index| levels[index]);
    let after = if is_isolate_initiator(original[last]) {
        paragraph
    } else {
        (last + 1..original.len())
            .find(|&index| !removed_by_x9(original[index]))
            .map_or(paragraph, |index| levels[index])
    };
    let sos = direction_of(level.max(before));
    let eos = direction_of(level.max(after));
    let mut types = sequence
        .iter()
        .map(|&index| classes[index])
        .collect::<Vec<_>>();
    let count = types.len();

    // W1: marks take the type before them
    for position in 0..count {
        if types[position] == Nsm {
            types[position] = if position == 0 {
                sos
            } else if matches!(types[position - 1], Lri | Rli | Fsi | Pdi) {
                ON
            } else {
                types[position - 1]
            };
        }
    }
    // W2, W3: European numbers after Arabic letters are Arabic numbers; AL becomes R
    let mut last_strong = sos;
    for kind in types.iter_mut() {
        match *kind {
            L | R | AL => last_strong = *kind,
            EN if last_strong == AL => *kind = AN,
            _ => {}
        }
    }
    for kind in types.iter_mut() {
        if *kind == AL {
            *kind = R;
        }
    }
    // W4: a single separator between two numbers of a kind joins them
    for position in 1..count.saturating_sub(1) {
        let (left, right) = (types[position - 1], types[position + 1]);
        match types[position] {
            ES if left == EN && right == EN => types[position] = EN,
            CS if left == right && matches!(left, EN | AN) => types[position] = left,
            _ => {}
        }
    }
    // W5: terminators next to European numbers become numbers
    let mut position = 0;
    while position < count {
        if types[position] == ET {
            let start = position;
            while position < count && types[position] == ET {
                position += 1;
            }
            let touches = (start > 0 && types[start - 1] == EN)
                || (position < count && types[position] == EN);
            if touches {
                types[start..position].fill(EN);
            }
        } else {
            position += 1;
        }
    }
    // W6: what separators remain are neutral
    for kind in types.iter_mut() {
        if matches!(*kind, ES | ET | CS) {
            *kind = ON;
        }
    }
    // W7: European numbers in left-to-right context are left-to-right
    let mut last_strong = sos;
    for kind in types.iter_mut() {
        match *kind {
            L | R => last_strong = *kind,
            EN if last_strong == L => *kind = L,
            _ => {}
        }
    }

    // N0: paired brackets take the direction found inside them, or around them
    let strong_of = |kind: BidiClass| match kind {
        L => Some(L),
        R | AN | EN => Some(R),
        _ => None,
    };
    let embedding = direction_of(level);
    let mut openers: Vec<(char, usize)> = Vec::new();
    let mut pairs = Vec::new();
    for (position, &index) in sequence.iter().enumerate() {
        if types[position] != ON {
            continue;
        }
        let Some((pair, opens)) = bracket(characters[index]) else {
            continue;
        };
        if opens {
            if openers.len() == 63 {
                break;
            }
            openers.push((pair, position));
        } else if let Some(depth) = openers
            .iter()
            .rposition(|&(expected, _)| expected == characters[index])
        {
            pairs.push((openers[depth].1, position));
            openers.truncate(depth);
        }
    }
    pairs.sort_unstable();
    for (open, close) in pairs {
        let inside = types[open + 1..close]
            .iter()
            .filter_map(|&kind| strong_of(kind))
            .collect::<Vec<_>>();
        let resolved = if inside.contains(&embedding) {
            Some(embedding)
        } else if let Some(&opposite) = inside.first() {
            let context = types[..open]
                .iter()
                .rev()
                .find_map(|&kind| strong_of(kind))
                .unwrap_or(sos);
            Some(if context == opposite {
                opposite
            } else {
                embedding
            })
        } else {
            None
        };
        if let Some(direction) = resolved {
            types[open] = direction;
            types[close] = direction;
            // marks after a bracket follow it
            for position in [open, close] {
                let mut next = position + 1;
                while next < count && original[sequence[next]] == Nsm {
                    types[next] = direction;
                    next += 1;
                }
            }
        }
    }

    // N1, N2: neutrals between like directions take it, the rest take the embedding
    let mut position = 0;
    while position < count {
        if matches!(types[position], B | S | WS | ON | Lri | Rli | Fsi | Pdi) {
            let start = position;
            while position < count
                && matches!(types[position], B | S | WS | ON | Lri | Rli | Fsi | Pdi)
            {
                position += 1;
            }
            let leading = if start == 0 {
                sos
            } else {
                strong_of(types[start - 1]).unwrap_or(embedding)
            };
            let trailing = if position == count {
                eos
            } else {
                strong_of(types[position]).unwrap_or(embedding)
            };
            let direction = if leading == trailing {
                leading
            } else {
                embedding
            };
            types[start..position].fill(direction);
        } else {
            position += 1;
        }
    }

    // I1, I2: implicit levels
    for (position, &index) in sequence.iter().enumerate() {
        let current = levels[index];
        levels[index] = match (current.is_multiple_of(2), types[position]) {
            (true, R) => current + 1,
            (true, AN | EN) => current + 2,
            (false, L | EN | AN) => current + 1,
            _ => current,
        };
        classes[index] = types[position];
    }
}

// L2: the visual order of items on a line, given each item's level
pub(crate) fn visual_order(levels: &[u8]) -> Vec<usize> {
    let mut order = (0..levels.len()).collect::<Vec<_>>();
    let Some(&highest) = levels.iter().max() else {
        return order;
    };
    let lowest_odd = levels
        .iter()
        .copied()
        .filter(|level| !level.is_multiple_of(2))
        .min()
        .unwrap_or(highest + 1);
    let mut level = highest;
    while level >= lowest_odd && level > 0 {
        let mut position = 0;
        while position < order.len() {
            if levels[order[position]] >= level {
                let start = position;
                while position < order.len() && levels[order[position]] >= level {
                    position += 1;
                }
                order[start..position].reverse();
            } else {
                position += 1;
            }
        }
        level -= 1;
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_levels_for_mixed_text_numbers_and_brackets() {
        let (paragraph, levels) = resolve_levels("abc", None);
        assert_eq!((paragraph, levels), (0, vec![0, 0, 0]));
        // hebrew in a left-to-right paragraph rises one level, with the space between
        let (_, levels) = resolve_levels("a \u{5D0}\u{5D1}", Some(0));
        assert_eq!(levels, vec![0, 0, 1, 1]);
        // an rtl paragraph found from its first strong letter, with digits two levels up
        let (paragraph, levels) = resolve_levels("\u{5D0} 12", None);
        assert_eq!(paragraph, 1);
        assert_eq!(levels, vec![1, 1, 2, 2]);
        // brackets around rtl text in an rtl paragraph stay rtl
        let (_, levels) = resolve_levels("\u{5D0}(\u{5D1})", Some(1));
        assert_eq!(levels, vec![1, 1, 1, 1]);
        assert_eq!(visual_order(&[0, 0, 1, 1]), vec![0, 1, 3, 2]);
        assert_eq!(visual_order(&[1, 2, 2, 1]), vec![3, 1, 2, 0]);
        assert_eq!(mirrored('('), ')');
    }
}
