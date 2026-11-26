// an engine-owned regular expression implementation: a parser for the ECMAScript pattern
// grammar (with the web-compatibility extensions outside unicode mode), a compiler to a small
// instruction set, and a backtracking matcher with an explicit stack and a step limit so a
// pathological pattern fails instead of hanging the page; subjects are UTF-16 code units

use std::rc::Rc;

const MAX_STEPS: u64 = 20_000_000;
const MAX_PROGRAM: usize = 200_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Flags {
    pub(super) global: bool,
    pub(super) ignore_case: bool,
    pub(super) multiline: bool,
    pub(super) dot_all: bool,
    pub(super) unicode: bool,
    pub(super) unicode_sets: bool,
    pub(super) sticky: bool,
    pub(super) has_indices: bool,
}

impl Flags {
    pub(super) fn parse(text: &str) -> Result<Self, String> {
        let mut flags = Flags::default();
        for character in text.chars() {
            let slot = match character {
                'g' => &mut flags.global,
                'i' => &mut flags.ignore_case,
                'm' => &mut flags.multiline,
                's' => &mut flags.dot_all,
                'u' => &mut flags.unicode,
                'v' => &mut flags.unicode_sets,
                'y' => &mut flags.sticky,
                'd' => &mut flags.has_indices,
                other => return Err(format!("Invalid regular expression flags '{other}'")),
            };
            if *slot {
                return Err(format!("Invalid regular expression flags '{text}'"));
            }
            *slot = true;
        }
        if flags.unicode && flags.unicode_sets {
            return Err(format!("Invalid regular expression flags '{text}'"));
        }
        Ok(flags)
    }

    // the canonical flags string, in the order the flags getter reports them
    pub(super) fn text(&self) -> String {
        let mut text = String::new();
        for (on, letter) in [
            (self.has_indices, 'd'),
            (self.global, 'g'),
            (self.ignore_case, 'i'),
            (self.multiline, 'm'),
            (self.dot_all, 's'),
            (self.unicode, 'u'),
            (self.unicode_sets, 'v'),
            (self.sticky, 'y'),
        ] {
            if on {
                text.push(letter);
            }
        }
        text
    }

    fn full_unicode(&self) -> bool {
        self.unicode || self.unicode_sets
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Property {
    Letter,
    Uppercase,
    Lowercase,
    Number,
    Digit,
    Alphabetic,
    WhiteSpace,
    Punctuation,
    Any,
    Ascii,
    AsciiHexDigit,
    Emoji,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Class {
    ranges: Vec<(u32, u32)>,
    properties: Vec<(Property, bool)>,
    negated: bool,
    ignore_case: bool,
}

#[derive(Clone, Debug)]
enum Node {
    Empty,
    Char(u32, bool),
    Any(bool),
    Class(Class),
    RgiEmoji,
    Start(bool),
    End(bool),
    Boundary(bool, bool),
    Group(Box<Node>, Option<usize>),
    Look {
        node: Box<Node>,
        behind: bool,
        negate: bool,
    },
    Backref(usize, bool),
    NamedBackref(String, bool),
    Concat(Vec<Node>),
    Alternate(Vec<Node>),
    Repeat {
        node: Box<Node>,
        min: u32,
        max: Option<u32>,
        greedy: bool,
    },
}

#[derive(Clone, Copy, Debug)]
enum Inst {
    Char(u32, bool),
    Any(bool),
    Class(usize),
    RgiEmoji,
    Split(usize, usize),
    Jump(usize),
    Save(usize),
    Start(bool),
    End(bool),
    Boundary(bool, bool),
    Backref(usize, bool),
    NamedBackref(usize, bool),
    Look {
        end: usize,
        behind: bool,
        negate: bool,
    },
    // records the position where a loop iteration began, to stop empty iterations
    Mark(usize),
    // fails when the iteration that began at the mark consumed nothing
    Progress(usize),
    // resets the captures of a quantified group at the start of each iteration
    Clear(usize, usize),
    Match,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum AsciiSplit {
    One(u8),
    Two(u8, u8),
    Mask(u128),
}

impl AsciiSplit {
    fn from_mask(mask: u128) -> Self {
        match mask.count_ones() {
            1 => Self::One(mask.trailing_zeros() as u8),
            2 => {
                let first = mask.trailing_zeros() as u8;
                let rest = mask & !(1u128 << first);
                Self::Two(first, rest.trailing_zeros() as u8)
            }
            _ => Self::Mask(mask),
        }
    }
}

pub(super) struct Regex {
    program: Vec<Inst>,
    classes: Vec<Class>,
    // capture groups, group 0 included
    pub(super) groups: usize,
    pub(super) names: Vec<(String, Vec<usize>)>,
    marks: usize,
    pub(super) flags: Flags,
    // which ascii units a one-character pattern matches, worked out on the first split by it
    ascii_units: std::cell::OnceCell<Option<u128>>,
    // pre-classified ascii fast split pattern (single unit or greedy `+` repeat)
    ascii_split: std::cell::OnceCell<Option<(AsciiSplit, bool)>>,
}

// the start and end of each group, group 0 first
type Captures = Vec<Option<(usize, usize)>>;

pub(super) struct Match {
    // UTF-16 offsets for each group, in pairs
    pub(super) captures: Vec<Option<(usize, usize)>>,
}

// ---- parsing ----

struct Parser<'a> {
    pattern: &'a [u32],
    position: usize,
    flags: Flags,
    ignore_case: bool,
    multiline: bool,
    dot_all: bool,
    groups: usize,
    total_groups: usize,
    names: Vec<(String, Vec<usize>)>,
    named_refs: Vec<String>,
    // whether the pattern contains named groups, which turns \k into a named reference
    has_names: bool,
}

fn syntax(message: impl Into<String>) -> String {
    message.into()
}

fn is_syntax_character(character: u32) -> bool {
    matches!(
        char::from_u32(character),
        Some(
            '^' | '$'
                | '\\'
                | '.'
                | '*'
                | '+'
                | '?'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '|'
                | '/'
        )
    )
}

fn digit_ranges() -> Vec<(u32, u32)> {
    vec![(b'0' as u32, b'9' as u32)]
}

fn word_ranges() -> Vec<(u32, u32)> {
    vec![
        (b'0' as u32, b'9' as u32),
        (b'A' as u32, b'Z' as u32),
        (b'_' as u32, b'_' as u32),
        (b'a' as u32, b'z' as u32),
    ]
}

fn space_ranges() -> Vec<(u32, u32)> {
    vec![
        (0x09, 0x0d),
        (0x20, 0x20),
        (0xa0, 0xa0),
        (0x1680, 0x1680),
        (0x2000, 0x200a),
        (0x2028, 0x2029),
        (0x202f, 0x202f),
        (0x205f, 0x205f),
        (0x3000, 0x3000),
        (0xfeff, 0xfeff),
    ]
}

fn normalize_ranges(ranges: &mut Vec<(u32, u32)>) {
    if ranges.len() <= 1 {
        return;
    }
    ranges.sort_unstable();
    let mut write = 0;
    for read in 1..ranges.len() {
        let (s, e) = ranges[read];
        if s <= ranges[write].1.saturating_add(1) {
            ranges[write].1 = ranges[write].1.max(e);
        } else {
            write += 1;
            ranges[write] = (s, e);
        }
    }
    ranges.truncate(write + 1);
}

fn intersect_ranges(a: &[(u32, u32)], b: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let lo = a[i].0.max(b[j].0);
        let hi = a[i].1.min(b[j].1);
        if lo <= hi {
            out.push((lo, hi));
        }
        if a[i].1 < b[j].1 {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

fn subtract_ranges(a: &[(u32, u32)], b: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut j = 0;
    for &(mut lo, hi) in a {
        while j < b.len() && b[j].1 < lo {
            j += 1;
        }
        let mut k = j;
        while k < b.len() && b[k].0 <= hi {
            if b[k].0 > lo {
                out.push((lo, b[k].0 - 1));
            }
            if b[k].1 >= hi {
                lo = hi.saturating_add(1);
                break;
            }
            lo = b[k].1 + 1;
            k += 1;
        }
        if lo <= hi {
            out.push((lo, hi));
        }
    }
    out
}

fn ascii_hex_ranges() -> Vec<(u32, u32)> {
    vec![(0x30, 0x39), (0x41, 0x46), (0x61, 0x66)]
}

fn emoji_keycap_strings() -> Vec<Vec<u32>> {
    let mut out = Vec::with_capacity(12);
    out.push(vec![0x23, 0xfe0f, 0x20e3]);
    out.push(vec![0x2a, 0xfe0f, 0x20e3]);
    for d in 0x30..=0x39u32 {
        out.push(vec![d, 0xfe0f, 0x20e3]);
    }
    out
}

fn property_ranges(property: Property, negated: bool) -> Vec<(u32, u32)> {
    let mut ranges = match property {
        Property::Ascii => vec![(0, 0x7f)],
        Property::AsciiHexDigit => ascii_hex_ranges(),
        Property::Any => vec![(0, 0x10ffff)],
        Property::WhiteSpace => space_ranges(),
        _ => {
            let mut out = Vec::new();
            let mut in_run = false;
            let mut start = 0u32;
            for cp in 0..=0x10ffffu32 {
                let hit = has_property(property, cp);
                if hit && !in_run {
                    start = cp;
                    in_run = true;
                } else if !hit && in_run {
                    out.push((start, cp - 1));
                    in_run = false;
                }
            }
            if in_run {
                out.push((start, 0x10ffff));
            }
            out
        }
    };
    normalize_ranges(&mut ranges);
    if negated { complement(&ranges) } else { ranges }
}

fn case_fold_ranges_unicode(ranges: &mut Vec<(u32, u32)>) {
    normalize_ranges(ranges);
    let mut extra = Vec::new();
    // fold ascii letters in constant time
    for &(lo, hi) in ranges.iter() {
        let a = lo.max(b'A' as u32);
        let b = hi.min(b'Z' as u32);
        if a <= b {
            extra.push((a + 32, b + 32));
        }
        let a = lo.max(b'a' as u32);
        let b = hi.min(b'z' as u32);
        if a <= b {
            extra.push((a - 32, b - 32));
        }
        if lo <= b's' as u32 && b's' as u32 <= hi || lo <= b'S' as u32 && b'S' as u32 <= hi {
            extra.push((0x017f, 0x017f));
        }
        if lo <= b'k' as u32 && b'k' as u32 <= hi || lo <= b'K' as u32 && b'K' as u32 <= hi {
            extra.push((0x212a, 0x212a));
        }
        if hi >= 0x80 {
            let start = lo.max(0x80);
            if hi - start <= 4096 {
                for cp in start..=hi {
                    for variant in case_variants(cp, true) {
                        extra.push((variant, variant));
                    }
                }
            } else {
                // scan known unicode cased blocks when folding a large range
                for &(blk_lo, blk_hi) in &[
                    (0x0080u32, 0x0587u32),
                    (0x10a0, 0x2cff),
                    (0xa640, 0xabff),
                    (0xff21, 0xff5a),
                    (0x10400, 0x1044f),
                    (0x10c80, 0x10cff),
                    (0x118a0, 0x118df),
                    (0x16e40, 0x16e7f),
                    (0x1e900, 0x1e943),
                ] {
                    let s = start.max(blk_lo);
                    let e = hi.min(blk_hi);
                    if s <= e {
                        for cp in s..=e {
                            for variant in case_variants(cp, true) {
                                extra.push((variant, variant));
                            }
                        }
                    }
                }
            }
        }
    }
    ranges.extend(extra);
    normalize_ranges(ranges);
}

#[derive(Clone, Debug, Default)]
struct ClassSet {
    ranges: Vec<(u32, u32)>,
    strings: Vec<Vec<u32>>,
    rgi_emoji: bool,
}

impl ClassSet {
    fn normalize(&mut self) {
        normalize_ranges(&mut self.ranges);
        self.strings.sort();
        self.strings.dedup();
    }

    fn fold_case(&mut self) {
        case_fold_ranges_unicode(&mut self.ranges);
        for string in &mut self.strings {
            for cp in string.iter_mut() {
                *cp = canonicalize(*cp, true);
            }
        }
        self.normalize();
    }

    fn union_with(&mut self, other: ClassSet) {
        self.ranges.extend(other.ranges);
        self.strings.extend(other.strings);
        self.rgi_emoji |= other.rgi_emoji;
        self.normalize();
    }

    fn intersect_with(&mut self, other: ClassSet) {
        self.ranges = intersect_ranges(&self.ranges, &other.ranges);
        self.strings
            .retain(|s| other.strings.binary_search(s).is_ok());
        self.rgi_emoji = self.rgi_emoji && other.rgi_emoji;
        self.normalize();
    }

    fn subtract(&mut self, other: ClassSet) {
        self.ranges = subtract_ranges(&self.ranges, &other.ranges);
        self.strings
            .retain(|s| other.strings.binary_search(s).is_err());
        if other.rgi_emoji {
            self.rgi_emoji = false;
        }
        self.normalize();
    }

    fn may_contain_strings(&self) -> bool {
        !self.strings.is_empty() || self.rgi_emoji
    }

    fn into_node(mut self, ignore_case: bool) -> Node {
        self.normalize();
        let has_empty = self.strings.iter().any(|s| s.is_empty());
        let mut non_empty: Vec<Vec<u32>> =
            self.strings.into_iter().filter(|s| !s.is_empty()).collect();
        if non_empty.is_empty() && !has_empty && !self.rgi_emoji {
            return Node::Class(Class {
                ranges: self.ranges,
                properties: Vec::new(),
                negated: false,
                ignore_case: false,
            });
        }
        // longest strings match first per unicode sets semantics
        non_empty.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        let mut alts = Vec::new();
        if self.rgi_emoji {
            alts.push(Node::RgiEmoji);
        }
        for string in non_empty {
            let nodes = string
                .into_iter()
                .map(|cp| Node::Char(cp, ignore_case))
                .collect();
            alts.push(Node::Concat(nodes));
        }
        if !self.ranges.is_empty() {
            alts.push(Node::Class(Class {
                ranges: self.ranges,
                properties: Vec::new(),
                negated: false,
                ignore_case: false,
            }));
        }
        if has_empty {
            alts.push(Node::Empty);
        }
        match alts.len() {
            0 => Node::Class(Class::default()),
            1 => alts.pop().expect("one alternative"),
            _ => Node::Alternate(alts),
        }
    }
}

fn is_v_syntax_char(c: char) -> bool {
    matches!(
        c,
        '(' | ')' | '[' | ']' | '{' | '}' | '/' | '-' | '\\' | '|'
    )
}

fn is_v_double_punctuator(c: char) -> bool {
    matches!(
        c,
        '&' | '!'
            | '#'
            | '$'
            | '%'
            | '*'
            | '+'
            | ','
            | '.'
            | ':'
            | ';'
            | '<'
            | '='
            | '>'
            | '?'
            | '@'
            | '^'
            | '`'
            | '~'
    )
}

fn is_property_of_strings(name: &str) -> bool {
    matches!(
        name,
        "Basic_Emoji"
            | "Emoji_Keycap_Sequence"
            | "RGI_Emoji_Modifier_Sequence"
            | "RGI_Emoji_Flag_Sequence"
            | "RGI_Emoji_Tag_Sequence"
            | "RGI_Emoji_ZWJ_Sequence"
            | "RGI_Emoji"
    )
}

fn complement(ranges: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut sorted = ranges.to_vec();
    sorted.sort_unstable();
    let mut result = Vec::new();
    let mut next = 0u32;
    for (start, end) in sorted {
        if start > next {
            result.push((next, start - 1));
        }
        next = next.max(end.saturating_add(1));
    }
    if next <= 0x10ffff {
        result.push((next, 0x10ffff));
    }
    result
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u32> {
        self.pattern.get(self.position).copied()
    }

    fn peek_char(&self) -> Option<char> {
        self.peek().and_then(char::from_u32)
    }

    fn eat(&mut self, character: char) -> bool {
        if self.peek_char() == Some(character) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn looking_at(&self, text: &str) -> bool {
        text.chars().enumerate().all(|(offset, character)| {
            self.pattern.get(self.position + offset).copied() == Some(character as u32)
        })
    }

    fn disjunction(&mut self) -> Result<(Node, Vec<String>), String> {
        let (first, mut names) = self.alternative()?;
        let mut alternatives = vec![first];
        while self.eat('|') {
            let (alt, alt_names) = self.alternative()?;
            for name in alt_names {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
            alternatives.push(alt);
        }
        let node = if alternatives.len() == 1 {
            alternatives.pop().expect("one alternative")
        } else {
            Node::Alternate(alternatives)
        };
        Ok((node, names))
    }

    fn alternative(&mut self) -> Result<(Node, Vec<String>), String> {
        let mut terms = Vec::new();
        let mut names: Vec<String> = Vec::new();
        while let Some(character) = self.peek_char() {
            if character == '|' || character == ')' {
                break;
            }
            let (term, term_names) = self.term()?;
            for name in term_names {
                if names.contains(&name) {
                    return Err(syntax("Duplicate capture group name"));
                }
                names.push(name);
            }
            terms.push(term);
        }
        let node = match terms.len() {
            0 => Node::Empty,
            1 => terms.pop().expect("one term"),
            _ => Node::Concat(terms),
        };
        Ok((node, names))
    }

    fn term(&mut self) -> Result<(Node, Vec<String>), String> {
        let (atom, names) = match self.peek_char() {
            Some('^') => {
                self.position += 1;
                return Ok((Node::Start(self.multiline), Vec::new()));
            }
            Some('$') => {
                self.position += 1;
                return Ok((Node::End(self.multiline), Vec::new()));
            }
            Some('\\')
                if matches!(
                    self.pattern
                        .get(self.position + 1)
                        .copied()
                        .and_then(char::from_u32),
                    Some('b' | 'B')
                ) =>
            {
                let negated = self.pattern[self.position + 1] == 'B' as u32;
                self.position += 2;
                return Ok((Node::Boundary(!negated, self.ignore_case), Vec::new()));
            }
            Some('(')
                if self.looking_at("(?=")
                    || self.looking_at("(?!")
                    || self.looking_at("(?<=")
                    || self.looking_at("(?<!") =>
            {
                let behind = self.looking_at("(?<");
                self.position += if behind { 3 } else { 2 };
                let negate = self.eat('!');
                if !negate && !self.eat('=') {
                    return Err(syntax("Invalid group"));
                }
                let (node, names) = self.disjunction()?;
                if !self.eat(')') {
                    return Err(syntax("Unterminated group"));
                }
                let look = Node::Look {
                    node: Box::new(node),
                    behind,
                    negate,
                };
                // lookaheads may be quantified outside unicode mode, lookbehinds never
                if behind || self.flags.full_unicode() {
                    return Ok((look, names));
                }
                (look, names)
            }
            _ => self.atom()?,
        };
        Ok((self.quantifier(atom)?, names))
    }

    fn quantifier(&mut self, atom: Node) -> Result<Node, String> {
        let (min, max) = match self.peek_char() {
            Some('*') => {
                self.position += 1;
                (0, None)
            }
            Some('+') => {
                self.position += 1;
                (1, None)
            }
            Some('?') => {
                self.position += 1;
                (0, Some(1))
            }
            Some('{') => match self.braced_quantifier()? {
                Some(bounds) => bounds,
                None => return Ok(atom),
            },
            _ => return Ok(atom),
        };
        if let Some(max) = max
            && max < min
        {
            return Err(syntax("numbers out of order in {} quantifier"));
        }
        if matches!(atom, Node::Start(_) | Node::End(_) | Node::Boundary(_, _)) {
            return Err(syntax("Nothing to repeat"));
        }
        let greedy = !self.eat('?');
        Ok(Node::Repeat {
            node: Box::new(atom),
            min,
            max,
            greedy,
        })
    }

    // {n}, {n,} or {n,m}; outside unicode mode a brace that is not a quantifier is a literal
    fn braced_quantifier(&mut self) -> Result<Option<(u32, Option<u32>)>, String> {
        let saved = self.position;
        self.position += 1;
        let read_number = |parser: &mut Self| -> Option<u32> {
            let start = parser.position;
            let mut value: u64 = 0;
            while let Some(digit) = parser
                .peek_char()
                .and_then(|character| character.to_digit(10))
            {
                value = (value * 10 + u64::from(digit)).min(u64::from(u32::MAX));
                parser.position += 1;
            }
            (parser.position > start).then_some(value as u32)
        };
        let Some(min) = read_number(self) else {
            self.position = saved;
            return if self.flags.full_unicode() {
                Err(syntax("Incomplete quantifier"))
            } else {
                Ok(None)
            };
        };
        let max = if self.eat(',') {
            read_number(self)
        } else {
            Some(min)
        };
        if !self.eat('}') {
            self.position = saved;
            return if self.flags.full_unicode() {
                Err(syntax("Incomplete quantifier"))
            } else {
                Ok(None)
            };
        }
        Ok(Some((min, max)))
    }

    fn atom(&mut self) -> Result<(Node, Vec<String>), String> {
        let character = self
            .peek()
            .ok_or_else(|| syntax("Unexpected end of pattern"))?;
        match char::from_u32(character) {
            Some('.') => {
                self.position += 1;
                Ok((Node::Any(self.dot_all), Vec::new()))
            }
            Some('(') => {
                self.position += 1;
                if self.eat('?') {
                    if self.eat(':') {
                        let (node, names) = self.disjunction()?;
                        if !self.eat(')') {
                            return Err(syntax("Unterminated group"));
                        }
                        return Ok((Node::Group(Box::new(node), None), names));
                    }
                    if self.eat('<') {
                        let name = self.group_name()?;
                        self.groups += 1;
                        let index = self.groups;
                        if let Some((_, indices)) = self
                            .names
                            .iter_mut()
                            .find(|(existing, _)| existing == &name)
                        {
                            indices.push(index);
                        } else {
                            self.names.push((name.clone(), vec![index]));
                        }
                        let (node, mut inner_names) = self.disjunction()?;
                        if !self.eat(')') {
                            return Err(syntax("Unterminated group"));
                        }
                        if inner_names.contains(&name) {
                            return Err(syntax("Duplicate capture group name"));
                        }
                        inner_names.push(name);
                        return Ok((Node::Group(Box::new(node), Some(index)), inner_names));
                    }
                    return self.parse_modifier_group();
                }
                self.groups += 1;
                let index = self.groups;
                let (node, names) = self.disjunction()?;
                if !self.eat(')') {
                    return Err(syntax("Unterminated group"));
                }
                Ok((Node::Group(Box::new(node), Some(index)), names))
            }
            Some(')') => Err(syntax("Unmatched ')'")),
            Some('[') => {
                self.position += 1;
                if self.flags.unicode_sets {
                    let set = self.class_v()?;
                    Ok((set.into_node(self.ignore_case), Vec::new()))
                } else {
                    Ok((Node::Class(self.class()?), Vec::new()))
                }
            }
            Some('*' | '+' | '?') => Err(syntax("Nothing to repeat")),
            Some('{') if self.flags.full_unicode() => Err(syntax("Lone quantifier brackets")),
            Some('{') => {
                // a brace that would be a valid quantifier has nothing to repeat
                let saved = self.position;
                if self.braced_quantifier()?.is_some() {
                    return Err(syntax("Nothing to repeat"));
                }
                self.position = saved + 1;
                Ok((Node::Char('{' as u32, self.ignore_case), Vec::new()))
            }
            Some('}' | ']') if self.flags.full_unicode() => Err(syntax("Lone quantifier brackets")),
            Some('\\') => {
                self.position += 1;
                Ok((self.atom_escape()?, Vec::new()))
            }
            _ => {
                self.position += 1;
                Ok((Node::Char(character, self.ignore_case), Vec::new()))
            }
        }
    }

    fn parse_modifier_group(&mut self) -> Result<(Node, Vec<String>), String> {
        let mut add = 0u8;
        let mut rem = 0u8;
        let bit = |ch: char| match ch {
            'i' => Some(1u8),
            'm' => Some(2u8),
            's' => Some(4u8),
            _ => None,
        };
        while let Some(ch) = self.peek_char() {
            if let Some(b) = bit(ch) {
                if (add & b) != 0 {
                    return Err(syntax("Repeated modifier flag"));
                }
                add |= b;
                self.position += 1;
            } else {
                break;
            }
        }
        if self.eat('-') {
            while let Some(ch) = self.peek_char() {
                if let Some(b) = bit(ch) {
                    if (rem & b) != 0 || (add & b) != 0 {
                        return Err(syntax("Repeated modifier flag"));
                    }
                    rem |= b;
                    self.position += 1;
                } else {
                    break;
                }
            }
        }
        if (add == 0 && rem == 0) || !self.eat(':') {
            return Err(syntax("Invalid group"));
        }
        let saved = (self.ignore_case, self.multiline, self.dot_all);
        if (add & 1) != 0 {
            self.ignore_case = true;
        }
        if (add & 2) != 0 {
            self.multiline = true;
        }
        if (add & 4) != 0 {
            self.dot_all = true;
        }
        if (rem & 1) != 0 {
            self.ignore_case = false;
        }
        if (rem & 2) != 0 {
            self.multiline = false;
        }
        if (rem & 4) != 0 {
            self.dot_all = false;
        }
        let parsed = self.disjunction();
        (self.ignore_case, self.multiline, self.dot_all) = saved;
        let (node, names) = parsed?;
        if !self.eat(')') {
            return Err(syntax("Unterminated group"));
        }
        Ok((Node::Group(Box::new(node), None), names))
    }

    fn group_name(&mut self) -> Result<String, String> {
        let mut name = String::new();
        loop {
            let Some(unit) = self.peek() else {
                return Err(syntax("Invalid capture group name"));
            };
            if unit == '>' as u32 {
                self.position += 1;
                break;
            }
            let ch = if unit == '\\' as u32 {
                self.position += 1;
                if !self.eat('u') {
                    return Err(syntax("Invalid capture group name"));
                }
                let cp = if self.eat('{') {
                    let mut value = 0u32;
                    let mut digits = 0;
                    while let Some(d) = self.peek_char().and_then(|c| c.to_digit(16)) {
                        value = value.saturating_mul(16).saturating_add(d);
                        self.position += 1;
                        digits += 1;
                    }
                    if digits == 0 || !self.eat('}') || value > 0x10ffff {
                        return Err(syntax("Invalid capture group name"));
                    }
                    value
                } else {
                    let Some(high) = self.hex_digits(4) else {
                        return Err(syntax("Invalid capture group name"));
                    };
                    if (0xd800..0xdc00).contains(&high) && self.looking_at("\\u") {
                        let saved = self.position;
                        self.position += 2;
                        match self.hex_digits(4) {
                            Some(low) if (0xdc00..0xe000).contains(&low) => {
                                0x10000 + ((high - 0xd800) << 10) + (low - 0xdc00)
                            }
                            _ => {
                                self.position = saved;
                                return Err(syntax("Invalid capture group name"));
                            }
                        }
                    } else {
                        high
                    }
                };
                char::from_u32(cp).ok_or_else(|| syntax("Invalid capture group name"))?
            } else {
                self.position += 1;
                if (0xd800..0xdc00).contains(&unit)
                    && let Some(low) = self.peek()
                    && (0xdc00..0xe000).contains(&low)
                {
                    self.position += 1;
                    let cp = 0x10000 + ((unit - 0xd800) << 10) + (low - 0xdc00);
                    char::from_u32(cp).ok_or_else(|| syntax("Invalid capture group name"))?
                } else {
                    char::from_u32(unit).ok_or_else(|| syntax("Invalid capture group name"))?
                }
            };
            let ok = if name.is_empty() {
                ch == '$' || ch == '_' || super::lexer::is_identifier_start(ch)
            } else {
                ch == '$'
                    || ch == '_'
                    || ch == '\u{200c}'
                    || ch == '\u{200d}'
                    || super::lexer::is_identifier_part(ch)
            };
            if !ok {
                return Err(syntax("Invalid capture group name"));
            }
            name.push(ch);
        }
        if name.is_empty() {
            return Err(syntax("Invalid capture group name"));
        }
        Ok(name)
    }

    fn atom_escape(&mut self) -> Result<Node, String> {
        let character = self
            .peek_char()
            .ok_or_else(|| syntax("\\ at end of pattern"))?;
        match character {
            '1'..='9' => {
                let start = self.position;
                let mut value: usize = 0;
                while let Some(digit) = self
                    .peek_char()
                    .and_then(|character| character.to_digit(10))
                {
                    value = value.saturating_mul(10).saturating_add(digit as usize);
                    self.position += 1;
                }
                // a number beyond the group count is an octal or identity escape outside unicode mode
                if value <= self.total_groups || self.flags.full_unicode() {
                    if value > self.total_groups {
                        return Err(syntax("Invalid escape"));
                    }
                    return Ok(Node::Backref(value, self.ignore_case));
                }
                self.position = start;
                self.legacy_octal_or_identity()
            }
            'k' if self.has_names || self.flags.full_unicode() => {
                self.position += 1;
                if !self.eat('<') {
                    return Err(syntax("Invalid named reference"));
                }
                let name = self.group_name()?;
                self.named_refs.push(name.clone());
                Ok(Node::NamedBackref(name, self.ignore_case))
            }
            'd' | 'D' | 'w' | 'W' | 's' | 'S' => {
                let class = self.class_escape()?.expect("a class escape");
                Ok(Node::Class(class))
            }
            'p' | 'P' if self.flags.full_unicode() => {
                let negated = character == 'P';
                self.position += 1;
                if self.flags.unicode_sets {
                    let set = self.v_property_escape(negated)?;
                    return Ok(set.into_node(self.ignore_case));
                }
                let (property, _) = self.property()?;
                Ok(Node::Class(Class {
                    ranges: Vec::new(),
                    properties: vec![(property, negated)],
                    negated: false,
                    ignore_case: self.ignore_case,
                }))
            }
            _ => Ok(Node::Char(self.character_escape(false)?, self.ignore_case)),
        }
    }

    // the number of capture groups and presence of named groups in the whole pattern
    fn scan_groups(pattern: &[u32], unicode_sets: bool) -> (usize, bool) {
        let mut count = 0;
        let mut has_names = false;
        let mut index = 0;
        let mut class_depth = 0usize;
        while index < pattern.len() {
            match char::from_u32(pattern[index]) {
                Some('\\') => index += 1,
                Some('[') => {
                    if unicode_sets || class_depth == 0 {
                        class_depth += 1;
                    }
                }
                Some(']') if class_depth > 0 => class_depth -= 1,
                Some('(') if class_depth == 0 => {
                    let next = pattern.get(index + 1).copied().and_then(char::from_u32);
                    let after = pattern.get(index + 2).copied().and_then(char::from_u32);
                    if next != Some('?') {
                        count += 1;
                    } else if after == Some('<')
                        && !matches!(
                            pattern.get(index + 3).copied().and_then(char::from_u32),
                            Some('=' | '!')
                        )
                    {
                        count += 1;
                        has_names = true;
                    }
                }
                _ => {}
            }
            index += 1;
        }
        (count, has_names)
    }

    fn legacy_octal_or_identity(&mut self) -> Result<Node, String> {
        let first = self
            .peek_char()
            .and_then(|character| character.to_digit(10))
            .unwrap_or(0);
        if first >= 8 {
            self.position += 1;
            return Ok(Node::Char('0' as u32 + first, self.ignore_case));
        }
        let mut value = 0u32;
        let mut digits = 0;
        while digits < 3 {
            match self.peek_char().and_then(|character| character.to_digit(8)) {
                Some(digit) if value * 8 + digit <= 0o377 => {
                    value = value * 8 + digit;
                    self.position += 1;
                    digits += 1;
                }
                _ => break,
            }
        }
        Ok(Node::Char(value, self.ignore_case))
    }

    fn hex_digits(&mut self, count: usize) -> Option<u32> {
        let mut value = 0;
        for offset in 0..count {
            let digit = self
                .pattern
                .get(self.position + offset)
                .copied()
                .and_then(char::from_u32)?
                .to_digit(16)?;
            value = value * 16 + digit;
        }
        self.position += count;
        Some(value)
    }

    // an escape that stands for one character; in_class allows \b as backspace and \- as dash
    fn character_escape(&mut self, in_class: bool) -> Result<u32, String> {
        let character = self
            .peek_char()
            .ok_or_else(|| syntax("\\ at end of pattern"))?;
        self.position += 1;
        Ok(match character {
            't' => 0x09,
            'n' => 0x0a,
            'v' => 0x0b,
            'f' => 0x0c,
            'r' => 0x0d,
            'b' if in_class => 0x08,
            '-' if in_class => '-' as u32,
            '0' if !self.peek_char().is_some_and(|next| next.is_ascii_digit()) => 0,
            '0'..='7' if !self.flags.full_unicode() => {
                self.position -= 1;
                match self.legacy_octal_or_identity()? {
                    Node::Char(value, _) => value,
                    _ => unreachable!("octal escapes are characters"),
                }
            }
            'c' => match self.peek_char() {
                Some(letter) if letter.is_ascii_alphabetic() => {
                    self.position += 1;
                    letter as u32 % 32
                }
                Some(letter)
                    if in_class
                        && !self.flags.full_unicode()
                        && (letter.is_ascii_digit() || letter == '_') =>
                {
                    self.position += 1;
                    letter as u32 % 32
                }
                _ if self.flags.full_unicode() => return Err(syntax("Invalid unicode escape")),
                _ => {
                    // \c without a letter is a literal backslash followed by c
                    self.position -= 1;
                    '\\' as u32
                }
            },
            'x' => match self.hex_digits(2) {
                Some(value) => value,
                None if self.flags.full_unicode() => return Err(syntax("Invalid escape")),
                None => 'x' as u32,
            },
            'u' => {
                if self.eat('{') {
                    if !self.flags.full_unicode() {
                        self.position -= 1;
                        return Ok('u' as u32);
                    }
                    let mut value = 0u32;
                    let mut digits = 0;
                    while let Some(digit) = self
                        .peek_char()
                        .and_then(|character| character.to_digit(16))
                    {
                        value = value.saturating_mul(16).saturating_add(digit);
                        self.position += 1;
                        digits += 1;
                    }
                    if digits == 0 || !self.eat('}') || value > 0x10ffff {
                        return Err(syntax("Invalid Unicode escape"));
                    }
                    value
                } else {
                    match self.hex_digits(4) {
                        Some(high) => {
                            // a surrogate pair written as two escapes is one code point in unicode mode
                            if self.flags.full_unicode()
                                && (0xd800..0xdc00).contains(&high)
                                && self.looking_at("\\u")
                            {
                                let saved = self.position;
                                self.position += 2;
                                match self.hex_digits(4) {
                                    Some(low) if (0xdc00..0xe000).contains(&low) => {
                                        0x10000 + ((high - 0xd800) << 10) + (low - 0xdc00)
                                    }
                                    _ => {
                                        self.position = saved;
                                        high
                                    }
                                }
                            } else {
                                high
                            }
                        }
                        None if self.flags.full_unicode() => {
                            return Err(syntax("Invalid Unicode escape"));
                        }
                        None => 'u' as u32,
                    }
                }
            }
            other if self.flags.full_unicode() => {
                if is_syntax_character(other as u32)
                    || (in_class && other == '-')
                    || (in_class && self.flags.unicode_sets && is_v_double_punctuator(other))
                {
                    other as u32
                } else {
                    return Err(syntax("Invalid escape"));
                }
            }
            other => other as u32,
        })
    }

    fn read_property_name(&mut self) -> Result<String, String> {
        if !self.eat('{') {
            return Err(syntax("Invalid property name"));
        }
        let mut name = String::new();
        while let Some(character) = self.peek_char() {
            self.position += 1;
            if character == '}' {
                return Ok(name);
            }
            name.push(character);
        }
        Err(syntax("Invalid property name"))
    }

    fn parse_property_name(name: &str) -> Result<Property, String> {
        match name {
            "L" | "Letter" => Ok(Property::Letter),
            "Lu" | "Uppercase_Letter" | "Uppercase" => Ok(Property::Uppercase),
            "Ll" | "Lowercase_Letter" | "Lowercase" => Ok(Property::Lowercase),
            "N" | "Number" => Ok(Property::Number),
            "Nd" | "Decimal_Number" | "digit" => Ok(Property::Digit),
            "Alphabetic" | "Alpha" => Ok(Property::Alphabetic),
            "White_Space" | "space" => Ok(Property::WhiteSpace),
            "P" | "Punctuation" | "punct" => Ok(Property::Punctuation),
            "Any" => Ok(Property::Any),
            "ASCII" => Ok(Property::Ascii),
            "ASCII_Hex_Digit" | "AHex" => Ok(Property::AsciiHexDigit),
            "Emoji" | "Extended_Pictographic" | "Emoji_Presentation" => Ok(Property::Emoji),
            _ => Err(syntax(format!("Invalid property name {name}"))),
        }
    }

    fn property(&mut self) -> Result<(Property, bool), String> {
        let name = self.read_property_name()?;
        Ok((Self::parse_property_name(&name)?, false))
    }

    fn v_property_escape(&mut self, negated: bool) -> Result<ClassSet, String> {
        let name = self.read_property_name()?;
        if is_property_of_strings(&name) {
            if negated {
                return Err(syntax("Cannot negate property of strings"));
            }
            let mut set = match name.as_str() {
                "Emoji_Keycap_Sequence" => ClassSet {
                    ranges: Vec::new(),
                    strings: emoji_keycap_strings(),
                    rgi_emoji: false,
                },
                "RGI_Emoji" => ClassSet {
                    ranges: Vec::new(),
                    strings: Vec::new(),
                    rgi_emoji: true,
                },
                _ => ClassSet {
                    ranges: Vec::new(),
                    strings: Vec::new(),
                    rgi_emoji: true,
                },
            };
            if self.ignore_case {
                set.fold_case();
            }
            return Ok(set);
        }
        let prop = Self::parse_property_name(&name)?;
        let mut ranges = property_ranges(prop, false);
        if self.ignore_case {
            case_fold_ranges_unicode(&mut ranges);
        }
        if negated {
            ranges = complement(&ranges);
        }
        Ok(ClassSet {
            ranges,
            strings: Vec::new(),
            rgi_emoji: false,
        })
    }

    // \d \w \s and \p escapes as classes; None when the escape is not a class
    fn class_escape(&mut self) -> Result<Option<Class>, String> {
        let Some(character) = self.peek_char() else {
            return Ok(None);
        };
        let (ranges, negated) = match character {
            'd' => (digit_ranges(), false),
            'D' => (digit_ranges(), true),
            'w' => (word_ranges(), false),
            'W' => (word_ranges(), true),
            's' => (space_ranges(), false),
            'S' => (space_ranges(), true),
            'p' | 'P' if self.flags.full_unicode() => {
                self.position += 1;
                let (property, _) = self.property()?;
                return Ok(Some(Class {
                    ranges: Vec::new(),
                    properties: vec![(property, character == 'P')],
                    negated: false,
                    ignore_case: self.ignore_case,
                }));
            }
            _ => return Ok(None),
        };
        self.position += 1;
        let ranges = if negated {
            let mut base = ranges;
            if character == 'W' && self.ignore_case && self.flags.full_unicode() {
                base.push((0x017f, 0x017f));
                base.push((0x212a, 0x212a));
            }
            complement(&base)
        } else {
            ranges
        };
        Ok(Some(Class {
            ranges,
            properties: Vec::new(),
            negated: false,
            ignore_case: self.ignore_case,
        }))
    }

    fn class_v(&mut self) -> Result<ClassSet, String> {
        let negated = self.eat('^');
        if self.eat(']') {
            let ranges = if negated { vec![(0, 0x10ffff)] } else { vec![] };
            return Ok(ClassSet {
                ranges,
                strings: Vec::new(),
                rgi_emoji: false,
            });
        }
        let mut acc = self.v_class_operand()?;
        let op = if self.looking_at("&&") {
            Some('&')
        } else if self.looking_at("--") {
            Some('-')
        } else {
            None
        };
        if let Some(op_ch) = op {
            let tok = if op_ch == '&' { "&&" } else { "--" };
            while self.looking_at(tok) {
                self.position += 2;
                let rhs = self.v_class_operand()?;
                if op_ch == '&' {
                    acc.intersect_with(rhs);
                } else {
                    acc.subtract(rhs);
                }
            }
            if !self.eat(']') {
                return Err(syntax("Invalid character class"));
            }
        } else {
            while !self.eat(']') {
                if self.peek().is_none() {
                    return Err(syntax("Unterminated character class"));
                }
                if self.looking_at("&&") || self.looking_at("--") {
                    return Err(syntax("Invalid character class"));
                }
                let next = self.v_class_operand()?;
                acc.union_with(next);
            }
        }
        if negated {
            if acc.may_contain_strings() {
                return Err(syntax("Cannot negate a character class containing strings"));
            }
            acc.ranges = complement(&acc.ranges);
        }
        Ok(acc)
    }

    fn v_class_operand(&mut self) -> Result<ClassSet, String> {
        match self.peek_char() {
            Some('[') => {
                self.position += 1;
                self.class_v()
            }
            Some('\\') => {
                if self.looking_at("\\q{") {
                    self.position += 3;
                    return self.v_string_disjunction();
                }
                let next = self
                    .pattern
                    .get(self.position + 1)
                    .copied()
                    .and_then(char::from_u32);
                match next {
                    Some('d' | 'D' | 'w' | 'W' | 's' | 'S') => {
                        self.position += 2;
                        let ch = next.expect("checked");
                        let (mut ranges, neg) = match ch {
                            'd' => (digit_ranges(), false),
                            'D' => (digit_ranges(), true),
                            'w' => (word_ranges(), false),
                            'W' => (word_ranges(), true),
                            's' => (space_ranges(), false),
                            'S' => (space_ranges(), true),
                            _ => unreachable!(),
                        };
                        if self.ignore_case && matches!(ch, 'w' | 'W') {
                            case_fold_ranges_unicode(&mut ranges);
                        }
                        if neg {
                            ranges = complement(&ranges);
                        }
                        if self.peek_char() == Some('-') && !self.looking_at("--") {
                            return Err(syntax("Invalid character class"));
                        }
                        Ok(ClassSet {
                            ranges,
                            strings: Vec::new(),
                            rgi_emoji: false,
                        })
                    }
                    Some('p' | 'P') => {
                        let neg = next == Some('P');
                        self.position += 2;
                        let set = self.v_property_escape(neg)?;
                        if self.peek_char() == Some('-') && !self.looking_at("--") {
                            return Err(syntax("Invalid character class"));
                        }
                        Ok(set)
                    }
                    _ => self.v_char_or_range(),
                }
            }
            _ => self.v_char_or_range(),
        }
    }

    fn v_char_or_range(&mut self) -> Result<ClassSet, String> {
        let start = self.v_class_char()?;
        let mut set = if self.peek_char() == Some('-') && !self.looking_at("--") {
            self.position += 1;
            let end = self.v_class_char()?;
            if end < start {
                return Err(syntax("Range out of order in character class"));
            }
            ClassSet {
                ranges: vec![(start, end)],
                strings: Vec::new(),
                rgi_emoji: false,
            }
        } else {
            ClassSet {
                ranges: vec![(start, start)],
                strings: Vec::new(),
                rgi_emoji: false,
            }
        };
        if self.ignore_case {
            set.fold_case();
        }
        Ok(set)
    }

    fn v_class_char(&mut self) -> Result<u32, String> {
        let Some(unit) = self.peek() else {
            return Err(syntax("Unterminated character class"));
        };
        if unit == '\\' as u32 {
            self.position += 1;
            return self.character_escape(true);
        }
        if let Some(ch) = char::from_u32(unit) {
            if is_v_syntax_char(ch) {
                return Err(syntax("Invalid character in unicodeSets class"));
            }
            if is_v_double_punctuator(ch)
                && self.pattern.get(self.position + 1).copied() == Some(unit)
            {
                return Err(syntax("Invalid double punctuator in unicodeSets class"));
            }
        }
        self.position += 1;
        Ok(unit)
    }

    fn v_string_disjunction(&mut self) -> Result<ClassSet, String> {
        let mut set = ClassSet::default();
        loop {
            let mut chars = Vec::new();
            while let Some(unit) = self.peek() {
                if unit == '|' as u32 || unit == '}' as u32 {
                    break;
                }
                chars.push(self.v_class_char()?);
            }
            if chars.len() == 1 {
                set.ranges.push((chars[0], chars[0]));
            } else {
                set.strings.push(chars);
            }
            if self.eat('|') {
                continue;
            }
            if self.eat('}') {
                break;
            }
            return Err(syntax("Unterminated \\q{} string literal"));
        }
        if self.ignore_case {
            set.fold_case();
        }
        set.normalize();
        Ok(set)
    }

    fn class(&mut self) -> Result<Class, String> {
        let mut class = Class {
            negated: self.eat('^'),
            ignore_case: self.ignore_case,
            ..Class::default()
        };
        loop {
            let Some(character) = self.peek_char() else {
                return Err(syntax("Unterminated character class"));
            };
            if character == ']' {
                self.position += 1;
                return Ok(class);
            }
            let start = match self.class_atom()? {
                ClassAtom::Set(set) => {
                    class.ranges.extend(set.ranges);
                    class.properties.extend(set.properties);
                    if self.peek_char() == Some('-')
                        && self.pattern.get(self.position + 1) != Some(&(']' as u32))
                        && self.flags.full_unicode()
                    {
                        return Err(syntax("Invalid character class"));
                    }
                    continue;
                }
                ClassAtom::Char(value) => value,
            };
            if self.peek_char() == Some('-')
                && self
                    .pattern
                    .get(self.position + 1)
                    .is_some_and(|next| *next != ']' as u32)
            {
                self.position += 1;
                match self.class_atom()? {
                    ClassAtom::Char(end) => {
                        if end < start {
                            return Err(syntax("Range out of order in character class"));
                        }
                        class.ranges.push((start, end));
                    }
                    ClassAtom::Set(set) => {
                        if self.flags.full_unicode() {
                            return Err(syntax("Invalid character class"));
                        }
                        class.ranges.push((start, start));
                        class.ranges.push(('-' as u32, '-' as u32));
                        class.ranges.extend(set.ranges);
                        class.properties.extend(set.properties);
                    }
                }
            } else {
                class.ranges.push((start, start));
            }
        }
    }

    fn class_atom(&mut self) -> Result<ClassAtom, String> {
        let character = self
            .peek()
            .ok_or_else(|| syntax("Unterminated character class"))?;
        self.position += 1;
        if character != '\\' as u32 {
            // a surrogate pair is one class member in unicode mode
            return Ok(ClassAtom::Char(character));
        }
        if let Some(set) = self.class_escape()? {
            return Ok(ClassAtom::Set(set));
        }
        Ok(ClassAtom::Char(self.character_escape(true)?))
    }
}

enum ClassAtom {
    Char(u32),
    Set(Class),
}

// ---- compiling ----

struct Compiler<'a> {
    program: Vec<Inst>,
    classes: Vec<Class>,
    marks: usize,
    names: &'a [(String, Vec<usize>)],
}

fn group_range(node: &Node) -> Option<(usize, usize)> {
    fn walk(node: &Node, range: &mut Option<(usize, usize)>) {
        match node {
            Node::Group(inner, index) => {
                if let Some(index) = index {
                    *range = Some(match range {
                        Some((low, high)) => ((*low).min(*index), (*high).max(*index)),
                        None => (*index, *index),
                    });
                }
                walk(inner, range);
            }
            Node::Look { node, .. } => walk(node, range),
            Node::Concat(nodes) | Node::Alternate(nodes) => {
                nodes.iter().for_each(|node| walk(node, range))
            }
            Node::Repeat { node, .. } => walk(node, range),
            _ => {}
        }
    }
    let mut range = None;
    walk(node, &mut range);
    range
}

fn can_be_empty(node: &Node) -> bool {
    match node {
        Node::Char(_, _) | Node::Any(_) | Node::Class(_) | Node::RgiEmoji => false,
        Node::Group(inner, _) => can_be_empty(inner),
        Node::Concat(nodes) => nodes.iter().all(can_be_empty),
        Node::Alternate(nodes) => nodes.iter().any(can_be_empty),
        Node::Repeat { node, min, .. } => *min == 0 || can_be_empty(node),
        Node::Backref(_, _) | Node::NamedBackref(_, _) => true,
        _ => true,
    }
}

impl Compiler<'_> {
    fn emit(&mut self, inst: Inst) -> usize {
        self.program.push(inst);
        self.program.len() - 1
    }

    fn check(&self) -> Result<(), String> {
        if self.program.len() > MAX_PROGRAM {
            return Err(syntax("Regular expression too large"));
        }
        Ok(())
    }

    fn compile(&mut self, node: &Node) -> Result<(), String> {
        self.check()?;
        match node {
            Node::Empty => {}
            Node::Char(value, ic) => {
                self.emit(Inst::Char(*value, *ic));
            }
            Node::Any(dot_all) => {
                self.emit(Inst::Any(*dot_all));
            }
            Node::Class(class) => {
                self.classes.push(class.clone());
                let index = self.classes.len() - 1;
                self.emit(Inst::Class(index));
            }
            Node::RgiEmoji => {
                self.emit(Inst::RgiEmoji);
            }
            Node::Start(multiline) => {
                self.emit(Inst::Start(*multiline));
            }
            Node::End(multiline) => {
                self.emit(Inst::End(*multiline));
            }
            Node::Boundary(word, ic) => {
                self.emit(Inst::Boundary(*word, *ic));
            }
            Node::Group(inner, index) => match index {
                Some(index) => {
                    self.emit(Inst::Save(index * 2));
                    self.compile(inner)?;
                    self.emit(Inst::Save(index * 2 + 1));
                }
                None => self.compile(inner)?,
            },
            Node::Look {
                node,
                behind,
                negate,
            } => {
                let look = self.emit(Inst::Look {
                    end: 0,
                    behind: *behind,
                    negate: *negate,
                });
                self.compile(node)?;
                self.emit(Inst::Match);
                let end = self.program.len();
                self.program[look] = Inst::Look {
                    end,
                    behind: *behind,
                    negate: *negate,
                };
            }
            Node::Backref(index, ic) => {
                self.emit(Inst::Backref(*index, *ic));
            }
            Node::NamedBackref(name, ic) => {
                let name_idx = self
                    .names
                    .iter()
                    .position(|(candidate, _)| candidate == name)
                    .ok_or_else(|| syntax("Invalid named capture referenced"))?;
                self.emit(Inst::NamedBackref(name_idx, *ic));
            }
            Node::Concat(nodes) => {
                for node in nodes {
                    self.compile(node)?;
                }
            }
            Node::Alternate(nodes) => {
                let mut exits = Vec::new();
                for (position, node) in nodes.iter().enumerate() {
                    if position + 1 < nodes.len() {
                        let split = self.emit(Inst::Split(0, 0));
                        self.compile(node)?;
                        exits.push(self.emit(Inst::Jump(0)));
                        let next = self.program.len();
                        self.program[split] = Inst::Split(split + 1, next);
                    } else {
                        self.compile(node)?;
                    }
                }
                let end = self.program.len();
                for exit in exits {
                    self.program[exit] = Inst::Jump(end);
                }
            }
            Node::Repeat {
                node,
                min,
                max,
                greedy,
            } => self.compile_repeat(node, *min, *max, *greedy)?,
        }
        Ok(())
    }

    fn compile_iteration(
        &mut self,
        node: &Node,
        captures: Option<(usize, usize)>,
    ) -> Result<(), String> {
        if let Some((low, high)) = captures {
            self.emit(Inst::Clear(low * 2, high * 2 + 1));
        }
        self.compile(node)
    }

    fn compile_repeat(
        &mut self,
        node: &Node,
        min: u32,
        max: Option<u32>,
        greedy: bool,
    ) -> Result<(), String> {
        let captures = group_range(node);
        let empty = can_be_empty(node);
        // the required iterations are copies of the body
        let copies = min.min(100_000);
        for _ in 0..copies {
            self.compile_iteration(node, captures)?;
            self.check()?;
        }
        match max {
            None => {
                // loop: split -> body -> (progress) -> jump loop
                let mark = self.marks;
                self.marks += 1;
                let split = self.emit(Inst::Split(0, 0));
                if empty {
                    self.emit(Inst::Mark(mark));
                }
                self.compile_iteration(node, captures)?;
                if empty {
                    self.emit(Inst::Progress(mark));
                }
                self.emit(Inst::Jump(split));
                let exit = self.program.len();
                self.program[split] = if greedy {
                    Inst::Split(split + 1, exit)
                } else {
                    Inst::Split(exit, split + 1)
                };
            }
            Some(max) => {
                let optional = max.saturating_sub(min);
                let mut splits = Vec::new();
                for _ in 0..optional {
                    let split = self.emit(Inst::Split(0, 0));
                    splits.push(split);
                    let mark = self.marks;
                    if empty {
                        self.marks += 1;
                        self.emit(Inst::Mark(mark));
                    }
                    self.compile_iteration(node, captures)?;
                    if empty {
                        self.emit(Inst::Progress(mark));
                    }
                    self.check()?;
                }
                let exit = self.program.len();
                for split in splits {
                    self.program[split] = if greedy {
                        Inst::Split(split + 1, exit)
                    } else {
                        Inst::Split(exit, split + 1)
                    };
                }
            }
        }
        Ok(())
    }
}

type ParsedPattern = (Node, usize, Vec<(String, Vec<usize>)>);

fn parse_pattern(pattern: &str, flags: Flags) -> Result<ParsedPattern, String> {
    // unicode mode reads the pattern by code point, otherwise by utf-16 unit
    let units: Vec<u32> = if flags.full_unicode() {
        pattern.chars().map(|character| character as u32).collect()
    } else {
        pattern.encode_utf16().map(u32::from).collect()
    };
    let (total_groups, has_names) = Parser::scan_groups(&units, flags.unicode_sets);
    let mut parser = Parser {
        pattern: &units,
        position: 0,
        flags,
        ignore_case: flags.ignore_case,
        multiline: flags.multiline,
        dot_all: flags.dot_all,
        groups: 0,
        total_groups,
        names: Vec::new(),
        named_refs: Vec::new(),
        has_names,
    };
    let (node, _) = parser.disjunction()?;
    if parser.position < units.len() {
        return Err(syntax(if units[parser.position] == ')' as u32 {
            "Unmatched ')'"
        } else {
            "Unexpected character"
        }));
    }
    for name in &parser.named_refs {
        if !parser.names.iter().any(|(existing, _)| existing == name) {
            return Err(syntax("Invalid named capture referenced"));
        }
    }
    Ok((node, parser.groups + 1, parser.names))
}

pub(super) fn validate(pattern: &str, flags: &str) -> Result<(), String> {
    let parsed = Flags::parse(flags)?;
    let _ = parse_pattern(pattern, parsed)?;
    Ok(())
}

impl Regex {
    pub(super) fn new(pattern: &str, flags: Flags) -> Result<Self, String> {
        let (node, groups, names) = parse_pattern(pattern, flags)?;
        let mut compiler = Compiler {
            program: Vec::new(),
            classes: Vec::new(),
            marks: 0,
            names: &names,
        };
        compiler.emit(Inst::Save(0));
        compiler.compile(&node)?;
        compiler.emit(Inst::Save(1));
        compiler.emit(Inst::Match);
        let program = compiler.program;
        let classes = compiler.classes;
        let marks = compiler.marks;
        Ok(Regex {
            program,
            classes,
            groups,
            names,
            marks,
            flags,
            ascii_units: std::cell::OnceCell::new(),
            ascii_split: std::cell::OnceCell::new(),
        })
    }

    // the ascii units a pattern of one character or one class matches, a bit each, so a scan
    // tests a unit with a shift; none for any other pattern
    pub(super) fn ascii_units(&self) -> Option<u128> {
        *self.ascii_units.get_or_init(|| {
            self.unit_match(0)?;
            Some((0..128u16).fold(0u128, |bits, unit| {
                bits | (u128::from(self.unit_match(unit) == Some(true)) << unit)
            }))
        })
    }

    // fast ascii split classifier for single unit/class or greedy `+` repeat (such as /[\/]/ or /\s+/)
    pub(super) fn ascii_split(&self) -> Option<(AsciiSplit, bool)> {
        *self.ascii_split.get_or_init(|| {
            if let Some(mask) = self.ascii_units() {
                return Some((AsciiSplit::from_mask(mask), false));
            }
            if self.flags.full_unicode() || self.groups != 1 {
                return None;
            }
            let mask = match self.program.as_slice() {
                [
                    Inst::Save(0),
                    Inst::Char(c1, ic1),
                    Inst::Split(3, 5),
                    Inst::Char(c2, ic2),
                    Inst::Jump(2),
                    Inst::Save(1),
                    Inst::Match,
                ] if c1 == c2 && ic1 == ic2 => {
                    let expected = *c1;
                    let ic = *ic1;
                    (0..128u32).fold(0u128, |bits, unit| {
                        let ok = unit == expected
                            || (ic
                                && self.canonical_with(unit, true)
                                    == self.canonical_with(expected, true));
                        bits | (u128::from(ok) << unit)
                    })
                }
                [
                    Inst::Save(0),
                    Inst::Class(i1),
                    Inst::Split(3, 5),
                    Inst::Class(i2),
                    Inst::Jump(2),
                    Inst::Save(1),
                    Inst::Match,
                ] if self.classes[*i1] == self.classes[*i2] => {
                    let class = &self.classes[*i1];
                    (0..128u32).fold(0u128, |bits, unit| {
                        let ok = self.class_matches(class, unit);
                        bits | (u128::from(ok) << unit)
                    })
                }
                _ => return None,
            };
            Some((AsciiSplit::from_mask(mask), true))
        })
    }

    // the first match at or after start (only at start when sticky)
    pub(super) fn find(
        &self,
        subject: &[u16],
        start: usize,
        sticky: bool,
    ) -> Result<Option<Match>, String> {
        let mut steps = 0u64;
        let mut position = start;
        while position <= subject.len() {
            if self.may_start_at(subject, position)
                && let Some(captures) = self.run(subject, position, &mut steps)?
            {
                return Ok(Some(Match { captures }));
            }
            if sticky {
                break;
            }
            position += 1;
            // unicode mode never starts inside a surrogate pair
            if self.flags.full_unicode()
                && position < subject.len()
                && (0xdc00..0xe000).contains(&subject[position])
                && (0xd800..0xdc00).contains(&subject[position - 1])
            {
                position += 1;
            }
        }
        Ok(None)
    }

    // whether a code unit is a whole match, for a pattern that is one character or one class
    // and no more, which a split can then find by scanning; none for any other pattern
    pub(super) fn unit_match(&self, unit: u16) -> Option<bool> {
        if self.flags.full_unicode() || self.groups != 1 {
            return None;
        }
        let value = u32::from(unit);
        match self.program.as_slice() {
            [
                Inst::Save(0),
                Inst::Char(expected, ic),
                Inst::Save(1),
                Inst::Match,
            ] => Some(
                value == *expected
                    || (*ic
                        && self.canonical_with(value, true)
                            == self.canonical_with(*expected, true)),
            ),
            [
                Inst::Save(0),
                Inst::Class(index),
                Inst::Save(1),
                Inst::Match,
            ] => Some(self.class_matches(&self.classes[*index], value)),
            _ => None,
        }
    }

    // a program whose first step reads one character can only match where that character
    // is, so find skips every other start without setting up a run there
    fn may_start_at(&self, subject: &[u16], position: usize) -> bool {
        let value = || self.decode(subject, position).map(|(value, _)| value);
        match self.program.get(1) {
            Some(Inst::Char(expected, ic)) => value().is_some_and(|value| {
                value == *expected
                    || (*ic
                        && self.canonical_with(value, true) == self.canonical_with(*expected, true))
            }),
            Some(Inst::Class(index)) => {
                value().is_some_and(|value| self.class_matches(&self.classes[*index], value))
            }
            _ => true,
        }
    }

    fn run(
        &self,
        subject: &[u16],
        start: usize,
        steps: &mut u64,
    ) -> Result<Option<Captures>, String> {
        let mut state = State {
            slots: vec![None; self.groups * 2],
            marks: vec![None; self.marks],
        };
        if self
            .execute(subject, 0, start, None, &mut state, steps)?
            .is_some()
        {
            let captures = (0..self.groups)
                .map(
                    |group| match (state.slots[group * 2], state.slots[group * 2 + 1]) {
                        (Some(start), Some(end)) => Some((start, end)),
                        _ => None,
                    },
                )
                .collect();
            return Ok(Some(captures));
        }
        Ok(None)
    }

    fn decode(&self, subject: &[u16], position: usize) -> Option<(u32, usize)> {
        let unit = *subject.get(position)?;
        if self.flags.full_unicode()
            && (0xd800..0xdc00).contains(&unit)
            && let Some(&low) = subject.get(position + 1)
            && (0xdc00..0xe000).contains(&low)
        {
            return Some((
                0x10000 + ((u32::from(unit) - 0xd800) << 10) + (u32::from(low) - 0xdc00),
                2,
            ));
        }
        Some((u32::from(unit), 1))
    }

    fn canonical_with(&self, value: u32, ignore_case: bool) -> u32 {
        if !ignore_case {
            return value;
        }
        canonicalize(value, self.flags.full_unicode())
    }

    fn class_matches(&self, class: &Class, value: u32) -> bool {
        let test = |value: u32| {
            class
                .ranges
                .iter()
                .any(|(start, end)| (*start..=*end).contains(&value))
                || class
                    .properties
                    .iter()
                    .any(|(property, negated)| has_property(*property, value) != *negated)
        };
        let mut found = test(value);
        if !found && class.ignore_case {
            found = case_variants(value, self.flags.full_unicode())
                .into_iter()
                .any(test);
        }
        found != class.negated
    }

    fn is_word(&self, subject: &[u16], position: usize, ignore_case: bool) -> bool {
        let Some(&unit) = subject.get(position) else {
            return false;
        };
        matches!(unit, 0x30..=0x39 | 0x41..=0x5a | 0x5f | 0x61..=0x7a)
            || (self.flags.full_unicode() && ignore_case && matches!(unit, 0x017f | 0x212a))
    }

    fn match_rgi_emoji(&self, subject: &[u16], position: usize) -> Option<usize> {
        let (first, w1) = self.decode(subject, position)?;
        // keycap sequence [0-9#*] u+fe0f u+20e3
        if matches!(first, 0x23 | 0x2a | 0x30..=0x39)
            && subject.get(position + w1) == Some(&0xfe0f)
            && subject.get(position + w1 + 1) == Some(&0x20e3)
        {
            return Some(position + w1 + 2);
        }
        // regional indicator flag pair
        if (0x1f1e6..=0x1f1ff).contains(&first)
            && let Some((second, w2)) = self.decode(subject, position + w1)
            && (0x1f1e6..=0x1f1ff).contains(&second)
        {
            return Some(position + w1 + w2);
        }
        if !is_emoji_base(first) {
            return None;
        }
        let mut pos = position + w1;
        // tag sequence such as black flag subdivisions
        if first == 0x1f3f4 {
            let mut p = pos;
            while let Some((cp, w)) = self.decode(subject, p) {
                if (0xe0020..=0xe007e).contains(&cp) {
                    p += w;
                } else {
                    break;
                }
            }
            if p > pos
                && let Some((0xe007f, w)) = self.decode(subject, p)
            {
                return Some(p + w);
            }
        }
        loop {
            if subject.get(pos) == Some(&0xfe0f) {
                pos += 1;
            }
            if let Some((cp, w)) = self.decode(subject, pos)
                && (0x1f3fb..=0x1f3ff).contains(&cp)
            {
                pos += w;
            }
            if subject.get(pos) == Some(&0x200d)
                && let Some((next_cp, nw)) = self.decode(subject, pos + 1)
                && is_emoji_base(next_cp)
            {
                pos += 1 + nw;
                continue;
            }
            break;
        }
        Some(pos)
    }

    fn is_line_terminator(value: u32) -> bool {
        matches!(value, 0x0a | 0x0d | 0x2028 | 0x2029)
    }

    // the backtracking loop; returns the end position of a match of the program from pc
    fn execute(
        &self,
        subject: &[u16],
        pc: usize,
        position: usize,
        must_end: Option<usize>,
        state: &mut State,
        steps: &mut u64,
    ) -> Result<Option<usize>, String> {
        enum Entry {
            Branch {
                pc: usize,
                position: usize,
                undo: usize,
            },
            Slot(usize, Option<usize>),
            Mark(usize, Option<usize>),
        }
        let mut stack: Vec<Entry> = Vec::new();
        let mut undo: Vec<Entry> = Vec::new();
        let (mut pc, mut position) = (pc, position);
        loop {
            *steps += 1;
            if *steps > MAX_STEPS {
                return Err(syntax("Regular expression is too complex to match"));
            }
            let mut fail = false;
            match self.program[pc] {
                Inst::Char(expected, ic) => match self.decode(subject, position) {
                    Some((value, width))
                        if value == expected
                            || (ic
                                && self.canonical_with(value, true)
                                    == self.canonical_with(expected, true)) =>
                    {
                        position += width;
                        pc += 1;
                    }
                    _ => fail = true,
                },
                Inst::Any(dot_all) => match self.decode(subject, position) {
                    Some((value, width)) if dot_all || !Self::is_line_terminator(value) => {
                        position += width;
                        pc += 1;
                    }
                    _ => fail = true,
                },
                Inst::Class(index) => match self.decode(subject, position) {
                    Some((value, width)) if self.class_matches(&self.classes[index], value) => {
                        position += width;
                        pc += 1;
                    }
                    _ => fail = true,
                },
                Inst::RgiEmoji => match self.match_rgi_emoji(subject, position) {
                    Some(end) => {
                        position = end;
                        pc += 1;
                    }
                    None => fail = true,
                },
                Inst::Split(first, second) => {
                    stack.push(Entry::Branch {
                        pc: second,
                        position,
                        undo: undo.len(),
                    });
                    pc = first;
                }
                Inst::Jump(target) => pc = target,
                Inst::Save(slot) => {
                    undo.push(Entry::Slot(slot, state.slots[slot]));
                    state.slots[slot] = Some(position);
                    pc += 1;
                }
                Inst::Clear(low, high) => {
                    for (slot, saved) in state.slots.iter_mut().enumerate().take(high + 1).skip(low)
                    {
                        if saved.is_some() {
                            undo.push(Entry::Slot(slot, saved.take()));
                        }
                    }
                    pc += 1;
                }
                Inst::Mark(mark) => {
                    undo.push(Entry::Mark(mark, state.marks[mark]));
                    state.marks[mark] = Some(position);
                    pc += 1;
                }
                Inst::Progress(mark) => {
                    if state.marks[mark] == Some(position) {
                        fail = true;
                    } else {
                        pc += 1;
                    }
                }
                Inst::Start(multiline) => {
                    let at_start = position == 0
                        || (multiline
                            && Self::is_line_terminator(u32::from(subject[position - 1])));
                    if at_start { pc += 1 } else { fail = true }
                }
                Inst::End(multiline) => {
                    let at_end = position == subject.len()
                        || (multiline && Self::is_line_terminator(u32::from(subject[position])));
                    if at_end { pc += 1 } else { fail = true }
                }
                Inst::Boundary(word, ic) => {
                    let before = position > 0 && self.is_word(subject, position - 1, ic);
                    let after = self.is_word(subject, position, ic);
                    if (before != after) == word {
                        pc += 1
                    } else {
                        fail = true
                    }
                }
                Inst::Backref(group, ic) => {
                    match (
                        state.slots.get(group * 2).copied().flatten(),
                        state.slots.get(group * 2 + 1).copied().flatten(),
                    ) {
                        (Some(start), Some(end)) if start <= end => {
                            let length = end - start;
                            let matches = position + length <= subject.len()
                                && (0..length).all(|offset| {
                                    let left = u32::from(subject[start + offset]);
                                    let right = u32::from(subject[position + offset]);
                                    left == right
                                        || (ic
                                            && self.canonical_with(left, true)
                                                == self.canonical_with(right, true))
                                });
                            if matches {
                                position += length;
                                pc += 1;
                            } else {
                                fail = true;
                            }
                        }
                        // an unset group matches the empty string
                        _ => pc += 1,
                    }
                }
                Inst::NamedBackref(name_idx, ic) => {
                    let active = self.names[name_idx].1.iter().find_map(|&group| {
                        match (
                            state.slots.get(group * 2).copied().flatten(),
                            state.slots.get(group * 2 + 1).copied().flatten(),
                        ) {
                            (Some(start), Some(end)) if start <= end => Some((start, end)),
                            _ => None,
                        }
                    });
                    match active {
                        Some((start, end)) => {
                            let length = end - start;
                            let matches = position + length <= subject.len()
                                && (0..length).all(|offset| {
                                    let left = u32::from(subject[start + offset]);
                                    let right = u32::from(subject[position + offset]);
                                    left == right
                                        || (ic
                                            && self.canonical_with(left, true)
                                                == self.canonical_with(right, true))
                                });
                            if matches {
                                position += length;
                                pc += 1;
                            } else {
                                fail = true;
                            }
                        }
                        None => pc += 1,
                    }
                }
                Inst::Look {
                    end,
                    behind,
                    negate,
                } => {
                    let saved = state.slots.clone();
                    let found = if behind {
                        let mut found = false;
                        for begin in (0..=position).rev() {
                            if self
                                .execute(subject, pc + 1, begin, Some(position), state, steps)?
                                .is_some()
                            {
                                found = true;
                                break;
                            }
                            state.slots.clone_from(&saved);
                        }
                        found
                    } else {
                        self.execute(subject, pc + 1, position, None, state, steps)?
                            .is_some()
                    };
                    if found != negate {
                        if negate {
                            state.slots = saved;
                        } else {
                            // keep the captures the assertion set, undoable as a unit
                            for (slot, old) in saved.into_iter().enumerate() {
                                if state.slots[slot] != old {
                                    undo.push(Entry::Slot(slot, old));
                                }
                            }
                        }
                        pc = end;
                    } else {
                        state.slots = saved;
                        fail = true;
                    }
                }
                Inst::Match => {
                    if must_end.is_none_or(|end| end == position) {
                        return Ok(Some(position));
                    }
                    fail = true;
                }
            }
            if fail {
                loop {
                    match stack.pop() {
                        None => return Ok(None),
                        Some(Entry::Branch {
                            pc: next,
                            position: at,
                            undo: depth,
                        }) => {
                            while undo.len() > depth {
                                match undo.pop() {
                                    Some(Entry::Slot(slot, old)) => state.slots[slot] = old,
                                    Some(Entry::Mark(mark, old)) => state.marks[mark] = old,
                                    _ => {}
                                }
                            }
                            pc = next;
                            position = at;
                            break;
                        }
                        Some(_) => {}
                    }
                }
            }
        }
    }
}

struct State {
    slots: Vec<Option<usize>>,
    marks: Vec<Option<usize>>,
}

// Canonicalize: simple uppercase outside unicode mode (never mapping non-ASCII to ASCII),
// simple case folding in unicode mode
fn canonicalize(value: u32, unicode: bool) -> u32 {
    if unicode {
        if value == 0x017f {
            return b's' as u32;
        }
        if value == 0x212a {
            return b'k' as u32;
        }
    }
    let Some(character) = char::from_u32(value) else {
        return value;
    };
    if unicode {
        let mut lower = character.to_lowercase();
        return match (lower.next(), lower.next()) {
            (Some(single), None) => single as u32,
            _ => value,
        };
    }
    let mut upper = character.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(single), None) if !(value >= 128 && (single as u32) < 128) => single as u32,
        _ => value,
    }
}

fn case_variants(value: u32, unicode: bool) -> Vec<u32> {
    let Some(character) = char::from_u32(value) else {
        return Vec::new();
    };
    let mut out: Vec<u32> = character
        .to_lowercase()
        .chain(character.to_uppercase())
        .map(|variant| variant as u32)
        .filter(|variant| *variant != value && (unicode || !(value >= 128 && *variant < 128)))
        .collect();
    if unicode {
        match value {
            0x53 | 0x73 => out.push(0x017f),
            0x017f => {
                out.push(0x53);
                out.push(0x73);
            }
            0x4b | 0x6b => out.push(0x212a),
            0x212a => {
                out.push(0x4b);
                out.push(0x6b);
            }
            _ => {}
        }
    }
    out
}

fn is_emoji_base(cp: u32) -> bool {
    matches!(
        cp,
        0x00a9
            | 0x00ae
            | 0x203c
            | 0x2049
            | 0x2122
            | 0x2139
            | 0x2194..=0x2199
            | 0x21a9..=0x21aa
            | 0x231a..=0x231b
            | 0x2328
            | 0x23cf
            | 0x23e9..=0x23f3
            | 0x23f8..=0x23fa
            | 0x24c2
            | 0x25aa..=0x25ab
            | 0x25b6
            | 0x25c0
            | 0x25fb..=0x25fe
            | 0x2600..=0x27bf
            | 0x2934..=0x2935
            | 0x2b05..=0x2b07
            | 0x2b1b..=0x2b1c
            | 0x2b50
            | 0x2b55
            | 0x3030
            | 0x303d
            | 0x3297
            | 0x3299
            | 0x1f000..=0x1fbff
    )
}

fn has_property(property: Property, value: u32) -> bool {
    let Some(character) = char::from_u32(value) else {
        return matches!(property, Property::Any);
    };
    match property {
        Property::Letter => character.is_alphabetic() && !character.is_numeric(),
        Property::Uppercase => character.is_uppercase(),
        Property::Lowercase => character.is_lowercase(),
        Property::Number => character.is_numeric(),
        Property::Digit => {
            character.is_ascii_digit()
                || ('\u{660}'..='\u{669}').contains(&character)
                || ('\u{6f0}'..='\u{6f9}').contains(&character)
                || ('\u{966}'..='\u{96f}').contains(&character)
                || ('\u{ff10}'..='\u{ff19}').contains(&character)
        }
        Property::Alphabetic => character.is_alphabetic(),
        Property::WhiteSpace => character.is_whitespace(),
        Property::Punctuation => {
            character.is_ascii_punctuation()
                || matches!(value, 0xa1 | 0xa7 | 0xab | 0xb6 | 0xb7 | 0xbb | 0xbf | 0x2010..=0x2027 | 0x2030..=0x205e | 0x3001..=0x3003 | 0x3008..=0x3011)
        }
        Property::Any => true,
        Property::Ascii => value < 128,
        Property::AsciiHexDigit => matches!(value, 0x30..=0x39 | 0x41..=0x46 | 0x61..=0x66),
        Property::Emoji => is_emoji_base(value),
    }
}

// the source text as RegExp.prototype.source shows it
pub(super) fn escape_source(source: &str) -> String {
    if source.is_empty() {
        return "(?:)".to_string();
    }
    let mut escaped = String::with_capacity(source.len());
    let mut in_class = false;
    let mut characters = source.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\\' => {
                escaped.push('\\');
                if let Some(next) = characters.next() {
                    escaped.push(next);
                }
            }
            '[' => {
                in_class = true;
                escaped.push('[');
            }
            ']' => {
                in_class = false;
                escaped.push(']');
            }
            '/' if !in_class => escaped.push_str("\\/"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\u{2028}' => escaped.push_str("\\u2028"),
            '\u{2029}' => escaped.push_str("\\u2029"),
            other => escaped.push(other),
        }
    }
    escaped
}

pub(super) struct RegExpData {
    pub(super) source: String,
    pub(super) regex: Rc<Regex>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first(pattern: &str, flags: &str, subject: &str) -> Option<Vec<Option<String>>> {
        let regex = Regex::new(pattern, Flags::parse(flags).unwrap()).unwrap();
        let units: Vec<u16> = subject.encode_utf16().collect();
        regex.find(&units, 0, false).unwrap().map(|found| {
            found
                .captures
                .iter()
                .map(|capture| {
                    capture.map(|(start, end)| String::from_utf16_lossy(&units[start..end]))
                })
                .collect()
        })
    }

    fn whole(pattern: &str, flags: &str, subject: &str) -> Option<String> {
        first(pattern, flags, subject).and_then(|captures| captures[0].clone())
    }

    #[test]
    fn matches_the_core_syntax() {
        assert_eq!(whole("a+b", "", "xaaab"), Some("aaab".to_string()));
        assert_eq!(whole("a+?", "", "aaa"), Some("a".to_string()));
        assert_eq!(whole("\\d{2,3}", "", "a12345"), Some("123".to_string()));
        assert_eq!(
            whole("^\\w+@\\w+\\.com$", "", "me@site.com"),
            Some("me@site.com".to_string())
        );
        assert_eq!(whole("colou?r", "", "color"), Some("color".to_string()));
        assert_eq!(whole("[^a-c]+", "", "abcdef"), Some("def".to_string()));
        assert_eq!(whole("cat|dog", "", "hotdog"), Some("dog".to_string()));
        assert_eq!(whole("HELLO", "i", "say hello"), Some("hello".to_string()));
        assert_eq!(whole("^b", "m", "a\nb"), Some("b".to_string()));
        assert_eq!(whole("a.c", "", "a\nc"), None);
        assert_eq!(whole("a.c", "s", "a\nc"), Some("a\nc".to_string()));
        assert_eq!(whole("\\bfoo\\b", "", "a foo b"), Some("foo".to_string()));
        assert_eq!(whole("x{2}", "", "xxx"), Some("xx".to_string()));
        assert_eq!(whole("a{", "", "a{"), Some("a{".to_string()));
    }

    #[test]
    fn captures_lookaround_and_backreferences_work() {
        assert_eq!(
            first("(\\d+)-(\\d+)", "", "tel 12-34"),
            Some(vec![
                Some("12-34".into()),
                Some("12".into()),
                Some("34".into())
            ])
        );
        assert_eq!(
            first("(a)|(b)", "", "b"),
            Some(vec![Some("b".into()), None, Some("b".into())])
        );
        assert_eq!(whole("(\\w)\\1", "", "abccd"), Some("cc".to_string()));
        assert_eq!(
            whole("(?<q>['\"]).*?\\k<q>", "", "say 'hi' now"),
            Some("'hi'".to_string())
        );
        assert_eq!(whole("\\d+(?=px)", "", "10em 20px"), Some("20".to_string()));
        assert_eq!(
            whole("\\d+(?!px)\\b", "", "20px 30 em"),
            Some("30".to_string())
        );
        assert_eq!(
            whole("(?<=\\$)\\d+", "", "cost $42"),
            Some("42".to_string())
        );
        assert_eq!(whole("(?<!\\$)\\b\\d+", "", "$42 7"), Some("7".to_string()));
        assert_eq!(
            first("(a*)*b", "", "aab"),
            Some(vec![Some("aab".into()), Some("aa".into())])
        );
        assert_eq!(
            first("(?:(a)|b)+", "", "ab"),
            Some(vec![Some("ab".into()), None])
        );
    }

    #[test]
    fn unicode_mode_reads_code_points() {
        assert_eq!(
            whole("^.$", "u", "\u{1f600}"),
            Some("\u{1f600}".to_string())
        );
        assert_eq!(whole("^.$", "", "\u{1f600}"), None);
        assert_eq!(whole("\\p{Lu}+", "u", "abcDEF"), Some("DEF".to_string()));
        assert_eq!(
            whole("\\u{1F600}", "u", "x\u{1f600}"),
            Some("\u{1f600}".to_string())
        );
        assert!(Regex::new("\\p{Lu}", Flags::parse("").unwrap()).is_ok());
        assert!(Regex::new("(", Flags::default()).is_err());
        assert!(Regex::new("a**", Flags::default()).is_err());
        assert!(Flags::parse("gg").is_err());
    }

    #[test]
    fn catastrophic_patterns_stop_at_the_step_limit() {
        let regex = Regex::new("(a+)+$", Flags::default()).unwrap();
        let subject: Vec<u16> = format!("{}b", "a".repeat(40)).encode_utf16().collect();
        assert!(regex.find(&subject, 0, false).is_err());
    }
}
