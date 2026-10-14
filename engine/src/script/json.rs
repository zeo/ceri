// JSON.parse per ECMA-404, a strict recursive descent that builds plain objects in source
// order, plus the reviver walk and JSON.stringify with toJSON, replacers and indentation

use super::object::{JsObject, ObjectClass, is_symbol_key, new_object};
use super::operations::{is_callable, is_primitive};
use super::*;

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
            Some(b'"') => self.string().map(|text| Value::String(text.into())),
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

// JSON.stringify's string quoting
pub(super) fn quote(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for character in text.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\u{8}' => quoted.push_str("\\b"),
            '\u{c}' => quoted.push_str("\\f"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            character if (character as u32) < 0x20 => {
                quoted.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}

struct Stringifier {
    replacer: Option<Value>,
    allowed: Option<Vec<String>>,
    gap: String,
    indent: String,
    stack: Vec<usize>,
}

fn identity(value: &Value) -> Option<usize> {
    match value {
        Value::Object(object) => Some(Rc::as_ptr(object) as *const u8 as usize),
        Value::Array(array) => Some(Rc::as_ptr(array) as *const u8 as usize),
        _ => None,
    }
}

impl ScriptRuntime {
    pub(super) fn json_parse(&mut self, text: Value, reviver: Value) -> Result<Value, ScriptError> {
        let text = self.coerce_string(text)?;
        let value = parse(&text).map_err(|message| self.throw_error("SyntaxError", message))?;
        if !is_callable(&reviver) {
            return Ok(value);
        }
        let root = self.new_plain_object();
        root.borrow_mut().insert(String::new(), value);
        self.internalize(Value::Object(root), String::new(), &reviver)
    }

    fn internalize(
        &mut self,
        holder: Value,
        key: String,
        reviver: &Value,
    ) -> Result<Value, ScriptError> {
        let value = self.get_property(&holder, &key)?;
        match &value {
            Value::Array(array) => {
                let length = array.borrow().len();
                for index in 0..length {
                    let element = self.internalize(value.clone(), index.to_string(), reviver)?;
                    if let Some(slot) = array.borrow_mut().get_mut(index) {
                        *slot = element;
                    }
                }
            }
            Value::Object(_) => {
                let keys: Vec<String> = self
                    .own_enumerable_entries(&value)?
                    .into_iter()
                    .map(|(key, _)| key)
                    .filter(|key| !is_symbol_key(key))
                    .collect();
                for name in keys {
                    let element = self.internalize(value.clone(), name.clone(), reviver)?;
                    if matches!(element, Value::Undefined) {
                        self.delete_property(&value, &name, false)?;
                    } else if let Value::Object(object) = &value {
                        object
                            .borrow_mut()
                            .define(name, object::Property::data(element));
                    }
                }
            }
            _ => {}
        }
        self.call_function(
            reviver.clone(),
            holder,
            vec![Value::String(key.into()), value],
        )
    }

    // JSON.stringify; None is the undefined result for values JSON cannot represent
    pub(super) fn json_stringify(
        &mut self,
        value: Value,
        replacer: Value,
        space: Value,
    ) -> Result<Option<String>, ScriptError> {
        let mut stringifier = Stringifier {
            replacer: None,
            allowed: None,
            gap: String::new(),
            indent: String::new(),
            stack: Vec::new(),
        };
        if is_callable(&replacer) {
            stringifier.replacer = Some(replacer);
        } else if let Value::Array(list) = &replacer {
            let mut allowed = Vec::new();
            let items = list.borrow().clone();
            for item in items {
                let wrapped = matches!(&item, Value::Object(object) if matches!(object.borrow().class, ObjectClass::String(_) | ObjectClass::Number(_)));
                let key = match &item {
                    Value::String(text) => Some(text.to_string()),
                    Value::Number(number) => Some(number_to_string(*number)),
                    _ if wrapped => Some(self.coerce_string(item.clone())?),
                    _ => None,
                };
                if let Some(key) = key
                    && !allowed.contains(&key)
                {
                    allowed.push(key);
                }
            }
            stringifier.allowed = Some(allowed);
        }
        let space = match &space {
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::Number(number) => Value::Number(*number),
                ObjectClass::String(text) => Value::String(text.as_str().into()),
                _ => space.clone(),
            },
            _ => space,
        };
        stringifier.gap = match space {
            Value::Number(number) => " ".repeat(number.clamp(0.0, 10.0) as usize),
            Value::String(text) => text.chars().take(10).collect(),
            _ => String::new(),
        };
        let wrapper = self.new_plain_object();
        wrapper.borrow_mut().insert(String::new(), value);
        self.serialize_property(&mut stringifier, String::new(), &Value::Object(wrapper))
    }

    fn serialize_property(
        &mut self,
        state: &mut Stringifier,
        key: String,
        holder: &Value,
    ) -> Result<Option<String>, ScriptError> {
        let mut value = match holder {
            Value::Array(array) => key
                .parse::<usize>()
                .ok()
                .and_then(|index| array.borrow().get(index).cloned())
                .unwrap_or(Value::Undefined),
            _ => self.get_property(holder, &key)?,
        };
        if !is_primitive(&value) || matches!(value, Value::BigInt(_)) {
            let to_json = self.get_property(&value, "toJSON")?;
            if is_callable(&to_json) {
                value =
                    self.call_function(to_json, value, vec![Value::String(key.as_str().into())])?;
            }
        }
        if let Some(replacer) = state.replacer.clone() {
            value = self.call_function(
                replacer,
                holder.clone(),
                vec![Value::String(key.as_str().into()), value],
            )?;
        }
        let unwrapped = match &value {
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::Number(_) => Some(1),
                ObjectClass::String(_) => Some(2),
                ObjectClass::Boolean(_) => Some(3),
                _ => None,
            },
            _ => None,
        };
        value = match unwrapped {
            Some(1) => Value::Number(self.coerce_number(value)?),
            Some(2) => Value::String(self.coerce_string(value)?.into()),
            Some(_) => match &value {
                Value::Object(object) => match &object.borrow().class {
                    ObjectClass::Boolean(flag) => Value::Bool(*flag),
                    _ => Value::Undefined,
                },
                _ => Value::Undefined,
            },
            None => value,
        };
        Ok(match value {
            Value::Null => Some("null".to_string()),
            Value::Bool(flag) => Some(flag.to_string()),
            Value::String(text) => Some(quote(&text)),
            Value::Number(number) if number.is_finite() => Some(number_to_string(number)),
            Value::Number(_) => Some("null".to_string()),
            Value::Undefined | Value::Symbol(_) | Value::Uninitialized => None,
            Value::BigInt(_) => {
                return Err(self.type_error("Do not know how to serialize a BigInt"));
            }
            ref callable if is_callable(callable) => None,
            Value::Array(_) => Some(self.serialize_array(state, &value)?),
            _ => Some(self.serialize_object(state, &value)?),
        })
    }

    fn enter(&mut self, state: &mut Stringifier, value: &Value) -> Result<(), ScriptError> {
        if let Some(address) = identity(value) {
            if state.stack.contains(&address) {
                return Err(self.type_error("Converting circular structure to JSON"));
            }
            state.stack.push(address);
        }
        if state.stack.len() > 1000 {
            return Err(self.range_error("JSON nesting exceeds the depth limit"));
        }
        Ok(())
    }

    fn serialize_object(
        &mut self,
        state: &mut Stringifier,
        value: &Value,
    ) -> Result<String, ScriptError> {
        self.enter(state, value)?;
        let stepback = state.indent.clone();
        state.indent = format!("{}{}", state.indent, state.gap);
        let keys: Vec<String> = match &state.allowed {
            Some(allowed) => allowed.clone(),
            None => self.own_enumerable_keys(value)?,
        };
        let mut parts = Vec::new();
        for key in keys {
            if let Some(text) = self.serialize_property(state, key.clone(), value)? {
                let separator = if state.gap.is_empty() { ":" } else { ": " };
                parts.push(format!("{}{separator}{text}", quote(&key)));
            }
        }
        let text = if parts.is_empty() {
            "{}".to_string()
        } else if state.gap.is_empty() {
            format!("{{{}}}", parts.join(","))
        } else {
            let separator = format!(",\n{}", state.indent);
            format!(
                "{{\n{}{}\n{stepback}}}",
                state.indent,
                parts.join(&separator)
            )
        };
        if identity(value).is_some() {
            state.stack.pop();
        }
        state.indent = stepback;
        Ok(text)
    }

    fn serialize_array(
        &mut self,
        state: &mut Stringifier,
        value: &Value,
    ) -> Result<String, ScriptError> {
        self.enter(state, value)?;
        let stepback = state.indent.clone();
        state.indent = format!("{}{}", state.indent, state.gap);
        let length = match value {
            Value::Array(array) => array.borrow().len(),
            _ => 0,
        };
        let mut parts = Vec::with_capacity(length);
        for index in 0..length {
            let text = self.serialize_property(state, index.to_string(), value)?;
            parts.push(text.unwrap_or_else(|| "null".to_string()));
        }
        let text = if parts.is_empty() {
            "[]".to_string()
        } else if state.gap.is_empty() {
            format!("[{}]", parts.join(","))
        } else {
            let separator = format!(",\n{}", state.indent);
            format!("[\n{}{}\n{stepback}]", state.indent, parts.join(&separator))
        };
        state.stack.pop();
        state.indent = stepback;
        Ok(text)
    }

    // JSON text for host APIs such as Response.json() and history state snapshots
    pub(super) fn json_text(&mut self, value: &Value) -> Result<String, ScriptError> {
        match self.json_stringify(value.clone(), Value::Undefined, Value::Undefined)? {
            Some(text) => Ok(text),
            None => Err(self.type_error("Value is not JSON serializable")),
        }
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
            matches!(object.get("a"), Some(Value::String(text)) if &**text == "x\u{e9}\u{1f600}\n")
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
