// an on-demand ECMAScript scanner: the parser decides whether a slash starts a regular
// expression and when a closing brace continues a template, so both are rescanned on request

use super::ScriptError;
use std::rc::Rc;

#[derive(Clone, Debug, PartialEq)]
pub(super) enum TokenKind {
    Eof,
    Identifier(String),
    // an identifier written with a unicode escape, which can never be a keyword
    EscapedIdentifier(String),
    PrivateName(String),
    Punct(&'static str),
    Number(f64),
    // a BigInt literal, as its decimal digits
    BigInt(Rc<str>),
    String(String),
    // cooked text is None when a tagged template holds an invalid escape
    Template {
        cooked: Option<String>,
        raw: String,
        tail: bool,
    },
    Regex {
        pattern: String,
        flags: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Token {
    pub(super) kind: TokenKind,
    pub(super) start: usize,
    pub(super) end: usize,
    // a line terminator sits between this token and the previous one
    pub(super) newline_before: bool,
}

// longest punctuators first, so the scanner always takes the longest match
const PUNCTUATORS: [&str; 58] = [
    ">>>=", "...", "===", "!==", "**=", "<<=", ">>=", ">>>", "&&=", "||=", "??=", "=>", "==", "!=",
    "<=", ">=", "&&", "||", "??", "?.", "++", "--", "+=", "-=", "*=", "%=", "&=", "|=", "^=", "**",
    "<<", ">>", "{", "}", "(", ")", "[", "]", ";", ",", "<", ">", "+", "-", "*", "%", "&", "|",
    "^", "!", "~", "?", ":", "=", ".", "@", "/=", "/",
];

pub(super) struct Lexer<'a> {
    source: &'a str,
    position: usize,
}

fn is_line_terminator(character: char) -> bool {
    matches!(character, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn is_identifier_start(character: char) -> bool {
    character == '$' || character == '_' || character.is_alphabetic()
}

fn is_identifier_part(character: char) -> bool {
    is_identifier_start(character)
        || character.is_alphanumeric()
        || matches!(character, '\u{200c}' | '\u{200d}')
        || is_combining_mark(character)
}

// the combining marks and connector punctuation that identifiers may continue with
fn is_combining_mark(character: char) -> bool {
    matches!(
        character as u32,
        0x0300..=0x036f | 0x1ab0..=0x1aff | 0x1dc0..=0x1dff | 0x20d0..=0x20ff | 0xfe20..=0xfe2f
            | 0x203f | 0x2040 | 0x2054 | 0xfe33 | 0xfe34 | 0xfe4d..=0xfe4f | 0xff3f
    )
}

impl<'a> Lexer<'a> {
    pub(super) fn new(source: &'a str) -> Self {
        // a hashbang comment is only allowed as the very first characters
        let position = if source.starts_with("#!") {
            source.find(is_line_terminator).unwrap_or(source.len())
        } else {
            0
        };
        Self { source, position }
    }

    pub(super) fn source(&self) -> &'a str {
        self.source
    }

    fn error(&self, offset: usize, message: impl Into<String>) -> ScriptError {
        ScriptError {
            offset,
            message: message.into(),
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.source[self.position..].chars().next()
    }

    fn peek_char_at(&self, offset: usize) -> Option<char> {
        self.source.get(self.position + offset..)?.chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let character = self.peek_char()?;
        self.position += character.len_utf8();
        Some(character)
    }

    // skips whitespace and comments, reporting whether a line terminator was crossed
    fn skip_trivia(&mut self) -> Result<bool, ScriptError> {
        let mut newline = false;
        while let Some(character) = self.peek_char() {
            if is_line_terminator(character) {
                newline = true;
                self.bump();
            } else if character.is_whitespace() || character == '\u{feff}' {
                self.bump();
            } else if self.source[self.position..].starts_with("//")
                || self.source[self.position..].starts_with("<!--")
                || (newline || self.position == 0)
                    && self.source[self.position..].starts_with("-->")
            {
                while let Some(character) = self.peek_char() {
                    if is_line_terminator(character) {
                        break;
                    }
                    self.bump();
                }
            } else if self.source[self.position..].starts_with("/*") {
                let start = self.position;
                let Some(end) = self.source[self.position + 2..].find("*/") else {
                    return Err(self.error(start, "unterminated comment"));
                };
                let body = &self.source[self.position + 2..self.position + 2 + end];
                newline |= body.contains(is_line_terminator);
                self.position += 2 + end + 2;
            } else {
                break;
            }
        }
        Ok(newline)
    }

    pub(super) fn next_token(&mut self) -> Result<Token, ScriptError> {
        let newline_before = self.skip_trivia()?;
        let start = self.position;
        let Some(character) = self.peek_char() else {
            return Ok(Token {
                kind: TokenKind::Eof,
                start,
                end: start,
                newline_before,
            });
        };
        let kind = if is_identifier_start(character) || character == '\\' {
            let (name, escaped) = self.scan_identifier_name()?;
            if escaped {
                TokenKind::EscapedIdentifier(name)
            } else {
                TokenKind::Identifier(name)
            }
        } else if character == '#'
            && self
                .peek_char_at(1)
                .is_some_and(|next| is_identifier_start(next) || next == '\\')
        {
            self.bump();
            TokenKind::PrivateName(self.scan_identifier_name()?.0)
        } else if character.is_ascii_digit()
            || character == '.'
                && self
                    .peek_char_at(1)
                    .is_some_and(|next| next.is_ascii_digit())
        {
            self.scan_number()?
        } else if character == '"' || character == '\'' {
            TokenKind::String(self.scan_string(character)?)
        } else if character == '`' {
            self.bump();
            self.scan_template_part(start)?
        } else {
            self.scan_punctuator(start)?
        };
        Ok(Token {
            kind,
            start,
            end: self.position,
            newline_before,
        })
    }

    // rescans from a token start as a regular expression literal
    pub(super) fn rescan_regex(&mut self, start: usize) -> Result<Token, ScriptError> {
        self.position = start + 1;
        let mut pattern = String::new();
        let mut in_class = false;
        loop {
            let Some(character) = self.bump() else {
                return Err(self.error(start, "unterminated regular expression"));
            };
            if is_line_terminator(character) {
                return Err(self.error(start, "unterminated regular expression"));
            }
            match character {
                '\\' => {
                    pattern.push(character);
                    match self.bump() {
                        Some(next) if !is_line_terminator(next) => pattern.push(next),
                        _ => return Err(self.error(start, "unterminated regular expression")),
                    }
                    continue;
                }
                '[' => in_class = true,
                ']' => in_class = false,
                '/' if !in_class => break,
                _ => {}
            }
            pattern.push(character);
        }
        let flags_start = self.position;
        while self.peek_char().is_some_and(is_identifier_part) {
            self.bump();
        }
        Ok(Token {
            kind: TokenKind::Regex {
                pattern,
                flags: self.source[flags_start..self.position].to_string(),
            },
            start,
            end: self.position,
            newline_before: false,
        })
    }

    // rescans from a closing brace as the middle or tail of a template literal
    pub(super) fn rescan_template_continuation(
        &mut self,
        start: usize,
    ) -> Result<Token, ScriptError> {
        self.position = start + 1;
        let kind = self.scan_template_part(start)?;
        Ok(Token {
            kind,
            start,
            end: self.position,
            newline_before: false,
        })
    }

    fn scan_identifier_name(&mut self) -> Result<(String, bool), ScriptError> {
        let mut name = String::new();
        let mut escaped = false;
        let start = self.position;
        loop {
            match self.peek_char() {
                Some('\\') => {
                    self.bump();
                    if self.bump() != Some('u') {
                        return Err(self.error(start, "invalid identifier escape"));
                    }
                    let character = self.scan_unicode_escape(start)?;
                    if !(if name.is_empty() {
                        is_identifier_start(character)
                    } else {
                        is_identifier_part(character)
                    }) {
                        return Err(self.error(start, "invalid identifier escape"));
                    }
                    name.push(character);
                    escaped = true;
                }
                Some(character)
                    if if name.is_empty() {
                        is_identifier_start(character)
                    } else {
                        is_identifier_part(character)
                    } =>
                {
                    self.bump();
                    name.push(character);
                }
                _ => break,
            }
        }
        if name.is_empty() {
            return Err(self.error(start, "expected identifier"));
        }
        Ok((name, escaped))
    }

    // after "\u": either four hex digits or a braced code point; surrogate pairs combine
    fn scan_unicode_escape(&mut self, start: usize) -> Result<char, ScriptError> {
        let unit = self.scan_unicode_unit(start)?;
        if (0xd800..0xdc00).contains(&unit) && self.source[self.position..].starts_with("\\u") {
            let saved = self.position;
            self.position += 2;
            if let Ok(low) = self.scan_unicode_unit(start)
                && (0xdc00..0xe000).contains(&low)
            {
                let combined = 0x10000 + ((unit - 0xd800) << 10) + (low - 0xdc00);
                return Ok(char::from_u32(combined).unwrap_or('\u{fffd}'));
            }
            self.position = saved;
        }
        Ok(char::from_u32(unit).unwrap_or('\u{fffd}'))
    }

    fn scan_unicode_unit(&mut self, start: usize) -> Result<u32, ScriptError> {
        if self.peek_char() == Some('{') {
            self.bump();
            let digits_start = self.position;
            while self
                .peek_char()
                .is_some_and(|character| character.is_ascii_hexdigit())
            {
                self.bump();
            }
            let digits = &self.source[digits_start..self.position];
            if self.bump() != Some('}') || digits.is_empty() {
                return Err(self.error(start, "invalid unicode escape"));
            }
            let value = u32::from_str_radix(digits, 16)
                .ok()
                .filter(|value| *value <= 0x10ffff)
                .ok_or_else(|| self.error(start, "unicode escape is out of range"))?;
            return Ok(value);
        }
        let digits = self
            .source
            .get(self.position..self.position + 4)
            .filter(|digits| {
                digits
                    .chars()
                    .all(|character| character.is_ascii_hexdigit())
            })
            .ok_or_else(|| self.error(start, "invalid unicode escape"))?;
        self.position += 4;
        Ok(u32::from_str_radix(digits, 16).expect("four hex digits"))
    }

    fn scan_digits(&mut self, radix: u32, start: usize) -> Result<String, ScriptError> {
        let mut digits = String::new();
        let mut previous_separator = true;
        while let Some(character) = self.peek_char() {
            if character == '_' {
                if previous_separator {
                    return Err(self.error(start, "invalid numeric separator"));
                }
                previous_separator = true;
                self.bump();
                continue;
            }
            if !character.is_digit(radix) {
                break;
            }
            digits.push(character);
            previous_separator = false;
            self.bump();
        }
        if previous_separator && !digits.is_empty() {
            return Err(self.error(start, "invalid numeric separator"));
        }
        Ok(digits)
    }

    fn scan_number(&mut self) -> Result<TokenKind, ScriptError> {
        let start = self.position;
        let prefix = self.source[self.position..]
            .get(..2)
            .map(str::to_ascii_lowercase);
        let radix = match prefix.as_deref() {
            Some("0x") => Some(16),
            Some("0o") => Some(8),
            Some("0b") => Some(2),
            _ => None,
        };
        let mut integer_digits: Option<(String, u32)> = None;
        let value = if let Some(radix) = radix {
            self.position += 2;
            let digits = self.scan_digits(radix, start)?;
            if digits.is_empty() {
                return Err(self.error(start, "missing digits after a radix prefix"));
            }
            integer_digits = Some((digits.clone(), radix));
            digits.chars().fold(0.0_f64, |value, digit| {
                value * f64::from(radix) + f64::from(digit.to_digit(radix).expect("digit"))
            })
        } else if self.peek_char() == Some('0')
            && self
                .peek_char_at(1)
                .is_some_and(|next| next.is_ascii_digit())
        {
            // legacy octal, or decimal when an 8 or 9 appears
            self.bump();
            let digits = self.scan_digits(10, start)?;
            if digits.chars().all(|digit| digit < '8') {
                digits.chars().fold(0.0_f64, |value, digit| {
                    value * 8.0 + f64::from(digit as u8 - b'0')
                })
            } else {
                digits.parse::<f64>().unwrap_or(f64::NAN)
            }
        } else {
            let mut text = self.scan_digits(10, start)?;
            if !matches!(self.peek_char(), Some('.' | 'e' | 'E')) {
                integer_digits = Some((text.clone(), 10));
            }
            if self.peek_char() == Some('.') {
                self.bump();
                text.push('.');
                text.push_str(&self.scan_digits(10, start)?);
            }
            if matches!(self.peek_char(), Some('e' | 'E')) {
                let sign = self.peek_char_at(1);
                let has_sign = matches!(sign, Some('+' | '-'));
                let digit_at = if has_sign { 2 } else { 1 };
                if self
                    .peek_char_at(digit_at)
                    .is_some_and(|character| character.is_ascii_digit())
                {
                    self.bump();
                    text.push('e');
                    if has_sign {
                        text.push(self.bump().expect("sign"));
                    }
                    text.push_str(&self.scan_digits(10, start)?);
                }
            }
            if text.starts_with('.') {
                text.insert(0, '0');
            }
            text.parse::<f64>()
                .map_err(|_| self.error(start, "invalid number"))?
        };
        if self.peek_char() == Some('n') {
            let Some((digits, radix)) = integer_digits else {
                return Err(self.error(start, "invalid BigInt literal"));
            };
            self.bump();
            if self.peek_char().is_some_and(is_identifier_start) {
                return Err(self.error(start, "identifier starts right after a number"));
            }
            let value = super::bigint::BigInt::parse_radix(&digits, radix)
                .ok_or_else(|| self.error(start, "invalid BigInt literal"))?;
            return Ok(TokenKind::BigInt(value.to_string_radix(10).into()));
        }
        if self.peek_char().is_some_and(is_identifier_start) {
            return Err(self.error(start, "identifier starts right after a number"));
        }
        Ok(TokenKind::Number(value))
    }

    // the escape after a backslash; None for a line continuation, Err for a bad escape
    fn scan_escape(&mut self, start: usize, template: bool) -> Result<Option<char>, ScriptError> {
        let Some(character) = self.bump() else {
            return Err(self.error(start, "unterminated escape"));
        };
        Ok(Some(match character {
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            'b' => '\u{8}',
            'f' => '\u{c}',
            'v' => '\u{b}',
            '0' if !self.peek_char().is_some_and(|next| next.is_ascii_digit()) => '\0',
            '1'..='7' | '0' => {
                if template {
                    return Err(self.error(start, "octal escapes are not allowed in templates"));
                }
                let mut value = character.to_digit(8).expect("octal digit");
                let limit = if character <= '3' { 2 } else { 1 };
                for _ in 0..limit {
                    match self.peek_char().and_then(|next| next.to_digit(8)) {
                        Some(digit) => {
                            value = value * 8 + digit;
                            self.bump();
                        }
                        None => break,
                    }
                }
                char::from_u32(value).unwrap_or('\u{fffd}')
            }
            '8' | '9' if template => {
                return Err(self.error(start, "invalid escape in template"));
            }
            'x' => {
                let digits = self
                    .source
                    .get(self.position..self.position + 2)
                    .filter(|digits| {
                        digits
                            .chars()
                            .all(|character| character.is_ascii_hexdigit())
                    })
                    .ok_or_else(|| self.error(start, "invalid hexadecimal escape"))?;
                self.position += 2;
                char::from_u32(u32::from_str_radix(digits, 16).expect("hex")).expect("byte")
            }
            'u' => self.scan_unicode_escape(start)?,
            '\r' => {
                if self.peek_char() == Some('\n') {
                    self.bump();
                }
                return Ok(None);
            }
            '\n' | '\u{2028}' | '\u{2029}' => return Ok(None),
            other => other,
        }))
    }

    fn scan_string(&mut self, quote: char) -> Result<String, ScriptError> {
        let start = self.position;
        self.bump();
        let mut value = String::new();
        loop {
            let Some(character) = self.bump() else {
                return Err(self.error(start, "unterminated string"));
            };
            if character == quote {
                return Ok(value);
            }
            match character {
                '\\' => {
                    if let Some(escaped) = self.scan_escape(start, false)? {
                        value.push(escaped);
                    }
                }
                '\n' | '\r' => return Err(self.error(start, "unterminated string")),
                other => value.push(other),
            }
        }
    }

    // reads template text up to "${" or the closing backtick
    fn scan_template_part(&mut self, start: usize) -> Result<TokenKind, ScriptError> {
        let mut cooked = Some(String::new());
        let mut raw = String::new();
        loop {
            let Some(character) = self.bump() else {
                return Err(self.error(start, "unterminated template literal"));
            };
            match character {
                '`' => {
                    return Ok(TokenKind::Template {
                        cooked,
                        raw,
                        tail: true,
                    });
                }
                '$' if self.peek_char() == Some('{') => {
                    self.bump();
                    return Ok(TokenKind::Template {
                        cooked,
                        raw,
                        tail: false,
                    });
                }
                '\\' => {
                    let escape_start = self.position;
                    match self.scan_escape(start, true) {
                        Ok(escaped) => {
                            if let (Some(text), Some(escaped)) = (cooked.as_mut(), escaped) {
                                text.push(escaped);
                            }
                        }
                        // tagged templates keep going with an undefined cooked string
                        Err(_) => cooked = None,
                    }
                    raw.push('\\');
                    raw.push_str(&self.source[escape_start..self.position].replace("\r\n", "\n"));
                }
                '\r' => {
                    if self.peek_char() == Some('\n') {
                        self.bump();
                    }
                    if let Some(text) = cooked.as_mut() {
                        text.push('\n');
                    }
                    raw.push('\n');
                }
                other => {
                    if let Some(text) = cooked.as_mut() {
                        text.push(other);
                    }
                    raw.push(other);
                }
            }
        }
    }

    fn scan_punctuator(&mut self, start: usize) -> Result<TokenKind, ScriptError> {
        let rest = &self.source[self.position..];
        for punctuator in PUNCTUATORS {
            if rest.starts_with(punctuator) {
                // "?." followed by a digit is a conditional with a decimal number
                if punctuator == "?."
                    && rest[2..]
                        .chars()
                        .next()
                        .is_some_and(|next| next.is_ascii_digit())
                {
                    continue;
                }
                self.position += punctuator.len();
                return Ok(TokenKind::Punct(punctuator));
            }
        }
        let character = rest.chars().next().expect("not at end");
        Err(self.error(start, format!("unexpected character {character:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<TokenKind> {
        let mut lexer = Lexer::new(source);
        let mut output = Vec::new();
        loop {
            let token = lexer.next_token().expect("source should scan");
            if token.kind == TokenKind::Eof {
                return output;
            }
            output.push(token.kind);
        }
    }

    #[test]
    fn numbers_cover_every_literal_form() {
        assert_eq!(
            kinds("0x1F 0o17 0b101 1_000 .5 2e3 1.5E-2 017 019"),
            [31.0, 15.0, 5.0, 1000.0, 0.5, 2000.0, 0.015, 15.0, 19.0]
                .map(TokenKind::Number)
                .to_vec()
        );
        assert_eq!(
            kinds("10n 0xffn 0b11n"),
            ["10", "255", "3"]
                .map(|digits| TokenKind::BigInt(digits.into()))
                .to_vec()
        );
        assert!(Lexer::new("1.5n").next_token().is_err());
        assert!(Lexer::new("1__0").next_token().is_err());
        assert!(Lexer::new("3in").next_token().is_err());
    }

    #[test]
    fn strings_decode_every_escape() {
        assert_eq!(
            kinds(
                r#"'a\x41B\u{1F600}😀\n\0' "q\
r""#
            ),
            vec![
                TokenKind::String("aAB\u{1F600}\u{1F600}\n\0".to_string()),
                TokenKind::String("qr".to_string()),
            ]
        );
    }

    #[test]
    fn punctuators_take_the_longest_match_and_track_newlines() {
        assert_eq!(
            kinds("a >>>= b ?? c?.d ?.5"),
            vec![
                TokenKind::Identifier("a".to_string()),
                TokenKind::Punct(">>>="),
                TokenKind::Identifier("b".to_string()),
                TokenKind::Punct("??"),
                TokenKind::Identifier("c".to_string()),
                TokenKind::Punct("?."),
                TokenKind::Identifier("d".to_string()),
                TokenKind::Punct("?"),
                TokenKind::Number(0.5),
            ]
        );
        let mut lexer = Lexer::new("a /* x\n */ b");
        assert!(!lexer.next_token().unwrap().newline_before);
        assert!(lexer.next_token().unwrap().newline_before);
    }

    #[test]
    fn regex_and_template_rescans_follow_the_parser() {
        let mut lexer = Lexer::new("/[/]\\//gi");
        let slash = lexer.next_token().unwrap();
        assert_eq!(slash.kind, TokenKind::Punct("/"));
        let regex = lexer.rescan_regex(slash.start).unwrap();
        assert_eq!(
            regex.kind,
            TokenKind::Regex {
                pattern: "[/]\\/".to_string(),
                flags: "gi".to_string()
            }
        );
        let mut lexer = Lexer::new("`a${b}c\\u{41}`");
        assert_eq!(
            lexer.next_token().unwrap().kind,
            TokenKind::Template {
                cooked: Some("a".to_string()),
                raw: "a".to_string(),
                tail: false
            }
        );
        lexer.next_token().unwrap();
        let close = lexer.next_token().unwrap();
        assert_eq!(close.kind, TokenKind::Punct("}"));
        assert_eq!(
            lexer
                .rescan_template_continuation(close.start)
                .unwrap()
                .kind,
            TokenKind::Template {
                cooked: Some("cA".to_string()),
                raw: "c\\u{41}".to_string(),
                tail: true
            }
        );
    }

    #[test]
    fn identifiers_take_unicode_escapes_and_private_names() {
        assert_eq!(
            kinds("caf\u{e9} \\u0061b #secret $_"),
            vec![
                TokenKind::Identifier("café".to_string()),
                TokenKind::EscapedIdentifier("ab".to_string()),
                TokenKind::PrivateName("secret".to_string()),
                TokenKind::Identifier("$_".to_string()),
            ]
        );
    }
}
