// word segmentation over UTF-16 text, shared by multi-click selection and
// word-wise caret movement in prose and text controls

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Word,
    Ideograph,
    Space,
    Other,
}

fn class_of(character: char) -> Class {
    if character.is_whitespace() {
        Class::Space
    } else if is_ideograph(character) {
        Class::Ideograph
    } else if character.is_alphanumeric() || character == '_' {
        Class::Word
    } else {
        Class::Other
    }
}

fn is_ideograph(character: char) -> bool {
    matches!(
        character as u32,
        0x3040..=0x30ff | 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xac00..=0xd7af | 0xf900..=0xfaff
            | 0x20000..=0x3134f
    )
}

// characters with their UTF-16 start offsets, so callers keep code unit positions
fn characters(units: &[u16]) -> Vec<(usize, char)> {
    let mut offset = 0;
    char::decode_utf16(units.iter().copied())
        .map(|decoded| {
            let character = decoded.unwrap_or(char::REPLACEMENT_CHARACTER);
            let start = offset;
            offset += character.len_utf16();
            (start, character)
        })
        .collect()
}

// an apostrophe between two word characters stays inside the word
fn classes(characters: &[(usize, char)]) -> Vec<Class> {
    let mut classes: Vec<Class> = characters.iter().map(|(_, c)| class_of(*c)).collect();
    for index in 1..characters.len().saturating_sub(1) {
        if matches!(characters[index].1, '\'' | '\u{2019}')
            && classes[index - 1] == Class::Word
            && classes[index + 1] == Class::Word
        {
            classes[index] = Class::Word;
        }
    }
    classes
}

// the segment around an offset: a word, one ideograph, a whitespace run or a punctuation run
pub(crate) fn word_at(units: &[u16], offset: usize) -> (usize, usize) {
    let characters = characters(units);
    if characters.is_empty() {
        return (0, 0);
    }
    let classes = classes(&characters);
    let mut index = characters
        .iter()
        .rposition(|(start, _)| *start <= offset)
        .unwrap_or(0);
    // a point at the end of a word belongs to that word rather than the gap after it
    if characters[index].0 == offset
        && index > 0
        && classes[index] != Class::Word
        && classes[index - 1] == Class::Word
    {
        index -= 1;
    }
    let class = classes[index];
    if class == Class::Ideograph {
        let (start, character) = characters[index];
        return (start, start + character.len_utf16());
    }
    let mut first = index;
    while first > 0 && classes[first - 1] == class {
        first -= 1;
    }
    let mut last = index;
    while last + 1 < characters.len() && classes[last + 1] == class {
        last += 1;
    }
    let (end_start, end_character) = characters[last];
    (characters[first].0, end_start + end_character.len_utf16())
}

// the next stop to the right: past the current word and any whitespace after it
pub(crate) fn next_word_boundary(units: &[u16], offset: usize) -> usize {
    let characters = characters(units);
    let classes = classes(&characters);
    let mut index = characters
        .iter()
        .position(|(start, _)| *start >= offset)
        .unwrap_or(characters.len());
    if index < characters.len() && classes[index] != Class::Space {
        let class = classes[index];
        index += 1;
        if class != Class::Ideograph {
            while index < characters.len() && classes[index] == class {
                index += 1;
            }
        }
    }
    while index < characters.len() && classes[index] == Class::Space {
        index += 1;
    }
    characters
        .get(index)
        .map(|(start, _)| *start)
        .unwrap_or(units.len())
}

// the previous stop to the left: back over whitespace, then to the start of the word before it
pub(crate) fn previous_word_boundary(units: &[u16], offset: usize) -> usize {
    let characters = characters(units);
    let classes = classes(&characters);
    let mut index = characters
        .iter()
        .rposition(|(start, _)| *start < offset)
        .map(|index| index + 1)
        .unwrap_or(0);
    while index > 0 && classes[index - 1] == Class::Space {
        index -= 1;
    }
    if index > 0 {
        let class = classes[index - 1];
        index -= 1;
        if class != Class::Ideograph {
            while index > 0 && classes[index - 1] == class {
                index -= 1;
            }
        }
    }
    characters.get(index).map(|(start, _)| *start).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(value: &str) -> Vec<u16> {
        value.encode_utf16().collect()
    }

    #[test]
    fn words_cover_letters_digits_and_inner_apostrophes() {
        let text = units("it's 42 apples, ok");
        assert_eq!(word_at(&text, 1), (0, 4));
        assert_eq!(word_at(&text, 5), (5, 7));
        assert_eq!(word_at(&text, 14), (8, 14));
        assert_eq!(word_at(&text, 4), (0, 4));
        assert_eq!(word_at(&text, 15), (15, 16));
    }

    #[test]
    fn ideographs_are_single_words_and_surrogates_stay_whole() {
        let text = units("漢字 a😀b");
        assert_eq!(word_at(&text, 0), (0, 1));
        assert_eq!(word_at(&text, 1), (1, 2));
        assert_eq!(word_at(&text, 4), (3, 4));
        assert_eq!(word_at(&text, 5), (4, 6));
    }

    #[test]
    fn word_boundaries_step_over_words_and_whitespace() {
        let text = units("one  two, three");
        assert_eq!(next_word_boundary(&text, 0), 5);
        assert_eq!(next_word_boundary(&text, 5), 8);
        assert_eq!(next_word_boundary(&text, 8), 10);
        assert_eq!(next_word_boundary(&text, 10), 15);
        assert_eq!(previous_word_boundary(&text, 15), 10);
        assert_eq!(previous_word_boundary(&text, 10), 8);
        assert_eq!(previous_word_boundary(&text, 7), 5);
        assert_eq!(previous_word_boundary(&text, 5), 0);
        assert_eq!(previous_word_boundary(&text, 0), 0);
    }
}
