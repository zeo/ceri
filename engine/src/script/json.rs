// JSON.parse per ECMA-404: a strict recursive descent over the text, building plain objects
// in source order and arrays

use super::Value;
use super::object::{JsObject, new_object};
use std::cell::RefCell;
use std::rc::Rc;

const MAX_DEPTH: usize = 512;

struct JsonParser<'a> {
    bytes: &'a [u8],
    source: &'a str,
    position: usize,
}

pub(super) fn parse(source: &str) -> Result<Value, String> {
    let mut parser = JsonParser {
        bytes: source.as_bytes(),
        source,
        position: 0,
    };
    parser.skip_whitespace();
    let value = parser.value(0)?;
    parser.skip_whitespace();
    if parser.position < parser.bytes.len() {
        return Err(parser.unexpected());
    }
    Ok(value)
}

impl JsonParser<'_> {
    fn skip_whitespace(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.bytes.get(self.position) {
            self.position += 1;
        }
    }

    fn unexpected(&self) -> String {
        match self.source[self.position..].chars().next() {
            Some(character) => format!(
                "Unexpected token '{character}' in JSON at position {}",
                self.position
            ),
            None => "Unexpected end of JSON input".to_string(),
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), String> {
        if self.bytes.get(self.position) == Some(&byte) {
            self.position += 1;
            Ok(())
        } else {
            Err(self.unexpected())
        }
    }

    fn literal(&mut self, word: &str, value: Value) -> Result<Value, String> {
        if self.source[self.position..].starts_with(word) {
            self.position += word.len();
            Ok(value)
        } else {
            Err(self.unexpected())
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > MAX_DEPTH {
            return Err("JSON nesting exceeds the depth limit".to_string());
        }
        match self.bytes.get(self.position) {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(Value::String),
            Some(b't') => self.literal("true", Value::Bool(true)),
            Some(b'f') => self.literal("false", Value::Bool(false)),
            Some(b'n') => self.literal("null", Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(self.unexpected()),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, String> {
        self.position += 1;
        let mut object = JsObject::new();
        self.skip_whitespace();
        if self.bytes.get(self.position) == Some(&b'}') {
            self.position += 1;
            return Ok(Value::Object(new_object(object)));
        }
        loop {
            self.skip_whitespace();
            if self.bytes.get(self.position) != Some(&b'"') {
                return Err(self.unexpected());
            }
            let key = self.string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_whitespace();
            let value = self.value(depth + 1)?;
            object.insert(key, value);
            self.skip_whitespace();
            match self.bytes.get(self.position) {
                Some(b',') => self.position += 1,
                Some(b'}') => {
                    self.position += 1;
                    return Ok(Value::Object(new_object(object)));
                }
                _ => return Err(self.unexpected()),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, String> {
        self.position += 1;
        let mut values = Vec::new();
        self.skip_whitespace();
        if self.bytes.get(self.position) == Some(&b']') {
            self.position += 1;
            return Ok(Value::Array(Rc::new(RefCell::new(values))));
        }
        loop {
            self.skip_whitespace();
            values.push(self.value(depth + 1)?);
            self.skip_whitespace();
            match self.bytes.get(self.position) {
                Some(b',') => self.position += 1,
                Some(b']') => {
                    self.position += 1;
                    return Ok(Value::Array(Rc::new(RefCell::new(values))));
                }
                _ => return Err(self.unexpected()),
            }
        }
    }

    fn hex4(&mut self) -> Result<u16, String> {
        let digits = self
            .source
            .get(self.position..self.position + 4)
            .ok_or_else(|| "Bad Unicode escape in JSON".to_string())?;
        let unit = u16::from_str_radix(digits, 16)
            .map_err(|_| "Bad Unicode escape in JSON".to_string())?;
        if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("Bad Unicode escape in JSON".to_string());
        }
        self.position += 4;
        Ok(unit)
    }

    fn string(&mut self) -> Result<String, String> {
        self.position += 1;
        let mut text = String::new();
        loop {
            let start = self.position;
            while let Some(&byte) = self.bytes.get(self.position) {
                if byte == b'"' || byte == b'\\' || byte < 0x20 {
                    break;
                }
                self.position += 1;
            }
            text.push_str(&self.source[start..self.position]);
            match self.bytes.get(self.position) {
                Some(b'"') => {
                    self.position += 1;
                    return Ok(text);
                }
                Some(b'\\') => {
                    self.position += 1;
                    let escape = self.bytes.get(self.position).copied();
                    self.position += 1;
                    match escape {
                        Some(b'"') => text.push('"'),
                        Some(b'\\') => text.push('\\'),
                        Some(b'/') => text.push('/'),
                        Some(b'b') => text.push('\u{8}'),
                        Some(b'f') => text.push('\u{c}'),
                        Some(b'n') => text.push('\n'),
                        Some(b'r') => text.push('\r'),
                        Some(b't') => text.push('\t'),
                        Some(b'u') => {
                            let unit = self.hex4()?;
                            let mut units = vec![unit];
                            if (0xd800..0xdc00).contains(&unit)
                                && self.source[self.position..].starts_with("\\u")
                            {
                                let saved = self.position;
                                self.position += 2;
                                let low = self.hex4()?;
                                if (0xdc00..0xe000).contains(&low) {
                                    units.push(low);
                                } else {
                                    self.position = saved;
                                }
                            }
                            text.push_str(&String::from_utf16_lossy(&units));
                        }
                        _ => {
                            self.position -= 1;
                            return Err("Bad escaped character in JSON".to_string());
                        }
                    }
                }
                Some(_) => {
                    return Err("Bad control character in string literal in JSON".to_string());
                }
                None => return Err("Unterminated string in JSON".to_string()),
            }
        }
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.position;
        if self.bytes.get(self.position) == Some(&b'-') {
            self.position += 1;
        }
        match self.bytes.get(self.position) {
            Some(b'0') => self.position += 1,
            Some(b'1'..=b'9') => {
                while let Some(b'0'..=b'9') = self.bytes.get(self.position) {
                    self.position += 1;
                }
            }
            _ => return Err(self.unexpected()),
        }
        if self.bytes.get(self.position) == Some(&b'.') {
            self.position += 1;
            if !matches!(self.bytes.get(self.position), Some(b'0'..=b'9')) {
                return Err(self.unexpected());
            }
            while let Some(b'0'..=b'9') = self.bytes.get(self.position) {
                self.position += 1;
            }
        }
        if let Some(b'e' | b'E') = self.bytes.get(self.position) {
            self.position += 1;
            if let Some(b'+' | b'-') = self.bytes.get(self.position) {
                self.position += 1;
            }
            if !matches!(self.bytes.get(self.position), Some(b'0'..=b'9')) {
                return Err(self.unexpected());
            }
            while let Some(b'0'..=b'9') = self.bytes.get(self.position) {
                self.position += 1;
            }
        }
        self.source[start..self.position]
            .parse::<f64>()
            .map(Value::Number)
            .map_err(|_| self.unexpected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_values_in_source_order() {
        let value =
            parse(r#" {"b": [1, -2.5e2, true, null], "a": "x\u00e9\ud83d\ude00\n"} "#).unwrap();
        let Value::Object(object) = value else {
            panic!("expected an object");
        };
        let object = object.borrow();
        assert_eq!(object.keys().collect::<Vec<_>>(), ["b", "a"]);
        assert!(
            matches!(object.get("a"), Some(Value::String(text)) if text == "x\u{e9}\u{1f600}\n")
        );
        let Some(Value::Array(array)) = object.get("b") else {
            panic!("expected an array");
        };
        assert!(matches!(array.borrow()[1], Value::Number(number) if number == -250.0));
    }

    #[test]
    fn rejects_what_json_does_not_allow() {
        for text in [
            "",
            "01",
            "1.",
            "'a'",
            "[1,]",
            "{\"a\":1,}",
            "tru",
            "\"\\x\"",
            "[1] 2",
            "NaN",
        ] {
            assert!(parse(text).is_err(), "{text} should not parse");
        }
    }
}
