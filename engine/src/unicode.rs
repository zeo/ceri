// unicode normalization forms (UAX #15) over the generated character database tables

use crate::unicode_tables::{COMBINING_CLASSES, COMPOSITIONS, DECOMPOSED, DECOMPOSITIONS};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NormalizationForm {
    Nfc,
    Nfd,
    Nfkc,
    Nfkd,
}

impl NormalizationForm {
    pub(crate) fn parse(name: &str) -> Option<Self> {
        match name {
            "NFC" => Some(Self::Nfc),
            "NFD" => Some(Self::Nfd),
            "NFKC" => Some(Self::Nfkc),
            "NFKD" => Some(Self::Nfkd),
            _ => None,
        }
    }

    fn compatibility(self) -> bool {
        matches!(self, Self::Nfkc | Self::Nfkd)
    }

    fn composes(self) -> bool {
        matches!(self, Self::Nfc | Self::Nfkc)
    }
}

// hangul syllables decompose and compose arithmetically, as section 3.12 of the standard gives
const S_BASE: u32 = 0xAC00;
const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11A7;
const L_COUNT: u32 = 19;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
const N_COUNT: u32 = V_COUNT * T_COUNT;
const S_COUNT: u32 = L_COUNT * N_COUNT;

pub(crate) fn combining_class(character: char) -> u8 {
    let point = u32::from(character);
    if point < 0x300 {
        return 0;
    }
    COMBINING_CLASSES
        .binary_search_by(|&(first, last, _)| {
            if last < point {
                std::cmp::Ordering::Less
            } else if first > point {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .map_or(0, |index| COMBINING_CLASSES[index].2)
}

pub(crate) fn normalize(text: &str, form: NormalizationForm) -> String {
    // every form leaves ASCII unchanged
    if text.bytes().all(|byte| byte < 0x80) {
        return text.to_string();
    }
    let mut characters = Vec::with_capacity(text.len());
    for character in text.chars() {
        decompose_into(character, form.compatibility(), &mut characters);
    }
    order_canonically(&mut characters);
    if form.composes() {
        compose(&mut characters);
    }
    characters.into_iter().collect()
}

fn decompose_into(character: char, compatibility: bool, output: &mut Vec<char>) {
    let point = u32::from(character);
    if (S_BASE..S_BASE + S_COUNT).contains(&point) {
        let index = point - S_BASE;
        output.push(jamo(L_BASE + index / N_COUNT));
        output.push(jamo(V_BASE + (index % N_COUNT) / T_COUNT));
        if !index.is_multiple_of(T_COUNT) {
            output.push(jamo(T_BASE + index % T_COUNT));
        }
        return;
    }
    let Ok(index) = DECOMPOSITIONS.binary_search_by_key(&point, |entry| entry.0) else {
        output.push(character);
        return;
    };
    let (_, canonical, compatible) = DECOMPOSITIONS[index];
    let (start, length) = if compatibility { compatible } else { canonical };
    if length == 0 {
        output.push(character);
        return;
    }
    output.extend(
        DECOMPOSED[usize::from(start)..usize::from(start) + usize::from(length)]
            .iter()
            .map(|&point| char::from_u32(point).expect("decomposition tables hold scalar values")),
    );
}

fn jamo(point: u32) -> char {
    char::from_u32(point).expect("jamo are scalar values")
}

// a stable sort of each run of non-starters by combining class
fn order_canonically(characters: &mut [char]) {
    let mut start = 0;
    while start < characters.len() {
        if combining_class(characters[start]) == 0 {
            start += 1;
            continue;
        }
        let end = characters[start..]
            .iter()
            .position(|&character| combining_class(character) == 0)
            .map_or(characters.len(), |offset| start + offset);
        characters[start..end].sort_by_key(|&character| combining_class(character));
        start = end;
    }
}

fn compose(characters: &mut Vec<char>) {
    let mut output: Vec<char> = Vec::with_capacity(characters.len());
    let mut starter = None;
    // the class of the last character after the starter, none while the starter is last
    let mut last_class: Option<u8> = None;
    for &character in characters.iter() {
        let class = combining_class(character);
        if let Some(position) = starter
            && last_class.is_none_or(|last| last < class)
            && let Some(composite) = compose_pair(output[position], character)
        {
            output[position] = composite;
            continue;
        }
        if class == 0 {
            starter = Some(output.len());
            last_class = None;
        } else {
            last_class = Some(class);
        }
        output.push(character);
    }
    *characters = output;
}

fn compose_pair(first: char, second: char) -> Option<char> {
    let (first, second) = (u32::from(first), u32::from(second));
    if (L_BASE..L_BASE + L_COUNT).contains(&first) && (V_BASE..V_BASE + V_COUNT).contains(&second) {
        let index = (first - L_BASE) * N_COUNT + (second - V_BASE) * T_COUNT;
        return char::from_u32(S_BASE + index);
    }
    if (S_BASE..S_BASE + S_COUNT).contains(&first)
        && (first - S_BASE).is_multiple_of(T_COUNT)
        && (T_BASE + 1..T_BASE + T_COUNT).contains(&second)
    {
        return char::from_u32(first + second - T_BASE);
    }
    COMPOSITIONS
        .binary_search_by_key(&(first, second), |&(a, b, _)| (a, b))
        .ok()
        .and_then(|index| char::from_u32(COMPOSITIONS[index].2))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(text: &str) -> [String; 4] {
        [
            NormalizationForm::Nfc,
            NormalizationForm::Nfd,
            NormalizationForm::Nfkc,
            NormalizationForm::Nfkd,
        ]
        .map(|form| normalize(text, form))
    }

    #[test]
    fn decomposes_and_recomposes_latin_and_hangul() {
        assert_eq!(
            all("caf\u{e9}"),
            ["caf\u{e9}", "cafe\u{301}", "caf\u{e9}", "cafe\u{301}"].map(String::from)
        );
        assert_eq!(
            normalize("cafe\u{301}", NormalizationForm::Nfc),
            "caf\u{e9}"
        );
        assert_eq!(
            normalize("\u{d55c}", NormalizationForm::Nfd),
            "\u{1112}\u{1161}\u{11ab}"
        );
        assert_eq!(
            normalize("\u{1112}\u{1161}\u{11ab}", NormalizationForm::Nfc),
            "\u{d55c}"
        );
    }

    #[test]
    fn orders_marks_and_keeps_exclusions_decomposed() {
        // the standard's example: dot above and dot below reorder, then the dot below composes
        assert_eq!(
            normalize("\u{1e0b}\u{323}", NormalizationForm::Nfc),
            "\u{1e0d}\u{307}"
        );
        assert_eq!(
            normalize("a\u{307}\u{323}", NormalizationForm::Nfd),
            "a\u{323}\u{307}"
        );
        assert_eq!(
            normalize("\u{958}", NormalizationForm::Nfc),
            "\u{915}\u{93c}"
        );
        assert_eq!(normalize("\u{212b}", NormalizationForm::Nfc), "\u{c5}");
        assert_eq!(combining_class('\u{301}'), 230);
        assert_eq!(combining_class('a'), 0);
    }

    #[test]
    fn compatibility_forms_fold_presentation_characters() {
        assert_eq!(
            all("\u{fb01}"),
            ["\u{fb01}", "\u{fb01}", "fi", "fi"].map(String::from)
        );
        assert_eq!(normalize("\u{2460}", NormalizationForm::Nfkc), "1");
        assert_eq!(
            normalize("\u{1e9b}\u{323}", NormalizationForm::Nfkc),
            "\u{1e69}"
        );
    }
}
