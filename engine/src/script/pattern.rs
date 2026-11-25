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
    Emoji,
}

#[derive(Clone, Debug, Default)]
struct Class {
    ranges: Vec<(u32, u32)>,
    properties: Vec<(Property, bool)>,
    negated: bool,
}

#[derive(Clone, Debug)]
enum Node {
    Empty,
    Char(u32),
    Any,
    Class(Class),
    Start,
    End,
    Boundary(bool),
    Group(Box<Node>, Option<usize>),
    Look {
        node: Box<Node>,
        behind: bool,
        negate: bool,
    },
    Backref(usize),
    NamedBackref(String),
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
    Char(u32),
    Any,
    Class(usize),
    Split(usize, usize),
    Jump(usize),
    Save(usize),
    Start,
    End,
    Boundary(bool),
    Backref(usize),
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

pub(super) struct Regex {
    program: Vec<Inst>,
    classes: Vec<Class>,
    // capture groups, group 0 included
    pub(super) groups: usize,
    pub(super) names: Vec<(String, usize)>,
    marks: usize,
    pub(super) flags: Flags,
    // which ascii units a one-character pattern matches, worked out on the first split by it
    ascii_units: std::cell::OnceCell<Option<u128>>,
    // which ascii units a greedy one-or-more (+) char/class pattern matches
    ascii_repeat_units: std::cell::OnceCell<Option<u128>>,
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
    groups: usize,
    names: Vec<(String, usize)>,
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

    fn disjunction(&mut self) -> Result<Node, String> {
        let mut alternatives = vec![self.alternative()?];
        while self.eat('|') {
            alternatives.push(self.alternative()?);
        }
        Ok(if alternatives.len() == 1 {
            alternatives.pop().expect("one alternative")
        } else {
            Node::Alternate(alternatives)
        })
    }

    fn alternative(&mut self) -> Result<Node, String> {
        let mut terms = Vec::new();
        while let Some(character) = self.peek_char() {
            if character == '|' || character == ')' {
                break;
            }
            terms.push(self.term()?);
        }
        Ok(match terms.len() {
            0 => Node::Empty,
            1 => terms.pop().expect("one term"),
            _ => Node::Concat(terms),
        })
    }

    fn term(&mut self) -> Result<Node, String> {
        let atom = match self.peek_char() {
            Some('^') => {
                self.position += 1;
                return Ok(Node::Start);
            }
            Some('$') => {
                self.position += 1;
                return Ok(Node::End);
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
                return Ok(Node::Boundary(!negated));
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
                let node = self.disjunction()?;
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
                    return Ok(look);
                }
                look
            }
            _ => self.atom()?,
        };
        self.quantifier(atom)
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
        if matches!(atom, Node::Start | Node::End | Node::Boundary(_)) {
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

    fn atom(&mut self) -> Result<Node, String> {
        let character = self
            .peek()
            .ok_or_else(|| syntax("Unexpected end of pattern"))?;
        match char::from_u32(character) {
            Some('.') => {
                self.position += 1;
                Ok(Node::Any)
            }
            Some('(') => {
                self.position += 1;
                if self.eat('?') {
                    if self.eat(':') {
                        let node = self.disjunction()?;
                        if !self.eat(')') {
                            return Err(syntax("Unterminated group"));
                        }
                        return Ok(Node::Group(Box::new(node), None));
                    }
                    if self.eat('<') {
                        let name = self.group_name()?;
                        self.groups += 1;
                        let index = self.groups;
                        if self.names.iter().any(|(existing, _)| *existing == name) {
                            return Err(syntax("Duplicate capture group name"));
                        }
                        self.names.push((name, index));
                        let node = self.disjunction()?;
                        if !self.eat(')') {
                            return Err(syntax("Unterminated group"));
                        }
                        return Ok(Node::Group(Box::new(node), Some(index)));
                    }
                    return Err(syntax("Invalid group"));
                }
                self.groups += 1;
                let index = self.groups;
                let node = self.disjunction()?;
                if !self.eat(')') {
                    return Err(syntax("Unterminated group"));
                }
                Ok(Node::Group(Box::new(node), Some(index)))
            }
            Some(')') => Err(syntax("Unmatched ')'")),
            Some('[') => {
                self.position += 1;
                Ok(Node::Class(self.class()?))
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
                Ok(Node::Char('{' as u32))
            }
            Some('}' | ']') if self.flags.full_unicode() => Err(syntax("Lone quantifier brackets")),
            Some('\\') => {
                self.position += 1;
                self.atom_escape()
            }
            _ => {
                self.position += 1;
                Ok(Node::Char(character))
            }
        }
    }

    fn group_name(&mut self) -> Result<String, String> {
        let mut name = String::new();
        loop {
            match self.peek_char() {
                Some('>') => {
                    self.position += 1;
                    break;
                }
                Some(character)
                    if character == '$' || character == '_' || character.is_alphanumeric() =>
                {
                    name.push(character);
                    self.position += 1;
                }
                _ => return Err(syntax("Invalid capture group name")),
            }
        }
        if name.is_empty()
            || name
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_digit())
        {
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
                if value <= self.total_groups() || self.flags.full_unicode() {
                    if value > self.total_groups() {
                        return Err(syntax("Invalid escape"));
                    }
                    return Ok(Node::Backref(value));
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
                Ok(Node::NamedBackref(name))
            }
            'd' | 'D' | 'w' | 'W' | 's' | 'S' => {
                let class = self.class_escape()?.expect("a class escape");
                Ok(Node::Class(class))
            }
            'p' | 'P' if self.flags.full_unicode() => {
                let class = self.class_escape()?.expect("a property escape");
                Ok(Node::Class(class))
            }
            _ => Ok(Node::Char(self.character_escape(false)?)),
        }
    }

    // the number of capture groups in the whole pattern, for deciding what \N means
    fn total_groups(&self) -> usize {
        let mut count = 0;
        let mut index = 0;
        let mut in_class = false;
        while index < self.pattern.len() {
            match char::from_u32(self.pattern[index]) {
                Some('\\') => index += 1,
                Some('[') => in_class = true,
                Some(']') => in_class = false,
                Some('(') if !in_class => {
                    let next = self
                        .pattern
                        .get(index + 1)
                        .copied()
                        .and_then(char::from_u32);
                    let after = self
                        .pattern
                        .get(index + 2)
                        .copied()
                        .and_then(char::from_u32);
                    if next != Some('?')
                        || (after == Some('<')
                            && !matches!(
                                self.pattern
                                    .get(index + 3)
                                    .copied()
                                    .and_then(char::from_u32),
                                Some('=' | '!')
                            ))
                    {
                        count += 1;
                    }
                }
                _ => {}
            }
            index += 1;
        }
        count
    }

    fn legacy_octal_or_identity(&mut self) -> Result<Node, String> {
        let first = self
            .peek_char()
            .and_then(|character| character.to_digit(10))
            .unwrap_or(0);
        if first >= 8 {
            self.position += 1;
            return Ok(Node::Char('0' as u32 + first));
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
        Ok(Node::Char(value))
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
                    Node::Char(value) => value,
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
                if is_syntax_character(other as u32) || other == '-' {
                    other as u32
                } else {
                    return Err(syntax("Invalid escape"));
                }
            }
            other => other as u32,
        })
    }

    fn property(&mut self) -> Result<(Property, bool), String> {
        if !self.eat('{') {
            return Err(syntax("Invalid property name"));
        }
        let mut name = String::new();
        while let Some(character) = self.peek_char() {
            self.position += 1;
            if character == '}' {
                let property = match name.as_str() {
                    "L" | "Letter" => Property::Letter,
                    "Lu" | "Uppercase_Letter" | "Uppercase" => Property::Uppercase,
                    "Ll" | "Lowercase_Letter" | "Lowercase" => Property::Lowercase,
                    "N" | "Number" => Property::Number,
                    "Nd" | "Decimal_Number" | "digit" => Property::Digit,
                    "Alphabetic" | "Alpha" => Property::Alphabetic,
                    "White_Space" | "space" => Property::WhiteSpace,
                    "P" | "Punctuation" | "punct" => Property::Punctuation,
                    "Any" => Property::Any,
                    "ASCII" => Property::Ascii,
                    "Emoji" | "Extended_Pictographic" | "Emoji_Presentation" => Property::Emoji,
                    _ => return Err(syntax(format!("Invalid property name {name}"))),
                };
                return Ok((property, false));
            }
            name.push(character);
        }
        Err(syntax("Invalid property name"))
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
                }));
            }
            _ => return Ok(None),
        };
        self.position += 1;
        Ok(Some(Class {
            ranges: if negated { complement(&ranges) } else { ranges },
            properties: Vec::new(),
            negated: false,
        }))
    }

    fn class(&mut self) -> Result<Class, String> {
        let mut class = Class {
            negated: self.eat('^'),
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
    names: &'a [(String, usize)],
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
        Node::Char(_) | Node::Any | Node::Class(_) => false,
        Node::Group(inner, _) => can_be_empty(inner),
        Node::Concat(nodes) => nodes.iter().all(can_be_empty),
        Node::Alternate(nodes) => nodes.iter().any(can_be_empty),
        Node::Repeat { node, min, .. } => *min == 0 || can_be_empty(node),
        Node::Backref(_) | Node::NamedBackref(_) => true,
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
            Node::Char(value) => {
                self.emit(Inst::Char(*value));
            }
            Node::Any => {
                self.emit(Inst::Any);
            }
            Node::Class(class) => {
                self.classes.push(class.clone());
                let index = self.classes.len() - 1;
                self.emit(Inst::Class(index));
            }
            Node::Start => {
                self.emit(Inst::Start);
            }
            Node::End => {
                self.emit(Inst::End);
            }
            Node::Boundary(word) => {
                self.emit(Inst::Boundary(*word));
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
            Node::Backref(index) => {
                self.emit(Inst::Backref(*index));
            }
            Node::NamedBackref(name) => {
                let index = self
                    .names
                    .iter()
                    .find(|(candidate, _)| candidate == name)
                    .map(|(_, index)| *index)
                    .ok_or_else(|| syntax("Invalid named capture referenced"))?;
                self.emit(Inst::Backref(index));
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

impl Regex {
    pub(super) fn new(pattern: &str, flags: Flags) -> Result<Self, String> {
        // unicode mode reads the pattern by code point, otherwise by UTF-16 unit
        let pattern: Vec<u32> = if flags.full_unicode() {
            pattern.chars().map(|character| character as u32).collect()
        } else {
            pattern.encode_utf16().map(u32::from).collect()
        };
        let mut parser = Parser {
            pattern: &pattern,
            position: 0,
            flags,
            groups: 0,
            names: Vec::new(),
            has_names: false,
        };
        parser.has_names = {
            let text: String = pattern
                .iter()
                .filter_map(|unit| char::from_u32(*unit))
                .collect();
            text.contains("(?<") && text.replace("(?<=", "").replace("(?<!", "").contains("(?<")
        };
        let node = parser.disjunction()?;
        if parser.position < pattern.len() {
            return Err(syntax(if pattern[parser.position] == ')' as u32 {
                "Unmatched ')'"
            } else {
                "Unexpected character"
            }));
        }
        let groups = parser.groups + 1;
        let names = parser.names;
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
            ascii_repeat_units: std::cell::OnceCell::new(),
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

    // the ascii units a greedy `+` repeat of one character or one class matches (such as /\s+/)
    pub(super) fn ascii_repeat_units(&self) -> Option<u128> {
        *self.ascii_repeat_units.get_or_init(|| {
            if self.flags.full_unicode() || self.groups != 1 {
                return None;
            }
            match self.program.as_slice() {
                [
                    Inst::Save(0),
                    Inst::Char(c1),
                    Inst::Split(3, 5),
                    Inst::Char(c2),
                    Inst::Jump(2),
                    Inst::Save(1),
                    Inst::Match,
                ] if c1 == c2 => {
                    let expected = *c1;
                    Some((0..128u32).fold(0u128, |bits, unit| {
                        let ok =
                            unit == expected || self.canonical(unit) == self.canonical(expected);
                        bits | (u128::from(ok) << unit)
                    }))
                }
                [
                    Inst::Save(0),
                    Inst::Class(i1),
                    Inst::Split(3, 5),
                    Inst::Class(i2),
                    Inst::Jump(2),
                    Inst::Save(1),
                    Inst::Match,
                ] if i1 == i2 => {
                    let class = &self.classes[*i1];
                    Some((0..128u32).fold(0u128, |bits, unit| {
                        let ok = self.class_matches(class, unit);
                        bits | (u128::from(ok) << unit)
                    }))
                }
                _ => None,
            }
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
                Inst::Char(expected),
                Inst::Save(1),
                Inst::Match,
            ] => Some(value == *expected || self.canonical(value) == self.canonical(*expected)),
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
            Some(Inst::Char(expected)) => value().is_some_and(|value| {
                value == *expected || self.canonical(value) == self.canonical(*expected)
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

    fn canonical(&self, value: u32) -> u32 {
        if !self.flags.ignore_case {
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
        if !found && self.flags.ignore_case {
            found = case_variants(value).into_iter().any(test);
        }
        found != class.negated
    }

    fn is_word(&self, subject: &[u16], position: usize) -> bool {
        subject
            .get(position)
            .is_some_and(|unit| matches!(*unit, 0x30..=0x39 | 0x41..=0x5a | 0x5f | 0x61..=0x7a))
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
                Inst::Char(expected) => match self.decode(subject, position) {
                    Some((value, width))
                        if value == expected
                            || self.canonical(value) == self.canonical(expected) =>
                    {
                        position += width;
                        pc += 1;
                    }
                    _ => fail = true,
                },
                Inst::Any => match self.decode(subject, position) {
                    Some((value, width))
                        if self.flags.dot_all || !Self::is_line_terminator(value) =>
                    {
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
                Inst::Start => {
                    let at_start = position == 0
                        || (self.flags.multiline
                            && Self::is_line_terminator(u32::from(subject[position - 1])));
                    if at_start { pc += 1 } else { fail = true }
                }
                Inst::End => {
                    let at_end = position == subject.len()
                        || (self.flags.multiline
                            && Self::is_line_terminator(u32::from(subject[position])));
                    if at_end { pc += 1 } else { fail = true }
                }
                Inst::Boundary(word) => {
                    let before = position > 0 && self.is_word(subject, position - 1);
                    let after = self.is_word(subject, position);
                    if (before != after) == word {
                        pc += 1
                    } else {
                        fail = true
                    }
                }
                Inst::Backref(group) => {
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
                                    left == right || self.canonical(left) == self.canonical(right)
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

fn case_variants(value: u32) -> Vec<u32> {
    let Some(character) = char::from_u32(value) else {
        return Vec::new();
    };
    character
        .to_lowercase()
        .chain(character.to_uppercase())
        .map(|variant| variant as u32)
        .filter(|variant| *variant != value)
        .collect()
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
        Property::Emoji => matches!(value, 0x1f000..=0x1faff | 0x2600..=0x27bf | 0x2b00..=0x2bff),
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
